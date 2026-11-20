use std::{collections::BTreeMap, fmt, io::Read};

use base64::Engine as _;
use hyper::{
    HeaderMap, Method,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

pub type Result<T> = std::result::Result<T, Error>;
pub type Headers = BTreeMap<String, Value>;

fn optional_text<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn serialize_binary<S: serde::Serializer>(
    value: &Option<Vec<u8>>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    value
        .as_ref()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
        .serialize(serializer)
}
fn deserialize_binary<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<u8>>, D::Error> {
    let value = String::deserialize(deserializer)?;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

impl Error {
    pub fn new(status: u16, message: impl fmt::Display) -> Self {
        Self {
            status,
            message: message.to_string(),
        }
    }
    pub fn bad(message: impl fmt::Display) -> Self {
        Self::new(400, message)
    }
    pub fn upstream(message: impl fmt::Display) -> Self {
        Self::new(502, message)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        if value.kind() == std::io::ErrorKind::ConnectionRefused {
            Self::upstream(format!("ECONNREFUSED: {value}"))
        } else {
            Self::upstream(value)
        }
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::bad(value)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::new(500, value)
    }
}

pub fn network_error(error: impl std::error::Error + 'static) -> Error {
    let mut status = 502;
    let mut message = error.to_string();
    let mut cause = Some(&error as &dyn std::error::Error);
    while let Some(item) = cause {
        if let Some(error) = item.downcast_ref::<Error>() {
            status = error.status;
        }
        if let Some(io) = item.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::TimedOut {
                status = 504;
            }
        }
        cause = item.source();
        if let Some(source) = cause {
            message.push_str(&format!(": {source}"));
        }
    }
    Error::new(status, message)
}

pub fn parse_url(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).map_err(Error::bad)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::bad(
            "Use an HTTP or HTTPS URL without embedded credentials",
        ));
    }
    url.set_fragment(None);
    Ok(url)
}

pub fn method(value: &str) -> Result<String> {
    Method::from_bytes(value.as_bytes()).map_err(|_| Error::bad("Invalid HTTP method"))?;
    Ok(value.to_ascii_uppercase())
}

pub fn normalize_headers(input: &Headers, patch: bool) -> Result<Headers> {
    let mut result = Headers::new();
    for (name, value) in input {
        HeaderName::from_bytes(name.as_bytes()).map_err(Error::bad)?;
        if !(patch && value.is_null()) {
            let values = header_values(value)?;
            for item in values {
                HeaderValue::from_str(item)
                    .map_err(|_| Error::bad(format!("Invalid character in header {name}")))?;
            }
        }
        result.insert(name.to_ascii_lowercase(), value.clone());
    }
    Ok(result)
}

pub fn header_values(value: &Value) -> Result<Vec<&str>> {
    if let Some(text) = value.as_str() {
        return Ok(vec![text]);
    }
    if let Some(list) = value.as_array() {
        return list
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::bad("Header values must be strings"))
            })
            .collect();
    }
    Err(Error::bad("Headers must contain strings or string arrays"))
}

