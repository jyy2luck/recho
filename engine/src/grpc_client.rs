//! Local gRPC API client: schema compilation/reflection and all four RPC shapes.
//! Connections are independent of the capture proxy and never require an account.
use base64::Engine as _;
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::{
    Request,
    body::{Body, Frame, SizeHint},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

const MESSAGE_LIMIT: usize = 16 * 1024 * 1024;
const EVENT_LIMIT: usize = 2_000;
type Result<T> = std::result::Result<T, String>;
fn fail(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn text<'a>(input: &'a Value, key: &str) -> &'a str {
    input[key].as_str().unwrap_or("")
}
fn boolean(input: &Value, key: &str, fallback: bool) -> bool {
    input[key].as_bool().unwrap_or(fallback)
}
fn timeout(input: &Value) -> Duration {
    Duration::from_secs_f64(input["timeout"].as_f64().unwrap_or(30.0).clamp(0.1, 3600.0))
}
fn registry() -> &'static Mutex<HashMap<String, Arc<Client>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Arc<Client>>>> = OnceLock::new();
    STORE.get_or_init(Mutex::default)
}
struct Client {
    method: MethodDescriptor,
    outgoing: Mutex<Option<mpsc::Sender<Bytes>>>,
    cancel: CancellationToken,
    state: Mutex<State>,
    started: Instant,
}
struct State {
    events: Vec<Value>,
    sequence: u64,
    completed: bool,
    code: Option<i32>,
    retained: usize,
}
impl Client {
    fn event(&self, kind: &str, payload: Value) {
        let mut state = self.state.lock().unwrap();
        state.sequence += 1;
        let sequence = state.sequence;
        let event = json!({"sequence":sequence,"kind":kind,"value":payload,"elapsed":self.started.elapsed().as_secs_f64()});
        state.retained += event.to_string().len();
        state.events.push(event);
        while state.events.len() > EVENT_LIMIT || state.retained > MESSAGE_LIMIT * 2 {
            let old = state.events.remove(0);
            state.retained = state.retained.saturating_sub(old.to_string().len());
        }
    }
    fn finish(&self, code: i32, message: &str) {
        self.event("status", json!({"code":code,"message":message}));
        *self.outgoing.lock().unwrap() = None;
        let mut state = self.state.lock().unwrap();
        state.completed = true;
        state.code = Some(code);
    }
    fn snapshot(&self, after: u64) -> Value {
        let state = self.state.lock().unwrap();
        json!({"events":state.events.iter().filter(|e|e["sequence"].as_u64().unwrap_or(0)>after).collect::<Vec<_>>(),"sequence":state.sequence,"completed":state.completed,"code":state.code,"firstSequence":state.events.first().and_then(|e|e["sequence"].as_u64()).unwrap_or(0)})
    }
}

