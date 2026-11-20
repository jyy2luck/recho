use crate::storage::{Recording, now};
use base64::Engine as _;
use flate2::{Decompress, FlushDecompress};
use serde::{Deserialize, Serialize};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// Stop inspecting compression bombs without changing the bytes being relayed.
const MESSAGE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Compression {
    client_no_context_takeover: bool,
    server_no_context_takeover: bool,
}
impl Compression {
    pub fn parse(extensions: &str) -> Option<Self> {
        extensions.split(',').find_map(|extension| {
            let mut parts = extension.split(';').map(str::trim);
            if !parts.next()?.eq_ignore_ascii_case("permessage-deflate") {
                return None;
            }
            let mut result = Self {
                client_no_context_takeover: false,
                server_no_context_takeover: false,
            };
            for parameter in parts {
                if parameter.eq_ignore_ascii_case("client_no_context_takeover") {
                    result.client_no_context_takeover = true;
                }
                if parameter.eq_ignore_ascii_case("server_no_context_takeover") {
                    result.server_no_context_takeover = true;
                }
            }
            Some(result)
        })
    }
    fn reset_each_message(self, direction: &str) -> bool {
        if direction == "request" {
            self.client_no_context_takeover
        } else {
            self.server_no_context_takeover
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub id: String,
    pub direction: String,
    pub opcode: u8,
    pub r#final: bool,
    pub compressed: bool,
    pub length: usize,
    pub payload_base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub truncated: bool,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_opcode: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_final: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_error: Option<String>,
}
struct Payload {
    opcode: u8,
    fin: bool,
    compressed: bool,
    length: usize,
    read: usize,
    mask: Option<[u8; 4]>,
    bytes: Vec<u8>,
}
struct Message {
    id: String,
    opcode: u8,
    compressed: bool,
    bytes: Vec<u8>,
    length: usize,
    encoded_length: usize,
    error: Option<String>,
}
impl Message {
    fn append(&mut self, bytes: &[u8], limit: usize) {
        self.length = self.length.saturating_add(bytes.len());
        let count = bytes.len().min(limit.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..count]);
    }
}
pub struct Decoder {
    header: Vec<u8>,
    payload: Option<Payload>,
    limit: usize,
    direction: String,
    compression: Option<Compression>,
    inflater: Decompress,
    invalid_context: bool,
    message: Option<Message>,
}
impl Decoder {
    pub fn new(limit: usize, direction: &str, compression: Option<Compression>) -> Self {
        Self {
            header: Vec::new(),
            payload: None,
            limit,
            direction: direction.into(),
            compression,
            inflater: Decompress::new(false),
            invalid_context: false,
            message: None,
        }
    }
    fn start_message(&mut self, opcode: u8, compressed: bool) {
        if self.message.as_ref().is_some_and(|m| m.compressed) {
            self.invalid_context = true;
        }
        if compressed
            && self
                .compression
                .is_some_and(|c| c.reset_each_message(&self.direction))
        {
            self.inflater = Decompress::new(false);
            self.invalid_context = false;
        }
        let error = if compressed && self.compression.is_none() {
            Some("Compressed frame without negotiated permessage-deflate".into())
        } else if compressed && self.invalid_context {
            Some("Compression context unavailable after an invalid or oversized message".into())
        } else {
            None
        };
        self.message = Some(Message {
            id: uuid::Uuid::new_v4().to_string(),
            opcode,
            compressed,
            bytes: Vec::new(),
            length: 0,
            encoded_length: 0,
            error,
        });
    }
    fn inspect_message(&mut self, mut bytes: &[u8]) {
        let Some(message) = &mut self.message else {
            return;
        };
        if message.error.is_some() {
            return;
        }
        if !message.compressed {
            message.append(bytes, self.limit);
            return;
        }
        message.encoded_length = message.encoded_length.saturating_add(bytes.len());
        if message.encoded_length > MESSAGE_LIMIT {
            message.error = Some("Compressed message exceeds the 16 MiB inspection limit".into());
            self.invalid_context = true;
            return;
        }
        loop {
            let mut output = [0u8; 8192];
            let before_in = self.inflater.total_in();
            let before_out = self.inflater.total_out();
            let result = self
                .inflater
                .decompress(bytes, &mut output, FlushDecompress::Sync);
            let consumed = (self.inflater.total_in() - before_in) as usize;
            let produced = (self.inflater.total_out() - before_out) as usize;
            message.append(&output[..produced], self.limit);
            if result.is_err() || matches!(result, Ok(flate2::Status::StreamEnd)) {
                message.error = Some("Invalid permessage-deflate stream".into());
                self.invalid_context = true;
                break;
            }
            if message.length > MESSAGE_LIMIT {
                message.error = Some("Decoded message exceeds the 16 MiB inspection limit".into());
                self.invalid_context = true;
                break;
            }
            bytes = &bytes[consumed..];
            if (consumed == 0 && produced == 0) || (bytes.is_empty() && produced < output.len()) {
                break;
            }
        }
    }
    fn finish_frame(&mut self, payload: Payload) -> Frame {
        let text = if !payload.compressed
            && !self
                .message
                .as_ref()
                .is_some_and(|message| message.compressed)
            && [0, 1].contains(&payload.opcode)
        {
            String::from_utf8(payload.bytes.clone()).ok()
        } else {
            None
        };
        let mut frame = Frame {
            id: uuid::Uuid::new_v4().to_string(),
            direction: self.direction.clone(),
            opcode: payload.opcode,
            r#final: payload.fin,
            compressed: payload.compressed,
            length: payload.length,
            payload_base64: base64::engine::general_purpose::STANDARD.encode(&payload.bytes),
            text,
            truncated: payload.bytes.len() < payload.length,
            timestamp: now(),
            message_id: None,
            message_opcode: None,
            message_final: None,
            message_length: None,
            message_base64: None,
            message_text: None,
            message_truncated: None,
            decode_error: None,
        };
        if ![0, 1, 2].contains(&payload.opcode) {
            return frame;
        }
        if payload.fin && self.message.as_ref().is_some_and(|m| m.compressed) {
            self.inspect_message(&[0, 0, 255, 255]);
        }
        if let Some(message) = &self.message {
            frame.message_id = Some(message.id.clone());
            frame.message_opcode = Some(message.opcode);
            frame.message_final = Some(payload.fin);
            frame.decode_error = message.error.clone();
        } else {
            frame.decode_error = Some("Continuation frame without a starting message".into());
        }
        if payload.fin {
            if let Some(message) = self.message.take() {
                if message.error.is_none() {
                    frame.message_length = Some(message.length);
                }
                frame.message_truncated =
                    Some(message.length > message.bytes.len() || message.error.is_some());
                // Failed compressed streams must never look like successfully decoded text.
                if message.error.is_none() {
                    if message.opcode == 1 {
                        frame.message_text = String::from_utf8(message.bytes.clone()).ok();
                    }
                    frame.message_base64 =
                        Some(base64::engine::general_purpose::STANDARD.encode(&message.bytes));
                }
            }
        }
        frame
    }
    pub fn push(&mut self, mut chunk: &[u8]) -> Vec<Frame> {
        let mut frames = vec![];
        while !chunk.is_empty() || self.payload.as_ref().is_some_and(|p| p.read == p.length) {
            if let Some(payload) = &mut self.payload {
                let count = chunk.len().min(payload.length - payload.read).min(8192);
                let mut decoded = [0u8; 8192];
                for (offset, byte) in chunk[..count].iter().enumerate() {
                    decoded[offset] = *byte
                        ^ payload
                            .mask
                            .map(|key| key[(payload.read + offset) % 4])
                            .unwrap_or(0);
                }
                let stored = count.min(self.limit.saturating_sub(payload.bytes.len()));
                payload.bytes.extend_from_slice(&decoded[..stored]);
                payload.read += count;
                chunk = &chunk[count..];
                let data_frame = [0, 1, 2].contains(&payload.opcode);
                let finished = payload.read == payload.length;
                if data_frame {
                    self.inspect_message(&decoded[..count]);
                }
                if finished {
                    let payload = self.payload.take().unwrap();
                    frames.push(self.finish_frame(payload));
                }
                if count == 0 && self.payload.is_some() {
                    break;
                }
                continue;
            }
            if chunk.is_empty() {
                break;
            }
            self.header.push(chunk[0]);
            chunk = &chunk[1..];
            if self.header.len() < 2 {
                continue;
            }
            let masked = self.header[1] & 128 != 0;
            let code = self.header[1] & 127;
            let extra = if code == 126 {
                2
            } else if code == 127 {
                8
            } else {
                0
            };
            let needed = 2 + extra + if masked { 4 } else { 0 };
            if self.header.len() < needed {
                continue;
            }
            let length = match code {
                126 => u16::from_be_bytes([self.header[2], self.header[3]]) as usize,
                127 => usize::try_from(u64::from_be_bytes(self.header[2..10].try_into().unwrap()))
                    .unwrap_or(usize::MAX),
                _ => code as usize,
            };
            let mask = masked.then(|| self.header[2 + extra..needed].try_into().unwrap());
            let opcode = self.header[0] & 15;
            let compressed = self.header[0] & 64 != 0;
            if [1, 2].contains(&opcode) {
                self.start_message(opcode, compressed);
            }
            if opcode == 0 && compressed {
                if let Some(message) = &mut self.message {
                    message.error = Some("RSV1 must be unset on continuation frames".into());
                    self.invalid_context = true;
                }
            }
            self.payload = Some(Payload {
                opcode,
                fin: self.header[0] & 128 != 0,
                compressed,
                length,
                read: 0,
                mask,
                bytes: Vec::new(),
            });
            self.header.clear();
        }
        frames
    }
}
pub struct Observed<T> {
    inner: T,
    decoder: Decoder,
    record: Arc<Mutex<Recording>>,
    request: bool,
}
impl<T> Observed<T> {
    pub fn new(
        inner: T,
        record: Arc<Mutex<Recording>>,
        request: bool,
        limit: usize,
        compression: Option<Compression>,
    ) -> Self {
        Self {
            inner,
            record,
            request,
            decoder: Decoder::new(
                limit,
                if request { "request" } else { "response" },
                compression,
            ),
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for Observed<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        let data = &buf.filled()[before..];
        if !data.is_empty() {
            let frames = self.decoder.push(data);
            let mut record = self.record.lock().unwrap();
            if self.request {
                record.request.add(data);
            } else {
                record.response.add(data);
            }
            record.session.websocket_frames.extend(frames);
            let remove = record.session.websocket_frames.len().saturating_sub(1000);
            record.session.websocket_frames.drain(..remove);
            record.publish();
        }
        result
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for Observed<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn compressed(compressor: &mut flate2::Compress, input: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(input.len() + 256);
        compressor
            .compress_vec(input, &mut bytes, flate2::FlushCompress::Sync)
            .unwrap();
        assert!(bytes.ends_with(&[0, 0, 255, 255]));
        bytes.truncate(bytes.len() - 4);
        bytes
    }
    fn packet(first: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![first];
        if bytes.len() < 126 {
            out.push(bytes.len() as u8);
        } else {
            out.push(126);
            out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(bytes);
        out
    }
    #[test]
    fn split_masked_fragmented_binary_and_large_frames_are_bounded() {
        let mut decoder = Decoder::new(4, "request", None);
        let packet = [
            0x81,
            0x85,
            1,
            2,
            3,
            4,
            b'h' ^ 1,
            b'e' ^ 2,
            b'l' ^ 3,
            b'l' ^ 4,
            b'o' ^ 1,
        ];
        let mut frames = vec![];
        for b in packet {
            frames.extend(decoder.push(&[b]));
        }
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].text.as_deref(), Some("hell"));
        assert!(frames[0].truncated);
        assert_eq!(frames[0].length, 5);
        assert_eq!(frames[0].message_length, Some(5));
        assert_eq!(frames[0].message_truncated, Some(true));
        let frames = decoder.push(&[0x82, 0, 0x80, 1, 255]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].opcode, 2);
        assert_eq!(frames[1].payload_base64, "/w==");
        assert!(frames[1].decode_error.is_some());
    }
    #[test]
    fn fragments_reassemble_utf8_around_control_frames() {
        let mut decoder = Decoder::new(100, "response", None);
        let mut bytes = packet(1, &[0xe4, 0xbd]);
        bytes.extend(packet(0x89, b"ping"));
        bytes.extend(packet(0x80, &[0xa0]));
        let frames = decoder.push(&bytes);
        assert_eq!(frames[0].message_id, frames[2].message_id);
        assert!(frames[1].message_id.is_none());
        assert_eq!(frames[2].message_text.as_deref(), Some("你"));
        assert_eq!(frames[2].message_length, Some(3));
    }
    #[test]
    fn deflate_context_takeover_fragments_and_bounded_preview() {
        let config =
            Compression::parse("other, permessage-deflate; client_no_context_takeover").unwrap();
        let mut decoder = Decoder::new(6, "response", Some(config));
        let mut compressor = flate2::Compress::new(flate2::Compression::default(), false);
        for text in ["hello hello hello", "hello hello world"] {
            let zipped = compressed(&mut compressor, text.as_bytes());
            let middle = zipped.len() / 2;
            let mut wire = packet(0x41, &zipped[..middle]);
            wire.extend(packet(0x89, b"p"));
            wire.extend(packet(0x80, &zipped[middle..]));
            let mut frames = vec![];
            for chunk in wire.chunks(2) {
                frames.extend(decoder.push(chunk));
            }
            let final_frame = frames.last().unwrap();
            assert_eq!(final_frame.message_text.as_deref(), Some("hello "));
            assert_eq!(final_frame.message_length, Some(text.len()));
            assert_eq!(final_frame.message_truncated, Some(true));
            assert_eq!(final_frame.decode_error, None);
        }
    }
    #[test]
    fn no_context_takeover_and_invalid_stream_do_not_affect_relay() {
        let config = Compression::parse("permessage-deflate; server_no_context_takeover").unwrap();
        let mut decoder = Decoder::new(100, "response", Some(config));
        let invalid = decoder.push(&packet(0xc1, &[255, 255, 255]));
        assert!(invalid[0].decode_error.is_some());
        assert_eq!(invalid[0].payload_base64, "////");
        assert!(invalid[0].message_text.is_none());
        for _ in 0..2 {
            let zipped = compressed(
                &mut flate2::Compress::new(flate2::Compression::default(), false),
                b"new dictionary",
            );
            let frames = decoder.push(&packet(0xc1, &zipped));
            assert_eq!(frames[0].message_text.as_deref(), Some("new dictionary"));
            assert!(frames[0].decode_error.is_none());
        }
    }
    #[test]
    fn compressed_expansion_is_bounded_and_does_not_poison_uncompressed_messages() {
        let mut decoder = Decoder::new(1024, "response", Compression::parse("permessage-deflate"));
        let input = vec![b'a'; MESSAGE_LIMIT + 8192];
        let bytes = compressed(
            &mut flate2::Compress::new(flate2::Compression::default(), false),
            &input,
        );
        let frame = decoder.push(&packet(0xc1, &bytes)).remove(0);
        assert!(
            frame
                .decode_error
                .as_deref()
                .unwrap()
                .contains("inspection limit")
        );
        assert!(frame.message_text.is_none());
        assert!(frame.payload_base64.len() < 1400);
        let next = decoder.push(&packet(0x81, b"plain"));
        assert_eq!(next[0].message_text.as_deref(), Some("plain"));
    }
}
