use crate::wire::Headers;
use base64::Engine as _;
use serde_json::{Value, json};
use std::io::Read;
fn varint(bytes: &[u8], offset: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*offset)?;
        *offset += 1;
        if shift == 63 && b > 1 {
            return None;
        }
        value |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Some(value);
        }
    }
    None
}
fn fields(bytes: &[u8], depth: usize) -> Option<Vec<Value>> {
    if depth > 4 {
        return None;
    }
    let mut output = vec![];
    let mut offset = 0;
    while offset < bytes.len() {
        if output.len() >= 10000 {
            return None;
        }
        let tag = varint(bytes, &mut offset)?;
        let number = tag >> 3;
        let wire = tag & 7;
        if number == 0 || number > 536870911 {
            return None;
        }
        let value = match wire {
            0 => json!(varint(bytes, &mut offset)?.to_string()),
            1 => {
                let data: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
                offset += 8;
                json!({"fixed64":u64::from_le_bytes(data).to_string(),"double":f64::from_le_bytes(data)})
            }
            2 => {
                let length = usize::try_from(varint(bytes, &mut offset)?).ok()?;
                let data = bytes.get(offset..offset.checked_add(length)?)?;
                offset += length;
                let mut value =
                    json!({"base64":base64::engine::general_purpose::STANDARD.encode(data)});
                if let Ok(text) = std::str::from_utf8(data) {
                    if !text
                        .chars()
                        .any(|c| c.is_control() && !['\n', '\r', '\t'].contains(&c))
                    {
                        value["text"] = json!(text);
                    }
                }
                if !data.is_empty() {
                    if let Some(nested) = fields(data, depth + 1) {
                        value["fields"] = json!(nested);
                    }
                }
                value
            }
            5 => {
                let data: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
                offset += 4;
                json!({"fixed32":u32::from_le_bytes(data),"float":f32::from_le_bytes(data)})
            }
            _ => return None,
        };
        output.push(json!({"number":number,"wireType":wire,"value":value}));
    }
    Some(output)
}
pub fn messages(bytes: &[u8], headers: &Headers, direction: &str, limit: usize) -> Vec<Value> {
    if !headers
        .get("content-type")
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("application/grpc"))
    {
        return vec![];
    }
    let mut offset = 0;
    let mut output = vec![];
    while bytes.len().saturating_sub(offset) >= 5 && output.len() < 1000 {
        let compressed = bytes[offset] & 1 == 1;
        let length = u32::from_be_bytes(bytes[offset + 1..offset + 5].try_into().unwrap()) as usize;
        offset += 5;
        let Some(data) = bytes.get(offset..offset.saturating_add(length)) else {
            break;
        };
        offset += length;
        let mut decoded = data.to_vec();
        let mut decode_error = None;
        if compressed {
            let encoding = headers
                .get("grpc-encoding")
                .and_then(Value::as_str)
                .unwrap_or("");
            if encoding == "gzip" {
                decoded.clear();
                match flate2::read::GzDecoder::new(data)
                    .take(limit as u64 + 1)
                    .read_to_end(&mut decoded)
                {
                    Ok(_) if decoded.len() <= limit => {}
                    _ => {
                        decoded = data.to_vec();
                        decode_error = Some("Cannot decode gzip message within body limit");
                    }
                }
            } else {
                decode_error = Some("Unsupported gRPC message compression");
            }
        }
        let mut item = json!({"direction":direction,"compressed":compressed,"length":length,"payloadBase64":base64::engine::general_purpose::STANDARD.encode(&decoded)});
        if let Some(error) = decode_error {
            item["error"] = json!(error);
        } else {
            if let Ok(text) = std::str::from_utf8(&decoded) {
                item["text"] = json!(text);
            }
            if let Some(fields) = fields(&decoded, 0) {
                item["fields"] = json!(fields);
            }
        }
        output.push(item);
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_protobuf_frames_and_partial_tail() {
        let data = [0, 0, 0, 0, 7, 8, 150, 1, 18, 2, b'h', b'i', 0, 0];
        let headers = serde_json::from_value(json!({"content-type":"application/grpc"})).unwrap();
        let messages = messages(&data, &headers, "response", 1024);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["fields"][0]["value"], "150");
        assert_eq!(messages[0]["fields"][1]["value"]["text"], "hi");
    }
}
