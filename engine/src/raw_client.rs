//! Independent HTTP/1.x message client. The editable message never passes through Hyper.
//! Header order, duplicates, request target, and body bytes survive transmission.
use crate::{
    storage::{History, Recording, Session},
    wire::{self, Error, RequestData, ResponseData, Result},
};
use base64::Engine as _;
use rustls::{ClientConfig, pki_types::ServerName};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::Instant,
};
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;

const HEADER_LIMIT: usize = 64 * 1024;
#[derive(Default)]
struct Requests {
    active: HashMap<String, CancellationToken>,
    pending_cancel: HashMap<String, Instant>,
}
static REQUESTS: OnceLock<Mutex<Requests>> = OnceLock::new();
fn requests() -> &'static Mutex<Requests> {
    REQUESTS.get_or_init(Default::default)
}
struct ActiveRequest(String);
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        requests().lock().unwrap().active.remove(&self.0);
    }
}

pub fn cancel(id: &str) -> bool {
    if id.is_empty() || id.len() > 256 {
        return false;
    }
    let mut registry = requests().lock().unwrap();
    if let Some(token) = registry.active.get(id) {
        token.cancel();
    } else {
        // A cancel control request can arrive before the send control request.
        // Keep a bounded, short-lived tombstone so that send cannot escape it.
        registry
            .pending_cancel
            .retain(|_, time| time.elapsed() < Duration::from_secs(60));
        if registry.pending_cancel.len() >= 1024 {
            if let Some(oldest) = registry
                .pending_cancel
                .iter()
                .min_by_key(|(_, time)| **time)
                .map(|(id, _)| id.clone())
            {
                registry.pending_cancel.remove(&oldest);
            }
        }
        registry.pending_cancel.insert(id.into(), Instant::now());
    }
    true
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug)]
struct Message {
    request_line: String,
    method: String,
    target: String,
    version: String,
    headers: Vec<String>,
    body: Vec<u8>,
}
impl Message {
    fn parse(raw: &str) -> Result<Self> {
        let split = raw
            .find("\r\n\r\n")
            .map(|n| (n, 4))
            .or_else(|| raw.find("\n\n").map(|n| (n, 2)));
        let (head, body) = split.map_or((raw, ""), |(n, len)| (&raw[..n], &raw[n + len..]));
        if head.len() > HEADER_LIMIT {
            return Err(Error::new(413, "HTTP message headers exceed 64 KiB"));
        }
        let mut lines = head.lines();
        let first = lines
            .next()
            .ok_or_else(|| Error::bad("请输入完整 HTTP 报文"))?;
        let parts = first.split_ascii_whitespace().collect::<Vec<_>>();
        if parts.len() != 3 || !["HTTP/1.0", "HTTP/1.1"].contains(&parts[2]) {
            return Err(Error::bad(
                "请求行格式应为 METHOD /path HTTP/1.1（支持 HTTP/1.0 和 HTTP/1.1）",
            ));
        }
        hyper::Method::from_bytes(parts[0].as_bytes()).map_err(Error::bad)?;
        if parts[1].bytes().any(|b| b < 33 || b == 127) {
            return Err(Error::bad("Invalid HTTP request target"));
        }
        let headers = lines.map(str::to_string).collect::<Vec<_>>();
        for line in &headers {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| Error::bad("请求头格式应为 Name: Value"))?;
            hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(Error::bad)?;
            hyper::header::HeaderValue::from_str(value.trim_start()).map_err(Error::bad)?;
        }
        Ok(Self {
            request_line: first.into(),
            method: parts[0].into(),
            target: parts[1].into(),
            version: parts[2].into(),
            headers,
            body: body.as_bytes().to_vec(),
        })
    }
    fn values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
            .collect()
    }
    fn set_header(&mut self, name: &str, value: Option<&Value>) {
        let first = self
            .headers
            .iter()
            .position(|line| {
                line.split_once(':')
                    .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
            })
            .unwrap_or(self.headers.len());
        self.headers.retain(|line| {
            !line
                .split_once(':')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
        });
        let values = value
            .map(|v| match v {
                Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
                _ => v.as_str().into_iter().collect(),
            })
            .unwrap_or_default();
        self.headers.splice(
            first..first,
            values.into_iter().map(|value| format!("{name}: {value}")),
        );
    }
    fn as_request(&self, url: &str) -> Result<RequestData> {
        let parsed = wire::parse_url(url)?;
        // Script path/query APIs refer to the editable request line, while the
        // address field supplies the connection's scheme, host, and port.
        let effective_url = if self.target.starts_with('/') {
            format!("{}{}", &parsed[..url::Position::BeforePath], self.target)
        } else {
            url.into()
        };
        Ok(RequestData {
            method: self.method.clone(),
            url: effective_url,
            headers: wire::parse_headers(&self.headers.join("\n"))?,
            body: String::from_utf8(self.body.clone()).ok(),
            binary_body: if std::str::from_utf8(&self.body).is_err() {
                Some(self.body.clone())
            } else {
                None
            },
        })
    }
    fn apply_script(&mut self, previous: &RequestData, next: &RequestData) -> Result<()> {
        let mut changed_line = false;
        if previous.method != next.method {
            self.method = next.method.clone();
            changed_line = true;
        }
        if previous.url != next.url {
            let url = wire::parse_url(&next.url)?;
            self.target = url[url::Position::BeforePath..url::Position::AfterQuery].to_string();
            if self.target.is_empty() {
                self.target = "/".into();
            }
            changed_line = true;
        }
        if changed_line {
            self.request_line = format!("{} {} {}", self.method, self.target, self.version);
        }
        for key in previous
            .headers
            .keys()
            .chain(next.headers.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            if previous.headers.get(key) != next.headers.get(key) {
                self.set_header(key, next.headers.get(key));
            }
        }
        let body = next
            .binary_body
            .clone()
            .unwrap_or_else(|| next.body.clone().unwrap_or_default().into_bytes());
        if body != self.body {
            self.body = body;
            if !self.values("transfer-encoding").is_empty() {
                return Err(Error::bad(
                    "脚本修改正文前，请移除 Transfer-Encoding，并使用未分块正文",
                ));
            }
            self.set_header("Content-Length", Some(&json!(self.body.len().to_string())));
        }
        Ok(())
    }
    fn encode(&mut self, url: &url::Url) -> Vec<u8> {
        if self.values("host").is_empty() {
            self.headers.push(format!(
                "Host: {}",
                &url[url::Position::BeforeHost..url::Position::AfterPort]
            ));
        }
        if !self.body.is_empty()
            && self.values("content-length").is_empty()
            && self.values("transfer-encoding").is_empty()
        {
            self.headers
                .push(format!("Content-Length: {}", self.body.len()));
        }
        let mut bytes = format!(
            "{}\r\n{}\r\n\r\n",
            self.request_line,
            self.headers.join("\r\n")
        )
        .into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

/// Returns the same JSON session shape as /request, plus exact raw response bytes.
pub async fn request(
    input: Value,
    tls: Arc<ClientConfig>,
    limit: usize,
    default_timeout: Duration,
    python_executable: &str,
    python_timeout_ms: u64,
) -> Result<Value> {
    let id = input
        .get("requestID")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let token = CancellationToken::new();
    if id.len() > 256 {
        return Err(Error::bad("requestID exceeds 256 bytes"));
    }
    {
        let mut registry = requests().lock().unwrap();
        if registry.active.contains_key(&id) {
            return Err(Error::bad("A raw request with this ID is already running"));
        }
        if registry
            .pending_cancel
            .remove(&id)
            .is_some_and(|time| time.elapsed() < Duration::from_secs(60))
        {
            return Err(Error::new(499, "请求已取消"));
        }
        registry.active.insert(id.clone(), token.clone());
    }
    let _active = ActiveRequest(id);
    let timeout = input
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(default_timeout.as_secs_f64());
    if !timeout.is_finite() || !(0.05..=3600.0).contains(&timeout) {
        return Err(Error::bad("请求超时应在 0.05 到 3600 秒之间"));
    }
    tokio::select! {
        _ = token.cancelled() => Err(Error::new(499, "请求已取消")),
        result = tokio::time::timeout(Duration::from_secs_f64(timeout), execute(&input, tls, limit, python_executable, python_timeout_ms)) => result.map_err(|_| Error::new(504, "HTTP 报文请求超时"))?,
    }
}

/// Archive bodies through the same path as capture/composer requests. The return
/// value keeps complete display bodies; persisted history uses bounded previews.
pub fn record(value: &mut Value, history: Arc<Mutex<History>>, preview: usize) -> Result<()> {
    let original: Session = serde_json::from_value(value.clone())?;
    let request_bytes = base64::engine::general_purpose::STANDARD
        .decode(value["requestBodyBase64"].as_str().unwrap_or(""))
        .map_err(Error::bad)?;
    let response_bytes = base64::engine::general_purpose::STANDARD
        .decode(value["responseBodyBase64"].as_str().unwrap_or(""))
        .map_err(Error::bad)?;
    let request_headers = wire::parse_headers(&original.request_headers)?;
    let response_headers = wire::parse_headers(&original.response_headers)?;
    let mut recording = Recording::new_hidden(
        history.clone(),
        "raw-http",
        &original.method,
        &original.url,
        request_headers.clone(),
        preview,
        false,
    );
    let id = recording.session.id.clone();
    let incognito = recording.session.incognito;
    let capture_group = recording.session.capture_group.clone();
    let capture_list_matches = std::mem::take(&mut recording.session.capture_list_matches);
    recording.session = original.clone();
    recording.session.capture_list_matches = capture_list_matches;
    recording.session.id = id.clone();
    recording.session.incognito = incognito;
    recording.session.capture_group = capture_group.clone();
    recording.request_headers = request_headers;
    recording.response_headers = response_headers;
    recording.request.add(&request_bytes);
    recording.response.add(&response_bytes);
    let mut saved = recording.finish(original.error.clone());
    saved.duration = original.duration;
    history.lock().unwrap().save(&saved);
    value["id"] = json!(id);
    value["incognito"] = json!(incognito);
    value["captureGroup"] = json!(capture_group);
    value["requestBodyComplete"] = json!(saved.request_body_complete);
    value["responseBodyComplete"] = json!(saved.response_body_complete);
    if !saved.capture_list_matches.is_empty() {
        value["captureListMatches"] = json!(saved.capture_list_matches);
    }
    if let Some(error) = saved.capture_list_filter_error {
        value["captureListFilterError"] = json!(error);
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("requestBodyBase64");
        object.remove("responseBodyBase64");
    }
    Ok(())
}

async fn execute(
    input: &Value,
    tls: Arc<ClientConfig>,
    limit: usize,
    python_executable: &str,
    python_timeout_ms: u64,
) -> Result<Value> {
    let began = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    let raw = input
        .get("raw")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::bad("raw must be a complete HTTP message string"))?;
    if raw.len() > limit.saturating_add(HEADER_LIMIT) {
        return Err(Error::new(413, "HTTP message exceeds request limit"));
    }
    let mut message = Message::parse(raw)?;
    if message.body.len() > limit {
        return Err(Error::new(413, "HTTP message body exceeds request limit"));
    }
    let mut url = wire::parse_url(
        input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::bad("请输入连接目标 URL"))?,
    )?;
    let mut req = message.as_request(url.as_str())?;
    let mut variables = input.get("variables").cloned().unwrap_or(json!({}));
    if !variables
        .as_object()
        .is_some_and(|values| values.values().all(Value::is_string))
    {
        return Err(Error::bad("variables must contain string values"));
    }
    let mut shared = json!({"_context":{"env":variables.clone()},"_shared":null});
    let mut logs = vec![];
    let mut tests = vec![];
    let before = input
        .get("beforeScript")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !before.trim().is_empty() {
        let result = crate::api_scripts::execute(before, &req, None, &variables)?;
        message.apply_script(&req, &result.request)?;
        req = result.request;
        variables = result.variables;
        logs.extend(result.logs);
        tests.extend(result.tests);
    }
    let python = input
        .get("pythonScript")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !python.trim().is_empty() {
        shared["_context"]["env"] = variables.clone();
        let (next, _, updated) = crate::python::execute(
            python_executable,
            python,
            &req,
            None,
            &shared,
            python_timeout_ms,
        )
        .await?;
        message.apply_script(&req, &next)?;
        req = next;
        shared = updated;
        variables = shared["_context"]["env"].clone();
        if let Some(log) = shared["_console"].as_str().filter(|s| !s.is_empty()) {
            logs.push(log.into());
        }
    }
    if message.body.len() > limit {
        return Err(Error::new(
            413,
            "Script request body exceeds the request limit",
        ));
    }
    url = wire::parse_url(&req.url)?;
    let bytes = message.encode(&url);
    if bytes.len().saturating_sub(message.body.len()) > HEADER_LIMIT {
        return Err(Error::new(413, "Script request headers exceed 64 KiB"));
    }
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    let socket = TcpStream::connect((host, url.port_or_known_default().unwrap())).await?;
    let mut socket: Box<dyn Io> = if url.scheme() == "https" {
        let mut config = (*tls).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        if input.get("verifyTLS").and_then(Value::as_bool) == Some(false) {
            config
                .dangerous()
                .set_certificate_verifier(Arc::new(UnverifiedCertificate));
        }
        Box::new(
            TlsConnector::from(Arc::new(config))
                .connect(
                    ServerName::try_from(host.to_string()).map_err(Error::bad)?,
                    socket,
                )
                .await?,
        )
    } else {
        Box::new(socket)
    };
    socket.write_all(&bytes).await?;
    socket.flush().await?;
    let response = read_response(&mut socket, &message.method, limit).await?;
    let headers = wire::parse_headers(&response.headers.join("\n"))?;
    let (body, binary) = wire::script_body(&response.body, &headers, limit)
        .unwrap_or_else(|_| (None, Some(response.body.clone())));
    let mut script_response = ResponseData {
        status: response.status,
        headers,
        body,
        binary_body: binary,
    };
    let initial_response = script_response.clone();
    if !python.trim().is_empty() {
        let (_, changed, updated) = crate::python::execute(
            python_executable,
            python,
            &req,
            Some(&script_response),
            &shared,
            python_timeout_ms,
        )
        .await?;
        shared = updated;
        variables = shared["_context"]["env"].clone();
        if let Some(changed) = changed {
            script_response = changed;
        }
        if let Some(log) = shared["_console"].as_str().filter(|s| !s.is_empty()) {
            logs.push(log.into());
        }
    }
    let after = input
        .get("afterScript")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !after.trim().is_empty() {
        let result = crate::api_scripts::execute(after, &req, Some(&script_response), &variables)?;
        variables = result.variables;
        logs.extend(result.logs);
        tests.extend(result.tests);
    }
    let changed_response =
        serde_json::to_value(&script_response)? != serde_json::to_value(&initial_response)?;
    let (archive_response, response_headers) = if changed_response {
        let bytes = script_response.binary_body.clone().unwrap_or_else(|| {
            script_response
                .body
                .clone()
                .unwrap_or_default()
                .into_bytes()
        });
        script_response.headers.remove("content-encoding");
        script_response.headers.remove("transfer-encoding");
        script_response
            .headers
            .insert("content-length".into(), json!(bytes.len().to_string()));
        (bytes, wire::header_text(&script_response.headers))
    } else {
        (response.body.clone(), response.headers.join("\r\n"))
    };
    if archive_response.len() > limit {
        return Err(Error::new(
            413,
            "Script response body exceeds the response limit",
        ));
    }
    let response_binary = script_response.binary_body.is_some();
    let body_text = script_response.body.unwrap_or_else(|| {
        format!(
            "[base64]\n{}",
            base64::engine::general_purpose::STANDARD
                .encode(script_response.binary_body.unwrap_or_default())
        )
    });
    let request_binary = std::str::from_utf8(&message.body).is_err()
        || message
            .body
            .iter()
            .any(|b| *b == 0 || (*b < 32 && ![9, 10, 13].contains(b)));
    let request_text = if request_binary {
        format!(
            "[base64]\n{}",
            base64::engine::general_purpose::STANDARD.encode(&message.body)
        )
    } else {
        String::from_utf8_lossy(&message.body).into_owned()
    };
    Ok(
        json!({"id":uuid::Uuid::new_v4().to_string(),"source":"raw-http","method":message.method,"url":url.as_str(),"status":script_response.status,
        "requestHeaders":message.headers.join("\r\n"),"responseHeaders":response_headers,"requestBody":request_text,"responseBody":body_text,
        "startedAt":started_at,"duration":began.elapsed().as_millis() as u64,"requestBytes":message.body.len(),"responseBytes":response.body.len(),"error":null,"tls":url.scheme()=="https",
        "protocol":response.version,"responseTrailers":response.trailers.join("\r\n"),"requestBodyComplete":true,"responseBodyComplete":true,"requestBodyLoaded":true,"responseBodyLoaded":true,
        "requestBodyEncoding":if request_binary {"base64"} else {"utf8"}, "responseBodyEncoding":if response_binary {"base64"} else {"utf8"},
        "requestBodyBase64":base64::engine::general_purpose::STANDARD.encode(&message.body),"responseBodyBase64":base64::engine::general_purpose::STANDARD.encode(&archive_response),
        "rawResponseBase64":base64::engine::general_purpose::STANDARD.encode(&response.raw),"scriptLogs":logs,"tests":tests,"variables":variables}),
    )
}