pub fn from_headers(input: &HeaderMap) -> Headers {
    let mut result = Headers::new();
    for name in input.keys() {
        let values: Vec<Value> = input
            .get_all(name)
            .iter()
            .map(|v| Value::String(String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        result.insert(
            name.as_str().into(),
            if values.len() == 1 {
                values[0].clone()
            } else {
                Value::Array(values)
            },
        );
    }
    result
}

pub fn header_text(headers: &Headers) -> String {
    headers
        .iter()
        .flat_map(|(name, value)| {
            header_values(value)
                .unwrap_or_default()
                .into_iter()
                .map(move |item| format!("{name}: {item}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn parse_headers(text: &str) -> Result<Headers> {
    let mut result = Headers::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| Error::bad("Invalid header: expected Key: Value"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = Value::String(value.trim().into());
        match result.get_mut(&name) {
            Some(Value::Array(list)) => list.push(value),
            Some(existing) => *existing = Value::Array(vec![existing.clone(), value]),
            None => {
                result.insert(name, value);
            }
        }
    }
    normalize_headers(&result, false)
}

pub fn forward_headers(headers: &Headers) -> Result<HeaderMap> {
    let mut omitted = vec![
        "connection",
        "proxy-connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "proxy-authorization",
        "proxy-authenticate",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    if let Some(value) = headers.get("connection").and_then(Value::as_str) {
        omitted.extend(value.split(',').map(|v| v.trim().to_ascii_lowercase()));
    }
    let mut output = HeaderMap::new();
    for (name, value) in headers {
        if omitted.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        let key = HeaderName::from_bytes(name.as_bytes()).map_err(Error::bad)?;
        for item in header_values(value)? {
            output.append(
                key.clone(),
                HeaderValue::from_str(item).map_err(Error::bad)?,
            );
        }
    }
    Ok(output)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestData {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Headers,
    #[serde(
        default,
        deserialize_with = "optional_text",
        skip_serializing_if = "Option::is_none"
    )]
    pub body: Option<String>,
    #[serde(
        default,
        rename = "bodyBase64",
        serialize_with = "serialize_binary",
        deserialize_with = "deserialize_binary",
        skip_serializing_if = "Option::is_none"
    )]
    pub binary_body: Option<Vec<u8>>,
}
impl RequestData {
    pub fn validate(mut self) -> Result<Self> {
        if self.body.is_some() && self.binary_body.is_some() {
            return Err(Error::bad("Choose body or bodyBase64, not both"));
        }
        self.method = method(&self.method)?;
        self.url = parse_url(&self.url)?.to_string();
        self.headers = normalize_headers(&self.headers, false)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseData {
    pub status: u16,
    #[serde(default)]
    pub headers: Headers,
    #[serde(
        default,
        deserialize_with = "optional_text",
        skip_serializing_if = "Option::is_none"
    )]
    pub body: Option<String>,
    #[serde(
        default,
        rename = "bodyBase64",
        serialize_with = "serialize_binary",
        deserialize_with = "deserialize_binary",
        skip_serializing_if = "Option::is_none"
    )]
    pub binary_body: Option<Vec<u8>>,
}
impl ResponseData {
    pub fn validate_for_upgrade(mut self) -> Result<Self> {
        if self.status != 101 {
            return self.validate();
        }
        self.status = 200;
        let mut result = self.validate()?;
        result.status = 101;
        Ok(result)
    }
    pub fn validate(mut self) -> Result<Self> {
        if self.body.is_some() && self.binary_body.is_some() {
            return Err(Error::bad("Choose body or bodyBase64, not both"));
        }
        if !(200..=599).contains(&self.status) {
            return Err(Error::bad("status must be an integer from 200 to 599"));
        }
        self.headers = normalize_headers(&self.headers, false)?;
        Ok(self)
    }
}

pub fn decoded(bytes: &[u8], headers: &Headers, limit: usize) -> Result<Vec<u8>> {
    let encoding = headers
        .get("content-encoding")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let reader: Box<dyn Read + '_> = match encoding.as_str() {
        "gzip" => Box::new(flate2::read::GzDecoder::new(bytes)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(bytes)),
        "br" => Box::new(brotli::Decompressor::new(bytes, 4096)),
        "" | "identity" => Box::new(bytes),
        _ => {
            return Err(Error::bad(format!(
                "Body rules cannot decode content encoding {encoding}"
            )));
        }
    };
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(Error::bad)?;
    if output.len() > limit {
        return Err(Error::new(
            413,
            format!("Decoded body exceeds {limit} bytes"),
        ));
    }
    Ok(output)
}

pub fn script_body(
    bytes: &[u8],
    headers: &Headers,
    limit: usize,
) -> Result<(Option<String>, Option<Vec<u8>>)> {
    let bytes = decoded(bytes, headers, limit)?;
    let content_type = headers
        .get("content-type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let text_type = content_type.starts_with("text/")
        || ["json", "xml", "javascript", "x-www-form-urlencoded"]
            .iter()
            .any(|v| content_type.contains(v));
    let binary_type = [
        "application/octet-stream",
        "application/grpc",
        "application/protobuf",
        "application/x-protobuf",
        "image/",
        "audio/",
        "video/",
    ]
    .iter()
    .any(|v| content_type.starts_with(v));
    if binary_type
        || (!text_type
            && bytes
                .iter()
                .any(|&v| v == 0 || (v < 32 && ![9, 10, 13].contains(&v))))
    {
        return Ok((None, Some(bytes)));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok((Some(text), None)),
        Err(error) => Ok((None, Some(error.into_bytes()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_valid_binary_bodies_remain_binary_for_scripts_and_breakpoints() {
        let headers = Headers::new();
        assert_eq!(
            script_body(&[0, 1, 2, 3], &headers, 1024).unwrap(),
            (None, Some(vec![0, 1, 2, 3]))
        );
        let mut binary = Headers::new();
        binary.insert(
            "content-type".into(),
            Value::String("application/octet-stream".into()),
        );
        assert_eq!(
            script_body(b"ABCD", &binary, 1024).unwrap(),
            (None, Some(b"ABCD".to_vec()))
        );
        let mut text = Headers::new();
        text.insert(
            "content-type".into(),
            Value::String("application/json; charset=utf-8".into()),
        );
        assert_eq!(
            script_body(b"{\"ok\":true}\n", &text, 1024).unwrap(),
            (Some("{\"ok\":true}\n".into()), None)
        );
    }
}
pub fn rules_text(bytes: &[u8], headers: &Headers, limit: usize) -> Result<String> {
    String::from_utf8(decoded(bytes, headers, limit)?).map_err(|_| Error::bad("Body rules require UTF-8 text; binary bodies are supported by header rules and plain forwarding"))
}

#[derive(Default)]
pub struct Capture {
    pub length: usize,
    pub bytes: Vec<u8>,
    pub limit: usize,
    pub archive: Vec<u8>,
    archive_limit: usize,
}
impl Capture {
    pub fn with_archive(limit: usize, archive_limit: usize) -> Self {
        Self {
            limit,
            archive_limit,
            ..Default::default()
        }
    }
    pub fn reset(&mut self, bytes: &[u8]) {
        self.length = 0;
        self.bytes.clear();
        self.archive.clear();
        self.add(bytes);
    }
    pub fn add(&mut self, bytes: &[u8]) {
        let archived = bytes
            .len()
            .min(self.archive_limit.saturating_sub(self.archive.len()));
        self.archive.extend_from_slice(&bytes[..archived]);
        self.length += bytes.len();
        let remaining = self.limit.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    }
    pub fn preview(&self, headers: &Headers) -> String {
        let bytes = match decoded(&self.bytes, headers, self.limit) {
            Ok(bytes) => bytes,
            Err(_) => {
                return format!(
                    "[{} body: {} bytes; preview exceeds the limit or cannot be decoded]",
                    headers
                        .get("content-encoding")
                        .and_then(Value::as_str)
                        .unwrap_or("encoded"),
                    self.length
                );
            }
        };
        let content_type = headers
            .get("content-type")
            .and_then(Value::as_str)
            .unwrap_or("unknown content type");
        let text = content_type.starts_with("text/")
            || ["json", "xml", "javascript", "x-www-form-urlencoded"]
                .iter()
                .any(|v| content_type.contains(v));
        if !text
            && bytes
                .iter()
                .any(|&v| v == 0 || (v < 32 && ![9, 10, 13].contains(&v)))
        {
            return format!("[Binary body: {} bytes, {content_type}]", self.length);
        }
        let mut result = String::from_utf8_lossy(&bytes).into_owned();
        if self.length > self.bytes.len() {
            result.push_str(&format!(
                "\n[Preview truncated at {} bytes; {} bytes transferred]",
                self.limit, self.length
            ));
        }
        result
    }
}