/// JSON control routes, called only after the host control server authenticates.
/// schema: {sources:[{name,source}]} or {reflection:true,url,metadata,verifyTLS}.
/// start: {sources|descriptorSet,method,url,metadata,messages:[JSON],finish,timeout,verifyTLS}.
pub async fn control(method: &str, path: &str, input: Value) -> Result<Value> {
    let path = path.trim_start_matches("/grpc-client/");
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    match (method, path) {
        ("POST", "schema") => {
            let pool = if boolean(&input, "reflection", false) {
                reflect(&input).await?
            } else {
                compile(&input)?
            };
            Ok(schema(&pool))
        }
        ("POST", "start") => {
            let pool = if let Some(encoded) = input["descriptorSet"].as_str() {
                DescriptorPool::decode(
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(fail)?
                        .as_slice(),
                )
                .map_err(fail)?
            } else if boolean(&input, "reflection", false) {
                reflect(&input).await?
            } else {
                compile(&input)?
            };
            let selected = find_method(&pool, text(&input, "method"))?;
            let messages = input["messages"].as_array().cloned().unwrap_or_default();
            if messages.len() > 32 {
                return Err("单次初始请求消息不能超过 32 条".into());
            }
            if !selected.is_client_streaming() && messages.len() != 1 {
                return Err("此方法必须提供一条请求消息".into());
            }
            let encoded = messages
                .iter()
                .map(|v| encode_json(&selected, v))
                .collect::<Result<Vec<_>>>()?;
            let (tx, rx) = mpsc::channel(32);
            let id = uuid::Uuid::new_v4().to_string();
            let client = Arc::new(Client {
                method: selected.clone(),
                outgoing: Mutex::new(Some(tx.clone())),
                cancel: CancellationToken::new(),
                state: Mutex::new(State {
                    events: vec![],
                    sequence: 0,
                    completed: false,
                    code: None,
                    retained: 0,
                }),
                started: Instant::now(),
            });
            {
                let mut store = registry().lock().unwrap();
                store.retain(|_, v| {
                    !v.state.lock().unwrap().completed
                        || v.started.elapsed() < Duration::from_secs(600)
                });
                if store.len() >= 16 {
                    let oldest = store
                        .iter()
                        .filter(|(_, v)| v.state.lock().unwrap().completed)
                        .max_by_key(|(_, v)| v.started.elapsed())
                        .map(|(id, _)| id.clone());
                    if let Some(oldest) = oldest {
                        store.remove(&oldest);
                    }
                }
                if store.len() >= 16 {
                    return Err("最多同时进行 16 个 gRPC 调用，请先取消其他调用".into());
                }
                store.insert(id.clone(), client.clone());
            }
            // Enqueue before returning, without waiting for the response headers.
            for data in encoded {
                tx.send(frame(data.clone())?).await.map_err(fail)?;
                client.event("sent", decode_json(selected.input(), data)?);
            }
            if boolean(&input, "finish", !selected.is_client_streaming()) {
                *client.outgoing.lock().unwrap() = None;
            }
            drop(tx);
            let task_client = client.clone();
            tokio::spawn(async move {
                let result = tokio::select! {
                    _ = task_client.cancel.cancelled() => Err((1,"已取消".to_string())),
                    result = tokio::time::timeout(timeout(&input),run_call(&input,rx,&task_client)) => match result { Ok(r)=>r, Err(_)=>Err((4,"调用超时".into())) }
                };
                match result {
                    Ok((code, message)) | Err((code, message)) => {
                        task_client.finish(code, &message)
                    }
                }
            });
            Ok(json!({"id":id,"method":method_json(&selected)}))
        }
        _ => {
            let (id, action) = path.split_once('/').ok_or("未知 gRPC 客户端路径")?;
            let client = registry()
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or("gRPC 调用不存在")?;
            match (method, action) {
                ("GET", "events") => {
                    let after = url::form_urlencoded::parse(query.as_bytes())
                        .find(|(k, _)| k == "after")
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    Ok(client.snapshot(after))
                }
                ("POST", "send") => {
                    if !client.method.is_client_streaming() {
                        return Err("此方法不支持发送多条请求消息".into());
                    }
                    let value = &input["message"];
                    let data = encode_json(&client.method, value)?;
                    let tx = client
                        .outgoing
                        .lock()
                        .unwrap()
                        .clone()
                        .ok_or("请求流已结束")?;
                    tx.try_send(frame(data.clone())?).map_err(fail)?;
                    client.event("sent", decode_json(client.method.input(), data)?);
                    Ok(json!({"ok":true}))
                }
                ("POST", "finish") => {
                    *client.outgoing.lock().unwrap() = None;
                    client.event("request-end", Value::Null);
                    Ok(json!({"ok":true}))
                }
                ("POST", "cancel") => {
                    client.cancel.cancel();
                    Ok(json!({"ok":true}))
                }
                ("DELETE", _) => {
                    client.cancel.cancel();
                    registry().lock().unwrap().remove(id);
                    Ok(json!({"ok":true}))
                }
                _ => Err("未知 gRPC 客户端操作".into()),
            }
        }
    }
}