#[derive(Debug)]
struct UnverifiedCertificate;
impl rustls::client::danger::ServerCertVerifier for UnverifiedCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct Reader<'a> {
    socket: &'a mut Box<dyn Io>,
    buffer: Vec<u8>,
    raw: Vec<u8>,
    wire_limit: usize,
}
impl Reader<'_> {
    async fn fill(&mut self) -> Result<bool> {
        let mut bytes = [0; 8192];
        let n = self.socket.read(&mut bytes).await?;
        if n > self.wire_limit.saturating_sub(self.raw.len()) {
            return Err(Error::new(
                413,
                "HTTP response framing exceeds buffering limit",
            ));
        }
        self.buffer.extend_from_slice(&bytes[..n]);
        self.raw.extend_from_slice(&bytes[..n]);
        Ok(n > 0)
    }
    async fn line(&mut self) -> Result<String> {
        loop {
            if let Some(n) = self.buffer.iter().position(|b| *b == b'\n') {
                if n > HEADER_LIMIT {
                    return Err(Error::upstream("HTTP response line exceeds 64 KiB"));
                }
                let line = self.buffer.drain(..=n).collect::<Vec<_>>();
                return String::from_utf8(
                    line[..n].strip_suffix(b"\r").unwrap_or(&line[..n]).to_vec(),
                )
                .map_err(Error::upstream);
            }
            if self.buffer.len() > HEADER_LIMIT {
                return Err(Error::upstream("HTTP response line exceeds 64 KiB"));
            }
            if !self.fill().await? {
                return Err(Error::upstream(
                    "Server closed before the HTTP response was complete",
                ));
            }
        }
    }
    async fn exact(&mut self, length: usize) -> Result<Vec<u8>> {
        while self.buffer.len() < length {
            if !self.fill().await? {
                return Err(Error::upstream("HTTP response body ended early"));
            }
        }
        Ok(self.buffer.drain(..length).collect())
    }
    async fn headers(&mut self) -> Result<Vec<String>> {
        let mut headers = vec![];
        let mut length = 0;
        loop {
            let line = self.line().await?;
            length += line.len() + 2;
            if length > HEADER_LIMIT {
                return Err(Error::upstream("HTTP response headers exceed 64 KiB"));
            }
            if line.is_empty() {
                return Ok(headers);
            }
            if !line.contains(':') {
                return Err(Error::upstream("Invalid HTTP response header"));
            }
            headers.push(line);
        }
    }
}
struct Received {
    status: u16,
    version: String,
    headers: Vec<String>,
    trailers: Vec<String>,
    body: Vec<u8>,
    raw: Vec<u8>,
}
async fn read_response(socket: &mut Box<dyn Io>, method: &str, limit: usize) -> Result<Received> {
    let mut reader = Reader {
        socket,
        buffer: vec![],
        raw: vec![],
        wire_limit: limit.saturating_mul(2).saturating_add(HEADER_LIMIT * 4),
    };
    let (status, version, headers) = loop {
        let line = reader.line().await?;
        let mut parts = line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        let status = parts
            .next()
            .unwrap_or("")
            .parse::<u16>()
            .map_err(|_| Error::upstream("Invalid HTTP response status line"))?;
        if !["HTTP/1.0", "HTTP/1.1"].contains(&version) || !(100..=599).contains(&status) {
            return Err(Error::upstream("Invalid HTTP response status line"));
        }
        let headers = reader.headers().await?;
        if !(100..200).contains(&status) || status == 101 {
            break (status, version.to_string(), headers);
        }
        if reader.raw.len() > HEADER_LIMIT * 4 {
            return Err(Error::upstream("Too many informational HTTP responses"));
        }
    };
    let parsed = wire::parse_headers(&headers.join("\n"))?;
    let mut body = vec![];
    let mut trailers = vec![];
    if method != "HEAD" && ![101, 204, 304].contains(&status) {
        let transfer = parsed
            .get("transfer-encoding")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !transfer.is_empty() {
            if !transfer.eq_ignore_ascii_case("chunked") {
                return Err(Error::upstream(
                    "Unsupported HTTP response Transfer-Encoding",
                ));
            }
            loop {
                let line = reader.line().await?;
                let length = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                    .map_err(|_| Error::upstream("Invalid HTTP chunk length"))?;
                if length == 0 {
                    trailers = reader.headers().await?;
                    break;
                }
                if length > limit.saturating_sub(body.len()) {
                    return Err(Error::new(
                        413,
                        "HTTP response body exceeds buffering limit",
                    ));
                }
                body.extend(reader.exact(length).await?);
                if !reader.line().await?.is_empty() {
                    return Err(Error::upstream("Invalid HTTP chunk terminator"));
                }
            }
        } else if let Some(length) = parsed.get("content-length") {
            let length = length
                .as_str()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| Error::upstream("Invalid or repeated HTTP Content-Length"))?;
            if length > limit {
                return Err(Error::new(
                    413,
                    "HTTP response body exceeds buffering limit",
                ));
            }
            body = reader.exact(length).await?;
        } else {
            loop {
                if reader.buffer.len() > limit.saturating_sub(body.len()) {
                    return Err(Error::new(
                        413,
                        "HTTP response body exceeds buffering limit",
                    ));
                }
                body.append(&mut reader.buffer);
                if !reader.fill().await? {
                    break;
                }
            }
        }
    }
    // Bytes read beyond this response belong to a subsequent message, if any.
    reader.raw.truncate(reader.raw.len() - reader.buffer.len());
    Ok(Received {
        status,
        version,
        headers,
        trailers,
        body,
        raw: reader.raw,
    })
}

#[cfg(test)]
#[path = "raw_client_tests.rs"]
mod tests;