struct SchemaTemp(std::path::PathBuf);
impl Drop for SchemaTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn compile(input: &Value) -> Result<DescriptorPool> {
    let sources = input["sources"]
        .as_array()
        .ok_or("请选择或导入 Proto 规范文件")?;
    if sources.is_empty() || sources.len() > 128 {
        return Err("Proto 规范文件数量必须为 1–128".into());
    }
    let root = std::env::temp_dir().join(format!("recho-proto-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(fail)?;
    }
    let _cleanup = SchemaTemp(root.clone());
    let mut files = vec![];
    let mut total = 0;
    for source in sources {
        let name = text(source, "name");
        let code = text(source, "source");
        total += code.len();
        if name.is_empty()
            || !name.ends_with(".proto")
            || Path::new(name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || total > 8 * 1024 * 1024
        {
            return Err("规范文件名必须是相对 .proto 路径，且源码总计不能超过 8 MiB".into());
        }
        let path = root.join(name);
        if files.contains(&path) {
            return Err(format!("存在同名 Proto 规范文件：{name}"));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(fail)?;
        }
        std::fs::write(&path, code).map_err(fail)?;
        files.push(path);
    }
    if !text(input, "root").is_empty() {
        let selected = root.join(text(input, "root"));
        if !files.contains(&selected) {
            return Err("所选 Proto 规范文件不存在".into());
        }
        files = vec![selected];
    }
    let descriptors = protox::compile(files, [root]).map_err(|e| format!("Proto 编译失败：{e}"))?;
    DescriptorPool::from_file_descriptor_set(descriptors).map_err(fail)
}
fn find_method(pool: &DescriptorPool, name: &str) -> Result<MethodDescriptor> {
    let name = name.trim_start_matches('/');
    pool.services()
        .flat_map(|s| s.methods().collect::<Vec<_>>())
        .find(|m| {
            format!("{}/{}", m.parent_service().full_name(), m.name()) == name
                || m.full_name() == name
        })
        .ok_or_else(|| format!("未找到 gRPC 方法：{name}"))
}
fn method_json(method: &MethodDescriptor) -> Value {
    json!({"name":method.name(),"path":format!("/{}/{}",method.parent_service().full_name(),method.name()),"input":method.input().full_name(),"output":method.output().full_name(),"clientStreaming":method.is_client_streaming(),"serverStreaming":method.is_server_streaming(),"template":message_template(method.input(),0)})
}
fn message_template(message: prost_reflect::MessageDescriptor, depth: usize) -> Value {
    match message.full_name() {
        "google.protobuf.Timestamp" => return json!("1970-01-01T00:00:00Z"),
        "google.protobuf.Duration" => return json!("0s"),
        "google.protobuf.FieldMask"
        | "google.protobuf.StringValue"
        | "google.protobuf.BytesValue" => return json!(""),
        "google.protobuf.BoolValue" => return json!(false),
        "google.protobuf.DoubleValue"
        | "google.protobuf.FloatValue"
        | "google.protobuf.Int64Value"
        | "google.protobuf.UInt64Value"
        | "google.protobuf.Int32Value"
        | "google.protobuf.UInt32Value" => return json!(0),
        "google.protobuf.ListValue" => return json!([]),
        "google.protobuf.Value" => return Value::Null,
        _ => {}
    }
    let mut result = serde_json::Map::new();
    if depth > 3 {
        return Value::Object(result);
    }
    let mut oneofs = std::collections::BTreeSet::new();
    for field in message.fields() {
        if let Some(oneof) = field.containing_oneof() {
            if !oneofs.insert(oneof.full_name().to_string()) {
                continue;
            }
        }
        let value = if field.is_list() {
            json!([])
        } else if field.is_map() {
            json!({})
        } else {
            match field.kind() {
                prost_reflect::Kind::Message(m) => message_template(m, depth + 1),
                prost_reflect::Kind::Bool => json!(false),
                prost_reflect::Kind::String | prost_reflect::Kind::Bytes => json!(""),
                prost_reflect::Kind::Enum(e) => json!(e.default_value().name()),
                _ => json!(0),
            }
        };
        result.insert(field.json_name().into(), value);
    }
    Value::Object(result)
}
fn schema(pool: &DescriptorPool) -> Value {
    json!({"services":pool.services().filter(|s|!s.full_name().starts_with("grpc.reflection.")).map(|s|json!({"name":s.full_name(),"methods":s.methods().map(|m|method_json(&m)).collect::<Vec<_>>()})).collect::<Vec<_>>(),"descriptorSet":base64::engine::general_purpose::STANDARD.encode(pool.encode_to_vec())})
}
fn encode_json(method: &MethodDescriptor, value: &Value) -> Result<Bytes> {
    let source = serde_json::to_vec(value).map_err(fail)?;
    let mut deserializer = serde_json::Deserializer::from_slice(&source);
    let message = DynamicMessage::deserialize(method.input(), &mut deserializer)
        .map_err(|e| format!("请求 JSON 不符合 {}：{e}", method.input().full_name()))?;
    let data = message.encode_to_vec();
    if data.len() > MESSAGE_LIMIT {
        return Err("gRPC 消息超过 16 MiB".into());
    }
    Ok(data.into())
}
fn decode_json(descriptor: prost_reflect::MessageDescriptor, data: Bytes) -> Result<Value> {
    let message = DynamicMessage::decode(descriptor, data).map_err(fail)?;
    serde_json::to_value(message).map_err(fail)
}
fn frame(data: Bytes) -> Result<Bytes> {
    if data.len() > MESSAGE_LIMIT {
        return Err("gRPC 消息超过 16 MiB".into());
    }
    let mut bytes = Vec::with_capacity(data.len() + 5);
    bytes.push(0);
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&data);
    Ok(bytes.into())
}

struct RequestBody(mpsc::Receiver<Bytes>);
impl Body for RequestBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        self.0.poll_recv(cx).map(|v| v.map(|b| Ok(Frame::data(b))))
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
struct Driver(tokio::task::JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Debug)]
struct SkipCertificateVerification;
impl rustls::client::danger::ServerCertVerifier for SkipCertificateVerification {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
fn server_url(input: &Value) -> Result<url::Url> {
    let raw = text(input, "url").trim();
    let mut url = url::Url::parse(&if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    })
    .map_err(fail)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("gRPC 地址必须为 http:// 或 https:// 服务端地址".into());
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}
fn metadata(input: &Value) -> Result<hyper::HeaderMap> {
    let mut result = hyper::HeaderMap::new();
    if let Some(source) = input["metadata"].as_str() {
        for line in source.lines().filter(|l| !l.trim().is_empty()) {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| format!("元数据格式错误：{line}"))?;
            let key = hyper::header::HeaderName::from_bytes(key.trim().as_bytes()).map_err(fail)?;
            if [
                "content-type",
                "te",
                "host",
                "grpc-timeout",
                "grpc-encoding",
                "content-length",
                "connection",
                "transfer-encoding",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("{key} 是保留的 gRPC 元数据"));
            }
            let value = value.trim();
            if key.as_str().ends_with("-bin") {
                base64::engine::general_purpose::STANDARD
                    .decode(value)
                    .map_err(|e| format!("二进制元数据必须为 Base64：{e}"))?;
            }
            result.append(
                key,
                hyper::header::HeaderValue::from_str(value).map_err(fail)?,
            );
        }
    }
    Ok(result)
}
fn headers_json(headers: &hyper::HeaderMap) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(k, v)| json!({"name":k.as_str(),"value":String::from_utf8_lossy(v.as_bytes())}))
            .collect(),
    )
}
fn grpc_status(headers: &hyper::HeaderMap) -> Option<(i32, String)> {
    headers
        .get("grpc-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .map(|code| {
            (
                code,
                headers
                    .get("grpc-message")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| {
                        url::form_urlencoded::parse(
                            format!("v={}", s.replace('+', "%2B")).as_bytes(),
                        )
                        .next()
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default()
                    })
                    .unwrap_or_default(),
            )
        })
}
async fn transport<F>(
    input: &Value,
    path: &str,
    rx: mpsc::Receiver<Bytes>,
    mut event: F,
) -> Result<(i32, String)>
where
    F: FnMut(&str, Value, Option<Bytes>) + Send,
{
    let mut url = server_url(input)?;
    url.set_path(path);
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    let socket = TcpStream::connect((host, url.port_or_known_default().unwrap()))
        .await
        .map_err(fail)?;
    socket.set_nodelay(true).map_err(fail)?;
    let io: Box<dyn Io> = if url.scheme() == "https" {
        let mut config = (*crate::certificates::client_config(None).map_err(fail)?).clone();
        config.alpn_protocols = vec![b"h2".to_vec()];
        if !boolean(input, "verifyTLS", true) {
            config
                .dangerous()
                .set_certificate_verifier(Arc::new(SkipCertificateVerification));
        }
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(
                rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(fail)?,
                socket,
            )
            .await
            .map_err(fail)?;
        if tls.get_ref().1.alpn_protocol() != Some(b"h2") {
            return Err("服务端未协商 HTTP/2".into());
        }
        Box::new(tls)
    } else {
        Box::new(socket)
    };
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(io))
            .await
            .map_err(fail)?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let mut request = Request::builder()
        .method("POST")
        .uri(url.as_str())
        .version(hyper::Version::HTTP_2)
        .body(RequestBody(rx))
        .map_err(fail)?;
    *request.headers_mut() = metadata(input)?;
    request.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/grpc"),
    );
    request
        .headers_mut()
        .insert("te", hyper::header::HeaderValue::from_static("trailers"));
    request.headers_mut().insert(
        "grpc-accept-encoding",
        hyper::header::HeaderValue::from_static("identity,gzip"),
    );
    request.headers_mut().insert(
        "grpc-timeout",
        hyper::header::HeaderValue::from_str(&format!("{}m", timeout(input).as_millis()))
            .map_err(fail)?,
    );
    let response = sender.send_request(request).await.map_err(fail)?;
    let (parts, mut body) = response.into_parts();
    event(
        "headers",
        json!({"httpStatus":parts.status.as_u16(),"metadata":headers_json(&parts.headers)}),
        None,
    );
    if parts.status != hyper::StatusCode::OK {
        return Err(format!("服务端返回 HTTP {}", parts.status));
    }
    if !parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/grpc"))
    {
        return Err("服务端响应不是 application/grpc".into());
    }
    let encoding = parts
        .headers
        .get("grpc-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("identity")
        .to_string();
    let mut status = grpc_status(&parts.headers);
    let mut pending = BytesMut::new();
    while let Some(next) = body.frame().await {
        let next = next.map_err(fail)?;
        match next.into_data() {
            Ok(data) => {
                pending.extend_from_slice(&data);
                while pending.len() >= 5 {
                    let compressed = pending[0];
                    let len = u32::from_be_bytes(pending[1..5].try_into().unwrap()) as usize;
                    if len > MESSAGE_LIMIT {
                        return Err("服务端 gRPC 消息超过 16 MiB".into());
                    }
                    if pending.len() < len + 5 {
                        break;
                    }
                    pending.advance(5);
                    let mut data = pending.split_to(len).freeze();
                    if compressed == 1 {
                        if encoding != "gzip" {
                            return Err(format!("不支持服务端 gRPC 压缩编码：{encoding}"));
                        }
                        use std::io::Read;
                        let mut out = vec![];
                        flate2::read::GzDecoder::new(data.as_ref())
                            .take(MESSAGE_LIMIT as u64 + 1)
                            .read_to_end(&mut out)
                            .map_err(fail)?;
                        if out.len() > MESSAGE_LIMIT {
                            return Err("解压后 gRPC 消息超过 16 MiB".into());
                        }
                        data = out.into();
                    } else if compressed != 0 {
                        return Err("无效 gRPC 消息压缩标志".into());
                    }
                    event("message", Value::Null, Some(data));
                }
            }
            Err(next) => {
                if let Ok(trailers) = next.into_trailers() {
                    status = grpc_status(&trailers).or(status);
                    event("trailers", headers_json(&trailers), None);
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err("服务端 gRPC 消息被截断".into());
    }
    status.ok_or("服务端缺少 grpc-status 拖尾".into())
}
async fn run_call(
    input: &Value,
    rx: mpsc::Receiver<Bytes>,
    client: &Client,
) -> std::result::Result<(i32, String), (i32, String)> {
    let path = format!(
        "/{}/{}",
        client.method.parent_service().full_name(),
        client.method.name()
    );
    let mut decode_error = None;
    let mut response_count = 0;
    let status = transport(input, &path, rx, |kind, value, data| {
        if let Some(data) = data {
            response_count += 1;
            match decode_json(client.method.output(), data) {
                Ok(value) => client.event("received", value),
                Err(error) => decode_error = Some(error),
            }
        } else {
            client.event(kind, value)
        }
    })
    .await
    .map_err(|e| (13, e))?;
    if let Some(error) = decode_error {
        return Err((13, error));
    }
    if status.0 == 0 && !client.method.is_server_streaming() && response_count != 1 {
        return Err((
            13,
            format!("此方法应返回一条响应消息，实际收到 {response_count} 条"),
        ));
    }
    Ok(status)
}

// Both reflection API versions use the same protobuf wire schema.
#[derive(Clone, PartialEq, Message)]
struct ReflectionRequest {
    #[prost(string, tag = "1")]
    host: String,
    #[prost(oneof = "reflection_request::MessageRequest", tags = "3,4,7")]
    request: Option<reflection_request::MessageRequest>,
}
mod reflection_request {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum MessageRequest {
        #[prost(string, tag = "3")]
        FileByFilename(String),
        #[prost(string, tag = "4")]
        FileContainingSymbol(String),
        #[prost(string, tag = "7")]
        ListServices(String),
    }
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionResponse {
    #[prost(oneof = "reflection_response::Response", tags = "4,6,7")]
    response: Option<reflection_response::Response>,
}
mod reflection_response {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Response {
        #[prost(message, tag = "4")]
        Files(super::ReflectionFiles),
        #[prost(message, tag = "6")]
        Services(super::ReflectionServices),
        #[prost(message, tag = "7")]
        Error(super::ReflectionError),
    }
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionFiles {
    #[prost(bytes = "vec", repeated, tag = "1")]
    files: Vec<Vec<u8>>,
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionServices {
    #[prost(message, repeated, tag = "1")]
    services: Vec<ReflectionService>,
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionService {
    #[prost(string, tag = "1")]
    name: String,
}
#[derive(Clone, PartialEq, Message)]
struct ReflectionError {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}
async fn reflection_request(
    input: &Value,
    version: &str,
    request: reflection_request::MessageRequest,
) -> Result<ReflectionResponse> {
    let request = ReflectionRequest {
        host: String::new(),
        request: Some(request),
    };
    let (tx, rx) = mpsc::channel(1);
    tx.send(frame(request.encode_to_vec().into())?)
        .await
        .map_err(fail)?;
    drop(tx);
    let mut messages = vec![];
    let (code, message) = transport(
        input,
        &format!("/grpc.reflection.{version}.ServerReflection/ServerReflectionInfo"),
        rx,
        |_, _, data| {
            if let Some(data) = data {
                messages.push(data);
            }
        },
    )
    .await?;
    if code != 0 {
        return Err(format!("服务反射失败 ({code})：{message}"));
    }
    ReflectionResponse::decode(messages.into_iter().next().ok_or("反射服务没有返回消息")?)
        .map_err(fail)
}
async fn reflect(input: &Value) -> Result<DescriptorPool> {
    tokio::time::timeout(timeout(input), async {
        let mut selected = None;
        let mut errors = vec![];
        for version in ["v1", "v1alpha"] {
            match reflection_request(
                input,
                version,
                reflection_request::MessageRequest::ListServices(String::new()),
            )
            .await
            {
                Ok(response) => {
                    selected = Some((version, response));
                    break;
                }
                Err(e) => errors.push(e),
            }
        }
        let (version, response) = selected.ok_or_else(|| errors.join("；"))?;
        let services = match response.response {
            Some(reflection_response::Response::Services(s)) => s.services,
            Some(reflection_response::Response::Error(e)) => {
                return Err(format!("服务反射失败 ({})：{}", e.code, e.message));
            }
            _ => return Err("服务反射没有返回服务列表".into()),
        };
        let mut files = BTreeMap::new();
        for service in services {
            if service.name.starts_with("grpc.reflection.") {
                continue;
            }
            let response = reflection_request(
                input,
                version,
                reflection_request::MessageRequest::FileContainingSymbol(service.name),
            )
            .await?;
            match response.response {
                Some(reflection_response::Response::Files(result)) => {
                    for file in result.files {
                        let descriptor = prost_types_descriptor(&file)?;
                        files.insert(descriptor, file);
                    }
                }
                Some(reflection_response::Response::Error(e)) => {
                    return Err(format!("服务反射失败 ({})：{}", e.code, e.message));
                }
                _ => return Err("服务反射没有返回规范描述".into()),
            }
        }
        // Some reflection implementations omit imported descriptors. Resolve them by name.
        for _ in 0..128 {
            if files.len() > 128 || files.values().map(Vec::len).sum::<usize>() > 8 * 1024 * 1024 {
                return Err("服务反射规范超过 128 个文件或 8 MiB".into());
            }
            let mut missing = std::collections::BTreeSet::new();
            for file in files.values() {
                for dependency in DescriptorInfo::decode(file.as_slice())
                    .map_err(fail)?
                    .dependencies
                {
                    if !files.contains_key(&dependency) {
                        missing.insert(dependency);
                    }
                }
            }
            if missing.is_empty() {
                let set = DescriptorBytes {
                    files: files.into_values().collect(),
                };
                return DescriptorPool::decode(set.encode_to_vec().as_slice()).map_err(fail);
            }
            let before = files.len();
            for dependency in missing {
                let response = reflection_request(
                    input,
                    version,
                    reflection_request::MessageRequest::FileByFilename(dependency),
                )
                .await?;
                match response.response {
                    Some(reflection_response::Response::Files(result)) => {
                        for file in result.files {
                            files.insert(prost_types_descriptor(&file)?, file);
                        }
                    }
                    Some(reflection_response::Response::Error(e)) => {
                        return Err(format!("反射依赖加载失败 ({})：{}", e.code, e.message));
                    }
                    _ => return Err("服务反射没有返回依赖文件".into()),
                }
            }
            if files.len() == before {
                return Err("反射规范包含无法解析的依赖文件".into());
            }
        }
        Err("反射规范依赖过多".into())
    })
    .await
    .map_err(|_| "服务反射超时".to_string())?
}
#[derive(Clone, PartialEq, Message)]
struct DescriptorInfo {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, repeated, tag = "3")]
    dependencies: Vec<String>,
}
#[derive(Clone, PartialEq, Message)]
struct DescriptorBytes {
    #[prost(bytes = "vec", repeated, tag = "1")]
    files: Vec<Vec<u8>>,
}
fn prost_types_descriptor(bytes: &[u8]) -> Result<String> {
    Ok(DescriptorInfo::decode(bytes).map_err(fail)?.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::{Response, service::service_fn};
    use std::convert::Infallible;
    const PROTO: &str = r#"syntax = "proto3"; package local.test; message Echo { string value = 1; } service EchoService { rpc Unary(Echo) returns(Echo); rpc Server(Echo) returns(stream Echo); rpc Client(stream Echo) returns(Echo); rpc Bidi(stream Echo) returns(stream Echo); rpc Hold(Echo) returns(Echo); rpc Denied(Echo) returns(Echo); }"#;
    fn input() -> Value {
        json!({"sources":[{"name":"echo.proto","source":PROTO}],"metadata":"x-test: local-value\nx-data-bin: AAE=","timeout":5})
    }
    struct TestBody(mpsc::Receiver<Frame<Bytes>>);
    impl Body for TestBody {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<Frame<Bytes>, Infallible>>> {
            self.0.poll_recv(cx).map(|x| x.map(Ok))
        }
    }
    async fn fixture() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pool = compile(&input()).unwrap();
        let descriptor = pool.files().next().unwrap().encode_to_vec();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let descriptor = descriptor.clone();
                tokio::spawn(async move {
                    let _=hyper::server::conn::http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(socket),service_fn(move|request:Request<hyper::body::Incoming>|{let descriptor=descriptor.clone();async move{
            let path=request.uri().path().to_owned();let metadata=request.headers().get("x-test").cloned();let(tx,rx)=mpsc::channel(32);let mut response=Response::builder().status(200).header("content-type","application/grpc").header("x-response","present");if let Some(metadata)=metadata{response=response.header("x-test",metadata);}let response=response.body(TestBody(rx)).unwrap();
            tokio::spawn(async move {
                if path.ends_with("/Hold"){tokio::time::sleep(Duration::from_secs(5)).await;drop(tx);return;}
                let mut body=request.into_body();let mut pending=BytesMut::new();let mut messages=vec![];
                while let Some(next)=body.frame().await{if let Ok(next)=next{if let Ok(data)=next.into_data(){pending.extend_from_slice(&data);while pending.len()>=5{let length=u32::from_be_bytes(pending[1..5].try_into().unwrap())as usize;if pending.len()<length+5{break;}pending.advance(5);let message=pending.split_to(length).freeze();if path.ends_with("/Bidi"){let _=tx.send(Frame::data(frame(message.clone()).unwrap())).await;}messages.push(message);}}}}
                if path.contains("grpc.reflection.v1."){let mut trailers=hyper::HeaderMap::new();trailers.insert("grpc-status",hyper::header::HeaderValue::from_static("12"));let _=tx.send(Frame::trailers(trailers)).await;return;}
                if path.contains("grpc.reflection.v1alpha."){let request=ReflectionRequest::decode(messages[0].clone()).unwrap();let response=match request.request.unwrap(){reflection_request::MessageRequest::ListServices(_)=>ReflectionResponse{response:Some(reflection_response::Response::Services(ReflectionServices{services:vec![ReflectionService{name:"local.test.EchoService".into()}]}))},_=>ReflectionResponse{response:Some(reflection_response::Response::Files(ReflectionFiles{files:vec![descriptor]}))}};let _=tx.send(Frame::data(frame(response.encode_to_vec().into()).unwrap())).await;
                }else if path.ends_with("/Client"){#[derive(Clone,PartialEq,Message)]struct Echo{#[prost(string,tag="1")]value:String}let echo=Echo{value:messages.len().to_string()};let _=tx.send(Frame::data(frame(echo.encode_to_vec().into()).unwrap())).await;
                }else if !path.ends_with("/Bidi"){for message in messages{let _=tx.send(Frame::data(frame(message.clone()).unwrap())).await;if path.ends_with("/Server"){let _=tx.send(Frame::data(frame(message).unwrap())).await;}}}
                let mut trailers=hyper::HeaderMap::new();trailers.insert("grpc-status",hyper::header::HeaderValue::from_static(if path.ends_with("/Denied"){"7"}else{"0"}));trailers.insert("x-trailer",hyper::header::HeaderValue::from_static("present"));if path.ends_with("/Denied"){trailers.insert("grpc-message",hyper::header::HeaderValue::from_static("permission%20denied%2Btest"));}let _=tx.send(Frame::trailers(trailers)).await;
            });Ok::<_,Infallible>(response)
        }})).await;
                });
            }
        });
        (format!("http://{address}"), task)
    }
    async fn start(url: &str, method: &str, finish: bool) -> String {
        let mut payload = input();
        payload["url"] = json!(url);
        payload["method"] = json!(format!("/local.test.EchoService/{method}"));
        payload["messages"] = json!([{"value":"hello"}]);
        payload["finish"] = json!(finish);
        control("POST", "/grpc-client/start", payload)
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn poll(id: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let state = control(
                    "GET",
                    &format!("/grpc-client/{id}/events?after=0"),
                    Value::Null,
                )
                .await
                .unwrap();
                if wanted(&state) {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    fn received(state: &Value) -> usize {
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "received")
            .count()
    }
    #[test]
    fn schemas_imports_and_json_validation() {
        let input = json!({"root":"api.proto","sources":[{"name":"data.proto","source":"syntax=\"proto3\"; package x; message Data { string name=1; int64 count=2; bytes raw=3; }"},{"name":"api.proto","source":"syntax=\"proto3\"; package x; import \"data.proto\"; service S { rpc Get(Data) returns(Data); }"},{"name":"unused.proto","source":"invalid unrelated definition"}]});
        let pool = compile(&input).unwrap();
        let m = find_method(&pool, "/x.S/Get").unwrap();
        let encoded = encode_json(
            &m,
            &json!({"name":"你好","count":"9007199254740993","raw":"AAEC"}),
        )
        .unwrap();
        let result = decode_json(m.output(), encoded).unwrap();
        assert_eq!(result["count"], "9007199254740993");
        assert_eq!(result["raw"], "AAEC");
        assert!(encode_json(&m, &json!({"unknown":true})).is_err());
        assert!(compile(&json!({"sources":[{"name":"../outside.proto","source":""}]})).is_err());
    }
    #[test]
    fn json_templates_obey_oneof_and_well_known_types() {
        let source = r#"syntax="proto3"; package x; import "google/protobuf/timestamp.proto"; import "google/protobuf/duration.proto"; message Input { oneof choice { string name=1; int32 count=2; } google.protobuf.Timestamp at=3; google.protobuf.Duration elapsed=4; } service S { rpc Get(Input) returns(Input); }"#;
        let pool = compile(&json!({"sources":[{"name":"wkt.proto","source":source}]})).unwrap();
        let method = find_method(&pool, "/x.S/Get").unwrap();
        let template = method_json(&method)["template"].clone();
        assert!(encode_json(&method, &template).is_ok(), "{template}");
        assert!(template.get("count").is_none());
    }
    #[tokio::test]
    async fn unary_and_server_streaming_preserve_metadata_trailers_and_status() {
        let (url, task) = fixture().await;
        for (method, count) in [("Unary", 1), ("Server", 2), ("Denied", 1)] {
            let id = start(&url, method, true).await;
            let state = poll(&id, |s| s["completed"] == true).await;
            assert_eq!(received(&state), count, "{state}");
            assert_eq!(
                state["code"],
                if method == "Denied" { 7 } else { 0 },
                "{state}"
            );
            assert!(state.to_string().contains("local-value"));
            assert!(state.to_string().contains("x-trailer"));
            if method == "Denied" {
                assert!(state.to_string().contains("permission denied+test"));
            }
        }
        task.abort();
    }
    #[tokio::test]
    async fn client_streaming_waits_for_finish_and_bidirectional_delivers_live() {
        let (url, task) = fixture().await;
        for method in ["Client", "Bidi"] {
            let id = start(&url, method, false).await;
            if method == "Bidi" {
                let live = poll(&id, |s| received(s) == 1).await;
                assert_eq!(live["completed"], false);
            }
            control(
                "POST",
                &format!("/grpc-client/{id}/send"),
                json!({"message":{"value":"second"}}),
            )
            .await
            .unwrap();
            if method == "Bidi" {
                let live = poll(&id, |s| received(s) == 2).await;
                assert_eq!(live["completed"], false);
            }
            control("POST", &format!("/grpc-client/{id}/finish"), json!({}))
                .await
                .unwrap();
            let state = poll(&id, |s| s["completed"] == true).await;
            assert_eq!(state["code"], 0, "{state}");
            assert_eq!(received(&state), if method == "Client" { 1 } else { 2 });
            if method == "Client" {
                assert!(
                    state["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|e| e["kind"] == "received" && e["value"]["value"] == "2")
                );
            }
        }
        task.abort();
    }
    #[tokio::test]
    async fn cancellation_deadline_and_reflection_fallback_are_real() {
        let (url, task) = fixture().await;
        let id = start(&url, "Hold", true).await;
        control("POST", &format!("/grpc-client/{id}/cancel"), json!({}))
            .await
            .unwrap();
        let cancelled = poll(&id, |s| s["completed"] == true).await;
        assert_eq!(cancelled["code"], 1);
        let mut payload = input();
        payload["url"] = json!(&url);
        payload["method"] = json!("/local.test.EchoService/Hold");
        payload["messages"] = json!([{}]);
        payload["timeout"] = json!(0.1);
        let id = control("POST", "/grpc-client/start", payload)
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(poll(&id, |s| s["completed"] == true).await["code"], 4);
        let reflected = control(
            "POST",
            "/grpc-client/schema",
            json!({"reflection":true,"url":url}),
        )
        .await
        .unwrap();
        assert_eq!(
            reflected["services"][0]["methods"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        task.abort();
    }
}
