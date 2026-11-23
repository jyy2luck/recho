#[path = "native.rs"]
pub mod native;

use std::{
    collections::BTreeMap,
    convert::Infallible,
    env,
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use base64::Engine as _;
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response,
    body::{Body as HttpBody, Frame, Incoming, SizeHint},
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::{Instant, Sleep},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    certificates::{Authority, client_config},
    rules::{Rule, Rules},
    settings::{GatewayDecision, Settings, SettingsStore},
    storage::{History, Recording, private_dir},
    wire::{self, Error, RequestData, ResponseData, Result, from_headers, header_text, parse_url},
};

type Body = UnsyncBoxBody<Bytes, Error>;
type Reply = Response<Body>;
type Record = Arc<Mutex<Recording>>;
enum UpstreamSender {
    Http1(hyper::client::conn::http1::SendRequest<Body>),
    Http2(hyper::client::conn::http2::SendRequest<Body>),
}
impl UpstreamSender {
    async fn send(
        &mut self,
        request: Request<Body>,
    ) -> std::result::Result<Response<Incoming>, hyper::Error> {
        match self {
            Self::Http1(sender) => sender.send_request(request).await,
            Self::Http2(sender) => sender.send_request(request).await,
        }
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;

fn capture_headers(headers: &hyper::HeaderMap) -> wire::Headers {
    let mut headers = from_headers(headers);
    headers.remove("proxy-authorization");
    headers
}
fn format_authority(host: &str, port: u16, scheme: &str) -> String {
    if (scheme == "http" && port == 80) || (scheme == "https" && port == 443) {
        host.into()
    } else {
        format!("{host}:{port}")
    }
}
fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
struct BufferedBody {
    bytes: Option<Bytes>,
    trailers: Option<hyper::HeaderMap>,
}
impl HttpBody for BufferedBody {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>>>> {
        if let Some(bytes) = self.bytes.take() {
            if !bytes.is_empty() {
                return Poll::Ready(Some(Ok(Frame::data(bytes))));
            }
        }
        Poll::Ready(
            self.trailers
                .take()
                .map(|headers| Ok(Frame::trailers(headers))),
        )
    }
    fn is_end_stream(&self) -> bool {
        self.bytes.is_none() && self.trailers.is_none()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.bytes.as_ref().map_or(0, |b| b.len()) as u64)
    }
}
fn buffered(bytes: Bytes, trailers: &str) -> Result<Body> {
    if trailers.is_empty() {
        return Ok(full(bytes));
    }
    Ok(BufferedBody {
        bytes: Some(bytes),
        trailers: Some(wire::forward_headers(&wire::parse_headers(trailers)?)?),
    }
    .boxed_unsync())
}
fn json_reply(status: u16, value: Value) -> Reply {
    let bytes = serde_json::to_vec(&value).unwrap();
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .header("cache-control", "no-store")
        .body(full(bytes))
        .unwrap()
}
fn error_reply(error: Error) -> Reply {
    let mut response = json_reply(error.status, json!({"error": error.message}));
    response.headers_mut().insert(
        "connection",
        hyper::header::HeaderValue::from_static("close"),
    );
    response
}

struct Config {
    proxy_host: std::net::Ipv4Addr,
    token: String,
    directory: PathBuf,
    port: Option<u16>,
    preview: usize,
    request: usize,
    sessions: usize,
    timeout: Duration,
    breakpoint_ms: u64,
    script_ms: u64,
    extra_ca: Option<PathBuf>,
}
fn number(name: &str, default: usize, zero: bool) -> Result<usize> {
    let value = env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<usize>()
        .map_err(Error::bad)?;
    if (!zero && value == 0) || value > i64::MAX as usize {
        return Err(Error::bad(format!("Invalid {name}")));
    }
    Ok(value)
}
impl Config {
    fn embedded(directory: PathBuf, port: Option<u16>, token: String) -> Result<Self> {
        if !directory.is_absolute() {
            return Err(Error::bad("The engine data directory must be absolute"));
        }
        Ok(Self {
            proxy_host: if cfg!(any(
                target_os = "windows",
                target_os = "macos",
                target_os = "linux"
            )) {
                std::net::Ipv4Addr::UNSPECIFIED
            } else {
                std::net::Ipv4Addr::LOCALHOST
            },
            token,
            directory,
            port,
            preview: 64 * 1024,
            request: 16 * 1024 * 1024,
            sessions: 200,
            timeout: Duration::from_secs(30),
            breakpoint_ms: 60_000,
            script_ms: 100,
            extra_ca: None,
        })
    }
    fn load() -> Result<Self> {
        let token = env::var("RECHO_CONTROL_TOKEN").unwrap_or_default();
        if token.is_empty() {
            return Err(Error::bad("RECHO_CONTROL_TOKEN is required"));
        }
        let directory = match env::var_os("RECHO_DATA_DIR") {
            Some(value) => PathBuf::from(value),
            None => PathBuf::from(
                env::var_os(if cfg!(windows) {
                    "LOCALAPPDATA"
                } else {
                    "HOME"
                })
                .ok_or_else(|| Error::bad("HOME or RECHO_DATA_DIR is required"))?,
            )
            .join(crate::identity::DATA_DIRECTORY),
        };
        Ok(Self {
            proxy_host: std::net::Ipv4Addr::UNSPECIFIED,
            token,
            directory,
            port: env::var_os("RECHO_PROXY_PORT")
                .map(|_| u16::try_from(number("RECHO_PROXY_PORT", 9988, true)?).map_err(Error::bad))
                .transpose()?,
            preview: number("RECHO_MAX_PREVIEW_BYTES", 64 * 1024, false)?,
            request: number("RECHO_MAX_REQUEST_BYTES", 16 * 1024 * 1024, false)?,
            sessions: number("RECHO_MAX_SESSIONS", 200, false)?,
            timeout: Duration::from_millis(
                number("RECHO_REQUEST_TIMEOUT_MS", 30_000, false)? as u64
            ),
            breakpoint_ms: number("RECHO_BREAKPOINT_TIMEOUT_MS", 60_000, false)? as u64,
            script_ms: number("RECHO_SCRIPT_TIMEOUT_MS", 100, false)? as u64,
            extra_ca: env::var_os("RECHO_UPSTREAM_CA").map(PathBuf::from),
        })
    }
}

// One idle deadline covers both directions, including stalled streaming bodies.
struct IdleIo<T> {
    inner: T,
    timer: Pin<Box<Sleep>>,
    timeout: Duration,
}
impl<T> IdleIo<T> {
    fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timer: Box::pin(tokio::time::sleep(timeout)),
            timeout,
        }
    }
    fn reset(&mut self) {
        self.timer.as_mut().reset(Instant::now() + self.timeout);
    }
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.timer.as_mut().poll(cx).is_ready()
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for IdleIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.expired(cx) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Network idle timeout",
            )));
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.reset();
        }
        result
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for IdleIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.expired(cx) {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Network idle timeout",
            )));
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            self.reset();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// Response preparation and breakpoints may be idle; only a stalled write times out here.
struct DownstreamIo<T> {
    inner: T,
    timer: Option<Pin<Box<Sleep>>>,
    timeout: Duration,
}
impl<T> DownstreamIo<T> {
    fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timer: None,
            timeout,
        }
    }
    fn writing<R>(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<std::io::Result<R>>,
    ) -> Poll<std::io::Result<R>> {
        if result.is_ready() {
            self.timer = None;
            return result;
        }
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.timeout)));
        if timer.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Client write idle timeout",
            )))
        } else {
            Poll::Pending
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for DownstreamIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for DownstreamIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.writing(cx, result)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.writing(cx, result)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.writing(cx, result)
    }
}

struct Driver(JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct FinishGuard {
    record: Record,
    armed: bool,
}
impl FinishGuard {
    fn new(record: Record) -> Self {
        Self {
            record,
            armed: true,
        }
    }
}
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if self.armed {
            self.record.lock().unwrap().finish(Some(
                "Client disconnected before the request completed".into(),
            ));
        }
    }
}
struct ObservedBody {
    inner: Incoming,
    record: Record,
    response: bool,
    limit: usize,
    finish: Option<FinishGuard>,
    driver: Option<Driver>,
    permit: Option<OwnedSemaphorePermit>,
    timer: Option<Pin<Box<Sleep>>>,
    timeout: Duration,
}
impl ObservedBody {
    fn complete(&mut self, error: Option<String>) {
        if self.response {
            self.record.lock().unwrap().finish(error);
        } else if error.is_none() {
            let mut record = self.record.lock().unwrap();
            if record.match_list_filters() {
                record.publish();
            }
        }
        if let Some(mut guard) = self.finish.take() {
            guard.armed = false;
        }
        self.driver.take();
        self.permit.take();
    }
}
impl HttpBody for ObservedBody {
    type Data = Bytes;
    type Error = Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                self.timer = None;
                if let Some(bytes) = frame.data_ref() {
                    let mut record = self.record.lock().unwrap();
                    if self.response {
                        record.response.add(bytes);
                    } else {
                        record.request.add(bytes);
                        if record.request.length > self.limit {
                            return Poll::Ready(Some(Err(Error::new(
                                413,
                                format!("Request body exceeds {} bytes", self.limit),
                            ))));
                        }
                    }
                }
                if let Some(trailers) = frame.trailers_ref() {
                    let mut record = self.record.lock().unwrap();
                    let text = header_text(&from_headers(trailers));
                    if self.response {
                        record.session.response_trailers = text;
                    } else {
                        record.session.request_trailers = text;
                    }
                }
                // Hyper may stop polling once a length-delimited body's last frame
                // advertises end-of-stream, without ever asking for a None frame.
                if self.inner.is_end_stream() {
                    self.complete(None);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                let error = wire::network_error(error);
                self.complete(Some(error.message.clone()));
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.complete(None);
                Poll::Ready(None)
            }
            Poll::Pending => {
                let timeout = self.timeout;
                let timer = self
                    .timer
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                if timer.as_mut().poll(cx).is_ready() {
                    let error = Error::new(504, "Body read idle timeout");
                    self.complete(Some(error.message.clone()));
                    Poll::Ready(Some(Err(error)))
                } else {
                    Poll::Pending
                }
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

async fn read_body<B: HttpBody<Data = Bytes> + Unpin>(
    body: &mut B,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes>
where
    B::Error: std::fmt::Display,
{
    Ok(read_body_with_trailers(body, limit, timeout).await?.0)
}
async fn read_body_with_trailers<B: HttpBody<Data = Bytes> + Unpin>(
    body: &mut B,
    limit: usize,
    timeout: Duration,
) -> Result<(Bytes, wire::Headers)>
where
    B::Error: std::fmt::Display,
{
    let mut bytes = BytesMut::new();
    let mut trailers = wire::Headers::new();
    while let Some(frame) = tokio::time::timeout(timeout, body.frame())
        .await
        .map_err(|_| Error::new(504, "Body read idle timeout"))?
    {
        let frame = frame.map_err(Error::upstream)?;
        if let Some(headers) = frame.trailers_ref() {
            trailers = from_headers(headers);
        }
        if let Some(data) = frame.data_ref() {
            if data.len() > limit.saturating_sub(bytes.len()) {
                return Err(Error::new(
                    413,
                    format!("Body buffering for rules exceeds {limit} bytes"),
                ));
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok((bytes.freeze(), trailers))
}

struct ReverseListener {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
struct Proxy {
    port: u16,
    running: bool,
    intercept: bool,
    cancel: CancellationToken,
    listener: Option<JoinHandle<()>>,
}
struct ApiExecution {
    python_script: String,
    shared: Value,
    version: String,
    after_script: String,
    variables: Value,
    logs: Vec<String>,
    tests: Vec<Value>,
}

pub(crate) struct Engine {
    config: Config,
    mcp_tokens: Mutex<crate::mcp::AccessTokens>,
    authority: Authority,
    client_tls: Arc<rustls::ClientConfig>,
    history: Arc<Mutex<History>>,
    rules: Arc<Rules>,
    settings: SettingsStore,
    ui: Arc<crate::ui_bridge::UIBridge>,
    settings_change: tokio::sync::Mutex<()>,
    reverse_listeners: tokio::sync::Mutex<BTreeMap<String, ReverseListener>>,
    proxy: tokio::sync::Mutex<Proxy>,
    requests: Arc<Semaphore>,
    tunnels: Arc<Semaphore>,
    socks_udp: Arc<Semaphore>,
    shutdown: CancellationToken,
    tasks: TaskTracker,
    remote_mcp: Mutex<Option<(u16, CancellationToken)>>,
}
impl Engine {
    async fn configure_proxy(self: &Arc<Self>, input: &Value) -> Result<()> {
        {
            let _serial = self.settings_change.lock().await;
            let mut proxy = self.proxy.lock().await;
            let port = input["port"].as_u64().map(|port| port as u16);
            let selected_port = port.unwrap_or(proxy.port);
            let running = input["running"].as_bool().unwrap_or(proxy.running);
            let intercept = input["interceptTLS"].as_bool().unwrap_or(proxy.intercept);
            let prepared = if selected_port != proxy.port {
                Some(Self::bind_proxy(selected_port, self.config.proxy_host).await?)
            } else {
                None
            };
            let actual_port = prepared
                .as_ref()
                .map(|listener| listener.local_addr())
                .transpose()?
                .map(|address| address.port())
                .unwrap_or(proxy.port);
            // Commit disk before any visible state or old listener
            // changes. Dropping a candidate on failure rolls it back.
            if port.is_some() || input.get("interceptTLS").is_some() {
                let mut patch = json!({});
                if let Some(port) = port {
                    patch["proxyPort"] = json!(port);
                }
                if input.get("interceptTLS").is_some() {
                    patch["interceptTLS"] = json!(intercept);
                }
                let next = self.settings.updated(patch)?;
                self.settings.save(next)?;
            }
            let previous = if prepared.is_some() {
                proxy.listener.take()
            } else {
                None
            };
            if let Some(listener) = &previous {
                listener.abort();
            }
            if running && !proxy.running {
                proxy.cancel = self.shutdown.child_token();
            } else if !running {
                proxy.cancel.cancel();
            }
            proxy.running = running;
            if let Some(listener) = prepared {
                // Rebinding keeps in-flight traffic until capture is paused.
                self.activate_proxy(&mut proxy, listener, actual_port);
            }
            proxy.intercept = intercept;
            // Publish the complete transaction before yielding, even
            // if a control client disconnects during listener cleanup.
            if let Some(listener) = previous {
                let _ = listener.await;
            }
        }
        Ok(())
    }
    pub(crate) async fn vpn_tcp(
        self: Arc<Self>,
        socket: TcpStream,
        destination: crate::android_vpn::Destination,
        cancel: CancellationToken,
    ) -> Result<()> {
        let traffic = {
            let proxy = self.proxy.lock().await;
            if !proxy.running {
                return Err(Error::new(503, "Capture is paused"));
            }
            proxy.cancel.clone()
        };
        let operation = async {
            let mut prefix = [0; 65536];
            let mut length = tokio::time::timeout(Duration::from_secs(1), socket.peek(&mut prefix))
                .await
                .unwrap_or(Ok(0))?;
            let methods = [
                b"GET ".as_slice(),
                b"POST ",
                b"PUT ",
                b"DELETE ",
                b"HEAD ",
                b"OPTIONS ",
                b"PATCH ",
                b"CONNECT ",
                b"TRACE ",
                b"PRI ",
            ];
            let deadline = Instant::now() + Duration::from_secs(10);
            while length > 0
                && methods
                    .iter()
                    .any(|method| method.len() > length && method.starts_with(&prefix[..length]))
            {
                if Instant::now() >= deadline {
                    return Err(Error::bad("HTTP request prefix timed out"));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                length = socket.peek(&mut prefix).await?;
            }
            let authority = if destination.host.contains(':') {
                format!("[{}]:{}", destination.host, destination.port)
            } else {
                format!("{}:{}", destination.host, destination.port)
            };
            if methods
                .iter()
                .any(|method| prefix[..length].starts_with(method))
            {
                let target = parse_url(&format!("http://{authority}"))?;
                let client_ip = socket.peer_addr().ok().map(|a| a.ip());
                self.clone()
                    .serve(
                        Box::new(socket),
                        false,
                        None,
                        client_ip,
                        Some(target),
                        None,
                        traffic.clone(),
                        None,
                    )
                    .await;
            } else if length > 0 && prefix[0] == 22 {
                let mut host = destination.host;
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let size = socket.peek(&mut prefix).await?;
                    if size == 0 {
                        return Err(Error::bad("Client closed during TLS handshake"));
                    }
                    let mut acceptor = rustls::server::Acceptor::default();
                    acceptor
                        .read_tls(&mut std::io::Cursor::new(&prefix[..size]))
                        .map_err(Error::upstream)?;
                    match acceptor.accept() {
                        Ok(Some(hello)) => {
                            if let Some(name) = hello.client_hello().server_name() {
                                host = name.to_string();
                            }
                            break;
                        }
                        Err(_) => break,
                        Ok(None) if size == prefix.len() || Instant::now() >= deadline => {
                            return Err(Error::bad(
                                "TLS ClientHello exceeds VPN limit or timed out",
                            ));
                        }
                        Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                    }
                }
                let authority = if host.contains(':') {
                    format!("[{host}]:{}", destination.port)
                } else {
                    format!("{host}:{}", destination.port)
                };
                // shortcut: TLS upstream resolves SNI again, preserve app-selected IPs when adding destination routing.
                let port = self.proxy.lock().await.port;
                let mut proxy = TcpStream::connect(("127.0.0.1", port)).await?;
                proxy
                    .write_all(
                        format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n")
                            .as_bytes(),
                    )
                    .await?;
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    if head.len() >= 32768 {
                        return Err(Error::upstream("VPN CONNECT response too large"));
                    }
                    head.push(proxy.read_u8().await?);
                }
                if !head.starts_with(b"HTTP/1.1 200 ") {
                    return Err(Error::upstream("VPN CONNECT rejected"));
                }
                tokio::io::copy_bidirectional(
                    &mut IdleIo::new(socket, self.config.timeout),
                    &mut IdleIo::new(proxy, self.config.timeout),
                )
                .await?;
            } else {
                let target = parse_url(&format!("https://{authority}"))?;
                let settings = self.settings.snapshot();
                let policy = self
                    .gateway_policy(&settings, &target, true, "CONNECT", false)
                    .await;
                if policy.block_request {
                    return Err(Error::new(403, "Blocked by gateway L4 rule"));
                }
                let mut upstream = self
                    .tunnel_transport(
                        &destination.host,
                        destination.port,
                        settings
                            .upstream(&destination.host, destination.port)
                            .as_ref(),
                    )
                    .await?;
                tokio::io::copy_bidirectional(
                    &mut IdleIo::new(socket, self.config.timeout),
                    &mut IdleIo::new(&mut upstream, self.config.timeout),
                )
                .await?;
            }
            Ok(())
        };
        tokio::select! {
            _ = cancel.cancelled() => Ok(()),
            _ = traffic.cancelled() => Ok(()),
            result = operation => result,
        }
    }

    fn mcp_access(&self) -> Value {
        json!({"tokens":self.mcp_tokens.lock().unwrap().items,"remoteEnabled":self.settings.snapshot().mcp_remote_enabled,"remotePort":self.remote_mcp.lock().unwrap().as_ref().map(|v|v.0),"addresses":Self::local_proxy_addresses().unwrap_or_default()})
    }
    fn activate_remote_mcp(self: &Arc<Self>, listener: Option<TcpListener>) -> Result<()> {
        if let Some((_, cancel)) = self.remote_mcp.lock().unwrap().take() {
            cancel.cancel();
        }
        if let Some(listener) = listener {
            let port = listener.local_addr()?.port();
            let cancel = self.shutdown.child_token();
            *self.remote_mcp.lock().unwrap() = Some((port, cancel.clone()));
            let engine = self.clone();
            self.tasks.spawn(async move {
                loop {
                    let accepted=tokio::select!{_=cancel.cancelled()=>break,result=listener.accept()=>result};
                    let Ok((socket,peer))=accepted else {break};
                    let Ok(permit)=engine.requests.clone().try_acquire_owned() else {continue};
                    let connection_engine=engine.clone();let connection_cancel=cancel.clone();
                    engine.tasks.spawn(async move {
                        let _permit=permit;
                        let service_engine=connection_engine.clone();
                        let service=service_fn(move|request:Request<Incoming>|{let engine=service_engine.clone();async move{Ok::<_,Infallible>(if request.uri().path()=="/mcp"{engine.mcp_http(request,peer.ip()).await}else{error_reply(Error::new(404,"MCP endpoint only"))})}});
                        let io=TokioIo::new(IdleIo::new(socket,connection_engine.config.timeout));
                        let connection=hyper::server::conn::http1::Builder::new().serve_connection(io,service);
                        tokio::select!{_=connection_cancel.cancelled()=>{},_=connection=>{}}
                    });
                }
            });
        }
        Ok(())
    }
    async fn bind_proxy(port: u16, host: std::net::Ipv4Addr) -> Result<TcpListener> {
        let failure = |error: std::io::Error| {
            Error::new(
                503,
                format!("Cannot listen on proxy address {host}:{port}: {error}"),
            )
        };
        let socket = tokio::net::TcpSocket::new_v4().map_err(failure)?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::{
                SO_EXCLUSIVEADDRUSE, SOL_SOCKET, WSAGetLastError, setsockopt,
            };
            let exclusive = 1_i32;
            // Winsock otherwise permits wildcard/specific-address overlap even without reuse.
            if unsafe {
                setsockopt(
                    socket.as_raw_socket() as _,
                    SOL_SOCKET,
                    SO_EXCLUSIVEADDRUSE,
                    (&exclusive as *const i32).cast(),
                    std::mem::size_of_val(&exclusive) as i32,
                )
            } != 0
            {
                return Err(failure(std::io::Error::from_raw_os_error(unsafe {
                    WSAGetLastError()
                })));
            }
        }
        socket.set_reuseaddr(false).map_err(failure)?;
        let address = (host, port).into();
        #[cfg(windows)]
        socket.bind(address).map_err(failure)?;
        #[cfg(unix)]
        {
            if let Err(error) = socket.bind(address) {
                if error.kind() != std::io::ErrorKind::AddrInUse {
                    return Err(failure(error));
                }
                // A recently closed connection can leave TIME_WAIT sockets behind.
                // Reuse supports an immediate pause/resume, but on macOS it also
                // permits wildcard/specific-address listeners to overlap. Probe the
                // machine's IPv4 addresses before accepting that fallback.
                socket.set_reuseaddr(true).map_err(failure)?;
                socket.bind(address).map_err(failure)?;
                for host in Self::local_proxy_addresses().map_err(failure)? {
                    if tokio::time::timeout(
                        Duration::from_millis(150),
                        TcpStream::connect((host, port)),
                    )
                    .await
                    .is_ok_and(|result| result.is_ok())
                    {
                        return Err(Error::new(
                            503,
                            format!("Proxy port {port} is already in use on {host}"),
                        ));
                    }
                }
            } else {
                // Future rebinds may reuse sockets after their connections close.
                socket.set_reuseaddr(true).map_err(failure)?;
            }
        }
        socket.listen(1024).map_err(failure)
    }
    fn local_proxy_addresses() -> std::io::Result<Vec<std::net::Ipv4Addr>> {
        let mut addresses = vec![std::net::Ipv4Addr::LOCALHOST];
        for interface in if_addrs::get_if_addrs()? {
            if let std::net::IpAddr::V4(address) = interface.ip() {
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
        }
        Ok(addresses)
    }
    fn activate_proxy(self: &Arc<Self>, proxy: &mut Proxy, listener: TcpListener, port: u16) {
        proxy.port = port;
        // The listener also serves MCP while capture is paused.
        let cancel = self.shutdown.clone();
        let engine = self.clone();
        proxy.listener = Some(tokio::spawn(async move {
            engine.accept(listener, false, cancel).await;
        }));
    }
    async fn stop_proxy(proxy: &mut Proxy) {
        proxy.running = false;
        proxy.cancel.cancel();
        if let Some(listener) = proxy.listener.take() {
            listener.abort();
            let _ = listener.await;
        }
    }
    async fn prepare_reverse(&self, settings: &Settings) -> Result<Vec<(String, TcpListener)>> {
        let current = self.reverse_listeners.lock().await;
        let mut prepared = Vec::new();
        if settings.reverse_enabled {
            for rule in settings.reverse_proxies.iter().filter(|r| r.enabled) {
                let key = format!("{}:{}", rule.listen_host, rule.port);
                if !current.contains_key(&key) {
                    let listener = TcpListener::bind((rule.listen_host.as_str(), rule.port))
                        .await
                        .map_err(|error| {
                            Error::new(
                                409,
                                format!(
                                    "Cannot start reverse proxy {} at {key}: {error}",
                                    rule.name
                                ),
                            )
                        })?;
                    prepared.push((key, listener));
                }
            }
        }
        Ok(prepared)
    }
    async fn activate_reverse(
        self: &Arc<Self>,
        settings: &Settings,
        prepared: Vec<(String, TcpListener)>,
    ) {
        let mut current = self.reverse_listeners.lock().await;
        let removed: Vec<_> = current
            .keys()
            .filter(|key| {
                !settings.reverse_enabled
                    || !settings
                        .reverse_proxies
                        .iter()
                        .any(|r| r.enabled && **key == format!("{}:{}", r.listen_host, r.port))
            })
            .cloned()
            .collect();
        for key in removed {
            if let Some(listener) = current.remove(&key) {
                listener.cancel.cancel();
                let _ = listener.task.await;
            }
        }
        for (key, listener) in prepared {
            let cancel = self.shutdown.child_token();
            let engine = self.clone();
            let listener_cancel = cancel.clone();
            let listener_key = key.clone();
            let task=self.tasks.spawn(async move {
                loop {tokio::select! {
                    _=cancel.cancelled()=>break,
                    incoming=listener.accept()=>match incoming {
                        Ok((socket,peer))=>{
                            let settings=engine.settings.snapshot();if !settings.permits(peer.ip()){drop(socket);continue;}
                            let Some(rule)=settings.reverse_proxies.iter().find(|r|r.enabled && key==format!("{}:{}",r.listen_host,r.port)) else {drop(socket);continue};
                            let preserve_host=rule.preserve_host;
                            let target=match parse_url(&rule.target){Ok(url)=>url,Err(_)=>{drop(socket);continue}};
                            let engine=engine.clone();let cancel=cancel.clone();
                            engine.tasks.clone().spawn(async move {let application = match socket.local_addr() { Ok(local) => crate::application::identify(peer, local).await, Err(_) => None };engine.serve(Box::new(socket),false,None,Some(peer.ip()),Some(target),Some(preserve_host),cancel,application).await;});
                        },Err(_)=>break
                    }
                }}
            });
            current.insert(
                listener_key,
                ReverseListener {
                    cancel: listener_cancel,
                    task,
                },
            );
        }
    }
    async fn state(&self, known: Option<&str>, since: Option<u64>, cursor: Option<&str>) -> Reply {
        let (proxy_host, proxy_hosts) = if self.config.proxy_host.is_unspecified() {
            crate::local_network::proxy_addresses()
        } else {
            (self.config.proxy_host, vec![self.config.proxy_host])
        };
        let proxy = self.proxy.lock().await;
        let history = self.history.lock().unwrap();
        let since = match cursor {
            Some(cursor) => history.cursor_revision(cursor),
            None => since.filter(|value| *value <= history.revision),
        };
        let mut state = json!({"running": proxy.running, "proxyHost": proxy_host, "proxyHosts": proxy_hosts, "proxyPort": proxy.port, "interceptTLS": proxy.intercept, "caPath": self.authority.ca_path, "sessionsRevision": history.revision, "sessionCursor": history.cursor(), "captureListFilters": history.capture_list_filters.items, "limits": {"maxSessions": self.config.sessions, "maxPreviewBytes": self.config.preview, "maxRequestBytes": self.config.request, "maxConcurrentRequests": 100, "idleTimeoutMs": self.config.timeout.as_millis()}, "historyPersistent": !self.settings.snapshot().incognito, "historyError": history.error, "settings": self.settings.get()});
        let mut hash = DefaultHasher::new();
        serde_json::to_vec(&state).unwrap().hash(&mut hash);
        let tag = format!("W/\"{:x}\"", hash.finish());
        // Revalidate before copying or serializing live body previews.
        let unchanged = known.is_some_and(|known| {
            known.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || candidate.trim_start_matches("W/") == &tag[2..]
            })
        });
        let mut response = if unchanged {
            Response::builder()
                .status(304)
                .body(full(Bytes::new()))
                .unwrap()
        } else {
            let sessions = match since {
                Some(revision) => history.sessions_since(revision),
                None => history.sessions.iter().collect(),
            };
            state["sessions"] = json!(sessions);
            state["sessionsDelta"] = json!(since.is_some());
            state["sessionIDs"] = json!(history.sessions.iter().map(|s| &s.id).collect::<Vec<_>>());
            json_reply(200, state)
        };
        response.headers_mut().insert("etag", tag.parse().unwrap());
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().unwrap());
        response
    }
    async fn accept(
        self: Arc<Self>,
        listener: TcpListener,
        control: bool,
        cancel: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((socket, peer)) => {
                        if !control && !self.settings.snapshot().permits(peer.ip()) {drop(socket);continue;}
                        let engine = self.clone();
                        let cancel = cancel.clone();
                        self.tasks.spawn(async move {
                            if !control {
                                let mut prefix = [0; 1];
                                let first = tokio::select! {
                                    _ = cancel.cancelled() => return,
                                    result = tokio::time::timeout(engine.config.timeout, socket.peek(&mut prefix)) => result,
                                };
                                if let Ok(Ok(1)) = first {
                                    if prefix[0] == 5 {
                                        // System SOCKS clients use the same local port as HTTP.
                                        // Keep UDP associations local and bound the shared pools.
                                        if !peer.ip().is_loopback() { return; }
                                        let Ok(_permit) = engine.tunnels.clone().try_acquire_owned() else { return; };
                                        let traffic = {
                                            let proxy = engine.proxy.lock().await;
                                            if !proxy.running { return; }
                                            proxy.cancel.clone()
                                        };
                                        let udp = engine.socks_udp.clone();
                                        crate::android_vpn::serve_connection(socket, engine, traffic, udp).await;
                                        return;
                                    }
                                } else { return; }
                            }
                            let application = if control { None } else { match socket.local_addr() { Ok(local) => crate::application::identify(peer, local).await, Err(_) => None } };
                            engine.serve(Box::new(socket), control, (!control).then_some(peer.ip()), (!control).then_some(peer.ip()), None, None, cancel, application).await;
                        });
                    }
                    Err(error) => { eprintln!("Local listener failed: {error}"); break; }
                }
            }
        }
    }
    fn serve(
        self: Arc<Self>,
        socket: BoxIo,
        control: bool,
        mcp_peer: Option<std::net::IpAddr>,
        client_ip: Option<std::net::IpAddr>,
        target: Option<url::Url>,
        reverse: Option<bool>,
        cancel: CancellationToken,
        application: Option<crate::application::Application>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let timeout = self.config.timeout;
            let service_cancel = cancel.clone();
            let (traffic_sender, traffic_receiver) =
                tokio::sync::oneshot::channel::<CancellationToken>();
            let traffic_sender = Arc::new(Mutex::new(Some(traffic_sender)));
            let service = service_fn(move |mut request: Request<Incoming>| {
                if let Some(ip) = client_ip {
                    request.extensions_mut().insert(ip);
                }
                let engine = self.clone();
                let target = target.clone();
                let cancel = service_cancel.clone();
                let application = application.clone();
                let traffic_sender = traffic_sender.clone();
                async move {
                    let response = if control {
                        engine.control(request).await
                    } else if let Some(peer) = mcp_peer {
                        let local_uri =
                            request.uri().scheme().is_none()
                                || (request.uri().host().is_some_and(|host| {
                                    host == "127.0.0.1" || host == "localhost"
                                }) && request.uri().port_u16()
                                    == Some(engine.proxy.lock().await.port));
                        if local_uri && request.uri().path() == "/mcp" {
                            engine.mcp_http(request, peer).await
                        } else {
                            let proxy = engine.proxy.lock().await;
                            if !proxy.running {
                                return Ok::<_, Infallible>(error_reply(Error::new(
                                    503,
                                    "Capture is paused",
                                )));
                            }
                            let cancel = proxy.cancel.clone();
                            // Ordinary connections still close on pause, including streaming bodies.
                            if let Some(sender) = traffic_sender.lock().unwrap().take() {
                                let _ = sender.send(cancel.clone());
                            }
                            drop(proxy);
                            engine
                                .proxy_request(request, target, reverse, cancel, application)
                                .await
                        }
                    } else {
                        engine
                            .proxy_request(request, target, reverse, cancel, application)
                            .await
                    };
                    Ok::<_, Infallible>(response)
                }
            });
            let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(timeout.min(Duration::from_secs(10)))
                .max_buf_size(32 * 1024);
            let connection = builder.serve_connection_with_upgrades(
                TokioIo::new(DownstreamIo::new(socket, timeout)),
                service,
            );
            let traffic_stopped = async {
                if let Ok(cancel) = traffic_receiver.await {
                    cancel.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! { _ = cancel.cancelled() => {}, _ = traffic_stopped => {}, _ = connection => {} }
        })
    }
    async fn mcp_http(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        peer: std::net::IpAddr,
    ) -> Reply {
        let result = async {
            let settings = self.settings.snapshot();
            let remote = settings.mcp_remote_enabled;
            if !settings.permits(peer) {
                return Err(Error::new(403, "MCP client blocked by access control"));
            }
            if !peer.is_loopback() && !remote {
                return Err(Error::new(403, "Remote MCP access is disabled"));
            }
            let mut hosts = vec!["127.0.0.1".to_string(), "localhost".to_string()];
            if remote {
                hosts.extend(
                    Self::local_proxy_addresses()?
                        .into_iter()
                        .map(|ip| ip.to_string()),
                );
            }
            let host = request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let authority = host
                .parse::<hyper::http::uri::Authority>()
                .map_err(|_| Error::new(403, "Invalid MCP host"))?;
            // Allow a forwarded port (for example SSH -L) while keeping the host allowlist.
            if authority.port_u16() == Some(0)
                || !hosts
                    .iter()
                    .any(|allowed| authority.host().eq_ignore_ascii_case(allowed))
            {
                return Err(Error::new(403, "Invalid MCP host"));
            }
            for origin in request.headers().get_all("origin") {
                let origin = origin.to_str().unwrap_or("");
                if origin != format!("http://{host}") {
                    return Err(Error::new(403, "Foreign MCP origin"));
                }
            }
            let supplied = request
                .headers()
                .get("authorization")
                .map(|v| v.as_bytes())
                .unwrap_or_default();
            if !self.mcp_tokens.lock().unwrap().authorized(supplied) {
                return Err(Error::new(401, "Unauthorized MCP token"));
            }
            let mut scope = crate::mcp::Scope::All;
            let mut scope_seen = false;
            for (key, value) in
                url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            {
                if key == "scope" {
                    if scope_seen {
                        return Err(Error::bad("Duplicate MCP scope"));
                    }
                    scope = crate::mcp::Scope::parse(&value)?;
                    scope_seen = true;
                }
            }
            if request.method() != hyper::Method::POST {
                let mut response = error_reply(Error::new(405, "Stateless MCP only accepts POST"));
                response
                    .headers_mut()
                    .insert("allow", hyper::header::HeaderValue::from_static("POST"));
                return Ok(response);
            }
            if let Some(version) = request.headers().get("mcp-protocol-version") {
                if !version
                    .to_str()
                    .is_ok_and(|version| crate::mcp::HTTP_PROTOCOLS.contains(&version))
                {
                    return Err(Error::bad("Unsupported MCP protocol version"));
                }
            }
            let content_type = request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
            {
                return Err(Error::new(415, "Content-Type must be application/json"));
            }
            let bytes = read_body(
                request.body_mut(),
                crate::mcp::MAX_MESSAGE as usize,
                self.config.timeout,
            )
            .await?;
            let mut message: Value = match serde_json::from_slice(&bytes) {
                Ok(message) => message,
                Err(_) => {
                    return Ok(json_reply(
                        400,
                        crate::mcp::rpc_error(Value::Null, -32700, "Parse error"),
                    ));
                }
            };
            // The legacy stdio version predates Streamable HTTP; negotiate a supported version.
            if message["method"] == "initialize"
                && message["params"]["protocolVersion"] == "2024-11-05"
            {
                message["params"]["protocolVersion"] =
                    json!(crate::mcp::HTTP_PROTOCOLS.last().unwrap());
            }
            match crate::mcp::dispatch(message, scope, &self.config.directory).await {
                Some(value) => {
                    let status = if value["error"]["code"] == -32600 {
                        400
                    } else {
                        200
                    };
                    Ok(json_reply(status, value))
                }
                None => Ok(Response::builder()
                    .status(202)
                    .header("cache-control", "no-store")
                    .body(full(Bytes::new()))
                    .unwrap()),
            }
        }
        .await;
        result.unwrap_or_else(error_reply)
    }
    async fn control(self: Arc<Self>, request: Request<Incoming>) -> Reply {
        let supplied = request
            .headers()
            .get("authorization")
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        let expected = format!("Bearer {}", self.config.token);
        if !bool::from(supplied.ct_eq(expected.as_bytes())) {
            return error_reply(Error::new(401, "Unauthorized"));
        }
        self.control_local(request).await
    }
    async fn control_local<B>(self: Arc<Self>, mut request: Request<B>) -> Reply
    where
        B: HttpBody<Data = Bytes> + Unpin,
        B::Error: std::fmt::Display,
    {
        let result = async {
            let path = request.uri().path().to_string();
            let verb = request.method().as_str().to_string();
            match (verb.as_str(), path.as_str()) {
                ("GET", "/health") => Ok(json_reply(200, json!({"ok": true}))),
                ("GET", "/mcp-access") => Ok(json_reply(200, self.mcp_access())),
                ("POST", "/mcp-access") | ("PATCH" | "DELETE", _) if path == "/mcp-access" || path.starts_with("/mcp-access/") => {
                    let input = self.read_json(&mut request).await?;
                    let mut tokens = self.mcp_tokens.lock().unwrap();
                    tokens.update(&verb, path.strip_prefix("/mcp-access/"), input)?;
                    drop(tokens);
                    Ok(json_reply(200, self.mcp_access()))
                }
                ("GET", "/state") => {
                    let query = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes());
                    let cursor = query.clone().find(|(key, _)| key == "cursor").map(|(_, value)| value.into_owned());
                    let since = query.clone().find(|(key, _)| key == "since")
                        .map(|(_, value)| value.parse::<u64>().map_err(Error::bad))
                        .transpose()?;
                    Ok(self.state(request.headers().get("if-none-match").and_then(|v| v.to_str().ok()), since, cursor.as_deref()).await)
                }
                ("GET", "/capture-list-filters") => Ok(json_reply(200, json!({"filters": self.history.lock().unwrap().capture_list_filters.items}))),
                ("POST", "/capture-list-filters") | ("PATCH" | "DELETE", _) if path == "/capture-list-filters" || path.starts_with("/capture-list-filters/") => {
                    let input = self.read_json(&mut request).await?;
                    let result = self.history.lock().unwrap().capture_list_filters.update(&verb, path.strip_prefix("/capture-list-filters/"), input)?;
                    Ok(json_reply(200, result))
                }
                ("GET", "/certificate") => Ok(json_reply(200, self.authority.info()?)),
                #[cfg(windows)]
                (method @ ("GET" | "POST"), "/system-proxy") => {
                    let input = if method == "POST" { self.read_json(&mut request).await? } else { json!({}) };
                    let method = method.to_string();
                    Ok(json_reply(200, tokio::task::spawn_blocking(move || crate::system_proxy::control(&method,input)).await.map_err(Error::bad)??))
                }
                (verb @ ("GET" | "PUT"), path @ ("/workspace/archive" | "/library/archive" | "/platform/archive")) => {
                    let kind = if path.starts_with("/library") { "library" } else if path.starts_with("/platform") { "platform" } else { "workspace" };
                    let document = if verb == "GET" {
                        crate::documents::read(&self.config.directory, kind)?
                    } else {
                        let input = self.read_json(&mut request).await?;
                        crate::documents::write(&self.config.directory, kind, input)?
                    };
                    Ok(json_reply(200, json!({"document":document})))
                }
                ("GET", "/ui-operations") => Ok(json_reply(200, self.ui.pending())),
                ("POST", "/library/snapshots") => {
                    let input = self.read_json(&mut request).await?;
                    let ids = input["ids"].as_array().filter(|ids| (1..=100).contains(&ids.len()))
                        .ok_or_else(|| Error::bad("Select 1–100 captures to save"))?;
                    let mut sessions = Vec::with_capacity(ids.len());
                    let mut size = 0;
                    const LIMIT: usize = 16 * 1024 * 1024;
                    let store = self.history.lock().unwrap();
                    for id in ids {
                        let id = id.as_str().ok_or_else(|| Error::bad("Capture ID must be a string"))?;
                        let session = store.get(id)?.ok_or_else(|| Error::new(404, "Capture no longer available"))?;
                        if session.incognito {
                            return Err(Error::bad("Incognito captures cannot be saved to the library"));
                        }
                        let mut value = serde_json::to_value(session)?;
                        value["retainedSnapshot"] = json!(true);
                        let mut bytes_in_snapshot = serde_json::to_vec(&value)?.len();
                        for phase in ["request", "response"] {
                            let status = format!("{phase}BodyComplete");
                            if value[&status] != true {
                                value[&status] = json!(false);
                                continue;
                            }
                            match store.body(id, phase) {
                                Ok((body, _)) => {
                                    bytes_in_snapshot += body.len().div_ceil(3) * 4 + 32;
                                    if size + bytes_in_snapshot > LIMIT {
                                        return Err(Error::new(413, "Capture snapshots exceed 16 MiB; save fewer captures at a time"));
                                    }
                                    value[format!("{phase}BodyBase64")] = json!(base64::engine::general_purpose::STANDARD.encode(body));
                                }
                                Err(_) => value[&status] = json!(false),
                            }
                        }
                        size += bytes_in_snapshot;
                        if size > LIMIT {
                            return Err(Error::new(413, "Capture snapshots exceed 16 MiB; save fewer captures at a time"));
                        }
                        sessions.push(value);
                    }
                    Ok(json_reply(200, json!({"sessions": sessions})))
                }
                ("POST", path) if path.starts_with("/ui-operations/") => {
                    let input = self.read_json(&mut request).await?;
                    Ok(json_reply(
                        200,
                        self.ui
                            .complete(path.trim_start_matches("/ui-operations/"), input)?,
                    ))
                }
                ("POST", "/workspace") => {
                    let input = self.read_json(&mut request).await?;
                    let name = input["name"]
                        .as_str()
                        .ok_or_else(|| Error::bad("Workspace operation name required"))?;
                    Ok(json_reply(
                        200,
                        self.ui
                            .request(
                                name,
                                input
                                    .get("arguments")
                                    .or_else(|| input.get("args"))
                                    .cloned()
                                    .unwrap_or(json!({})),
                            )
                            .await?,
                    ))
                }
                ("GET", "/settings") => Ok(json_reply(200, self.settings.get())),
                ("PATCH", "/settings") => {
                    let input = self.read_json(&mut request).await?;
                    let _serial = self.settings_change.lock().await;
                    let next = self.settings.updated(input)?;
                    if next.proxy_port != self.settings.snapshot().proxy_port {
                        return Err(Error::bad("Change the listening port using POST /proxy"));
                    }
                    if next.intercept_tls != self.settings.snapshot().intercept_tls {
                        return Err(Error::bad("Change HTTPS interception using POST /proxy"));
                    }
                    let prepared = self.prepare_reverse(&next).await?;
                    let remote_changed=next.mcp_remote_enabled!=self.settings.snapshot().mcp_remote_enabled;
                    let remote=if remote_changed && next.mcp_remote_enabled {Some(Self::bind_proxy(0,std::net::Ipv4Addr::UNSPECIFIED).await?)}else{None};
                    let result = self.settings.save(next.clone())?;
                    if remote_changed {self.activate_remote_mcp(remote)?;}
                    self.activate_reverse(&next, prepared).await;
                    self.rules.configure(&next);
                    self.history
                        .lock()
                        .unwrap()
                        .configure(&next, self.config.request)?;
                    Ok(json_reply(200, result))
                }
                ("GET", path) if path.starts_with("/history/") && path.ends_with("/body") => {
                    let id = path
                        .trim_start_matches("/history/")
                        .trim_end_matches("/body");
                    let phase =
                        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                            .find(|(k, _)| k == "phase")
                            .map(|(_, v)| v.into_owned())
                            .unwrap_or_else(|| "response".into());
                    let (mut bytes, mut headers) = self.history.lock().unwrap().body(id, &phase)?;
                    if url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                        .any(|(k, v)| k == "decoded" && v == "true")
                    {
                        bytes = wire::decoded(&bytes, &headers, self.config.request)?;
                        headers.remove("content-encoding");
                    }
                    let mut response = Response::builder()
                        .status(200)
                        .header("cache-control", "no-store")
                        .body(full(bytes))
                        .map_err(Error::bad)?;
                    for key in ["content-type", "content-encoding"] {
                        if let Some(value) = headers.get(key).and_then(Value::as_str) {
                            response.headers_mut().insert(
                                hyper::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                                hyper::header::HeaderValue::from_str(value).map_err(Error::bad)?,
                            );
                        }
                    }
                    Ok(response)
                }
                ("GET", path) if path.starts_with("/history/") => {
                    let session = self.history.lock().unwrap()
                        .get(path.trim_start_matches("/history/"))?
                        .ok_or_else(|| Error::new(404, "Captured request not found (deleted, expired or never saved)"))?;
                    Ok(json_reply(200, json!(session)))
                }
                ("DELETE", path) if path.starts_with("/history/") => {
                    self.history
                        .lock()
                        .unwrap()
                        .delete(path.trim_start_matches("/history/"))?;
                    Ok(json_reply(200, json!({"ok":true})))
                }
                ("GET", "/rules") => Ok(json_reply(200, self.rules.get())),
                ("PATCH", "/rules/enabled") => {
                    let input = self.read_json(&mut request).await?;
                    let id = input["id"].as_str().ok_or_else(|| Error::bad("id must be a string"))?.to_string();
                    let enabled = input["enabled"].as_bool().ok_or_else(|| Error::bad("enabled must be boolean"))?;
                    let expected = input["expected"].as_bool().ok_or_else(|| Error::bad("expected must be boolean"))?;
                    if input.as_object().unwrap().keys().any(|key| !["id", "enabled", "expected"].contains(&key.as_str())) {
                        return Err(Error::bad("Unexpected rule state field"));
                    }
                    let rules = self.rules.clone();
                    let result = tokio::task::spawn_blocking(move || rules.set_enabled(&id, enabled, expected)).await.map_err(Error::bad)??;
                    Ok(json_reply(200, result))
                }
                ("PUT", "/rules") => {
                    let input = self.read_json(&mut request).await?;
                    Ok(json_reply(
                        200,
                        self.rules
                            .set_expected(input["rules"].clone(), input.get("expected"))?,
                    ))
                }
                ("GET", "/script-logs") => Ok(json_reply(200, crate::python::logs())),
                ("POST", "/rewrite/preview") => {
                    let input = self.read_json(&mut request).await?;
                    Ok(json_reply(200, crate::rules::preview_edit(input)?))
                }
                ("GET", "/breakpoints") => {
                    let summary = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                        .any(|(key, value)| key == "summary" && value == "1");
                    Ok(json_reply(200, if summary { self.rules.pending_summary() } else { self.rules.pending() }))
                }
                ("GET", path) if path.starts_with("/breakpoints/") => {
                    Ok(json_reply(200, self.rules.pending_detail(path.trim_start_matches("/breakpoints/"))?))
                }
                ("POST", path) if path.starts_with("/breakpoints/") => {
                    let id = path.trim_start_matches("/breakpoints/");
                    let input = self.read_json(&mut request).await?;
                    Ok(json_reply(200, self.rules.resume(id, input)?))
                }
                ("GET", "/history") => {
                    let mut offset = 0;
                    let mut limit = 200;
                    let mut query = String::new();
                    for (key, value) in
                        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                    {
                        if key == "query" {
                            query = value.into_owned();
                            continue;
                        }
                        if key == "offset" || key == "limit" {
                            if value.is_empty() || !value.bytes().all(|v| v.is_ascii_digit()) {
                                return Err(Error::bad("Invalid history pagination"));
                            }
                            let number = value.parse::<usize>().map_err(Error::bad)?;
                            if number > i64::MAX as usize {
                                return Err(Error::bad("Invalid history pagination"));
                            }
                            if key == "offset" {
                                offset = number;
                            } else {
                                limit = number;
                            }
                        }
                    }
                    if !(1..=200).contains(&limit) {
                        return Err(Error::bad("History limit must be from 1 to 200"));
                    }
                    Ok(json_reply(
                        200,
                        json!(self.history.lock().unwrap().search(offset, limit, &query)?),
                    ))
                }
                ("DELETE", "/sessions") => {
                    self.history.lock().unwrap().clear()?;
                    Ok(self.state(None, None, None).await)
                }
                ("POST", "/proxy") => {
                    let input = self.read_json(&mut request).await?;
                    let object = input.as_object().unwrap();
                    if object.iter().any(|(k, v)| {
                        match k.as_str() {
                            "running" | "interceptTLS" => !v.is_boolean(),
                            "port" => !v.as_u64().is_some_and(|port| port <= u16::MAX as u64),
                            _ => true,
                        }
                    }) {
                        return Err(Error::bad(
                            "Only boolean running/interceptTLS and integer port (0–65535) settings are accepted",
                        ));
                    }
                    self.configure_proxy(&input).await?;
                    Ok(self.state(None, None, None).await)
                }
                (_, path) if path.starts_with("/grpc-client/") => {
                    let route = request.uri().path_and_query().map(|x| x.as_str()).unwrap_or("").to_string();
                    let input = if verb == "GET" { json!({}) } else { self.read_json(&mut request).await? };
                    let value = crate::grpc_client::control(&verb, &route, input).await.map_err(Error::bad)?;
                    Ok(json_reply(200, value))
                }
                ("POST", "/request/raw") => {
                    let input = self.read_json(&mut request).await?;
                    let record_history = input.get("autoSaveHistory").and_then(Value::as_bool).unwrap_or(true);
                    let settings = self.settings.snapshot();
                    let _permit = self.requests.clone().try_acquire_owned().map_err(|_| Error::new(503, "At most 100 requests may run concurrently"))?;
                    let mut value = crate::raw_client::request(input, self.client_tls.clone(), self.config.request, self.config.timeout, &settings.python_executable, settings.python_timeout_ms).await?;
                    if record_history { crate::raw_client::record(&mut value, self.history.clone(), self.config.preview)?; }
                    else { value["incognito"] = json!(settings.incognito); }
                    Ok(json_reply(200, value))
                }
                ("POST", "/request/raw/cancel") => {
                    let input = self.read_json(&mut request).await?;
                    let id = input.get("requestID").and_then(Value::as_str).ok_or_else(|| Error::bad("Missing requestID"))?;
                    Ok(json_reply(200, json!({"cancelled": crate::raw_client::cancel(id)})))
                }
                ("POST", "/request") => {
                    let input = self.read_json(&mut request).await?;
                    let body = input
                        .get("body")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("body must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    if input.get("bodyBase64").is_some() && input.get("body").is_some() {
                        return Err(Error::bad("Choose body or bodyBase64"));
                    }
                    let binary = input
                        .get("bodyBase64")
                        .map(|value| {
                            let encoded = value
                                .as_str()
                                .ok_or_else(|| Error::bad("bodyBase64 must be a string"))?;
                            base64::engine::general_purpose::STANDARD
                                .decode(encoded)
                                .map_err(|_| Error::bad("Invalid bodyBase64"))
                        })
                        .transpose()?;
                    if binary.as_ref().map_or(body.len(), Vec::len) > self.config.request {
                        return Err(Error::new(
                            413,
                            format!("Request body exceeds {} bytes", self.config.request),
                        ));
                    }
                    let headers = input
                        .get("headers")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("headers must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    let req = RequestData {
                        method: input
                            .get("method")
                            .map(|v| v.as_str().ok_or_else(|| Error::bad("Invalid HTTP method")))
                            .transpose()?
                            .unwrap_or("GET")
                            .into(),
                        url: input["url"]
                            .as_str()
                            .ok_or_else(|| Error::bad("url must be a string"))?
                            .into(),
                        headers: wire::parse_headers(headers)?,
                        body: None,
                        binary_body: None,
                    }
                    .validate()?;
                    let version = input
                        .get("protocol")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("protocol must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("HTTP/1.1");
                    if !["HTTP/1.1", "HTTP/2", "HTTP/3"].contains(&version) {
                        return Err(Error::bad("Unsupported HTTP protocol"));
                    }
                    let before = input
                        .get("beforeScript")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("beforeScript must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    let python_script = input
                        .get("pythonScript")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("pythonScript must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    let after = input
                        .get("afterScript")
                        .map(|v| {
                            v.as_str()
                                .ok_or_else(|| Error::bad("afterScript must be a string"))
                        })
                        .transpose()?
                        .unwrap_or("");
                    let variables = input.get("variables").cloned().unwrap_or(json!({}));
                    if !variables
                        .as_object()
                        .is_some_and(|v| v.values().all(Value::is_string))
                    {
                        return Err(Error::bad("variables must contain string values"));
                    }
                    let mut req = req;
                    req.body = Some(body.to_string());
                    let mut options = ApiExecution {
                        python_script: python_script.into(),
                        shared: json!({"_context":{"env":variables.clone()},"_shared":null}),
                        version: version.into(),
                        after_script: after.into(),
                        variables,
                        logs: vec![],
                        tests: vec![],
                    };
                    if !before.trim().is_empty() {
                        if binary.is_some() {
                            return Err(Error::bad(
                                "Before scripts for binary API bodies are not supported",
                            ));
                        }
                        let result =
                            crate::api_scripts::execute(before, &req, None, &options.variables)?;
                        req = result.request;
                        options.variables = result.variables;
                        options.logs = result.logs;
                        options.tests = result.tests;
                    }
                    if let Some(binary) = binary {
                        req.body = None;
                        req.binary_body = Some(binary);
                    }
                    if !options.python_script.trim().is_empty() {
                        let settings = self.settings.snapshot();
                        let (updated, _, shared) = crate::python::execute(
                            &settings.python_executable,
                            &options.python_script,
                            &req,
                            None,
                            &options.shared,
                            settings.python_timeout_ms,
                        )
                        .await?;
                        req = updated;
                        options.shared = shared;
                        options.variables = options.shared["_context"]["env"].clone();
                        if let Some(log) = options.shared["_console"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                        {
                            options.logs.push(log.into());
                        }
                    }
                    let bytes = Bytes::from(
                        req.binary_body
                            .take()
                            .unwrap_or_else(|| req.body.take().unwrap_or_default().into_bytes()),
                    );
                    if bytes.len() > self.config.request {
                        return Err(Error::new(
                            413,
                            "Script request body exceeds the request limit",
                        ));
                    }
                    self.execute("composer", req, None, Some(bytes), Some(options), None, None)
                        .await
                }
                _ => Err(Error::new(404, "Not found")),
            }
        }
        .await;
        result.unwrap_or_else(error_reply)
    }
    async fn read_json<B>(&self, request: &mut Request<B>) -> Result<Value>
    where
        B: HttpBody<Data = Bytes> + Unpin,
        B::Error: std::fmt::Display,
    {
        let content_type = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type != "application/json" && !content_type.starts_with("application/json;") {
            return Err(Error::new(415, "Content-Type must be application/json"));
        }
        let archive = matches!(
            request.uri().path(),
            "/workspace/archive" | "/library/archive" | "/platform/archive"
        );
        let bytes = read_body(
            request.body_mut(),
            if archive {
                128 * 1024 * 1024 + 64 * 1024
            } else {
                self.config
                    .request
                    .saturating_mul(2)
                    .saturating_add(64 * 1024)
            },
            self.config.timeout,
        )
        .await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        if !value.is_object() {
            return Err(Error::bad("Expected a JSON object"));
        }
        Ok(value)
    }
    async fn proxy_request(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        target: Option<url::Url>,
        reverse: Option<bool>,
        cancel: CancellationToken,
        application: Option<crate::application::Application>,
    ) -> Reply {
        if target.is_none()
            && reverse.is_none()
            && request
                .uri()
                .host()
                .is_some_and(|host| host.eq_ignore_ascii_case(crate::identity::CA_HOST))
        {
            return self.ca_download(&request).unwrap_or_else(error_reply);
        }
        if reverse.is_some() {
            if request.method() == hyper::Method::CONNECT {
                return error_reply(Error::bad(
                    "CONNECT is not accepted by reverse proxy listeners",
                ));
            }
            if let Some(target) = target.as_ref().filter(|_| reverse == Some(false)) {
                request.headers_mut().insert(
                    "host",
                    hyper::header::HeaderValue::from_str(
                        &target[url::Position::BeforeHost..url::Position::AfterPort],
                    )
                    .unwrap(),
                );
            }
        }
        if request.method() == hyper::Method::CONNECT {
            return self
                .connect(request, cancel, application)
                .await
                .unwrap_or_else(error_reply);
        }
        if request
            .headers()
            .get("upgrade")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        {
            return self
                .websocket_proxy(request, target, cancel, application)
                .await
                .unwrap_or_else(error_reply);
        }
        let result = async {
            let url = if let Some(target) = target {
                let url = target
                    .join(&request.uri().to_string())
                    .map_err(Error::bad)?;
                if url.origin() != target.origin() {
                    return Err(Error::bad(
                        "A TLS tunnel may only request its CONNECT destination",
                    ));
                }
                url
            } else {
                parse_url(&request.uri().to_string())?
            };
            let req = RequestData {
                method: request.method().to_string(),
                url: url.to_string(),
                headers: from_headers(request.headers()),
                body: None,
                binary_body: None,
            }
            .validate()?;
            let client_ip = request.extensions().get::<std::net::IpAddr>().copied();
            self.execute(
                "proxy",
                req,
                Some(request.into_body()),
                None,
                None,
                application,
                client_ip,
            )
            .await
        }
        .await;
        result.unwrap_or_else(error_reply)
    }
    fn ca_download(&self, request: &Request<Incoming>) -> Result<Reply> {
        let uri = request.uri();
        let authority = uri.authority().map(|value| value.as_str()).unwrap_or("");
        if uri.scheme_str() != Some("http")
            || !(authority.eq_ignore_ascii_case(crate::identity::CA_HOST)
                || authority.eq_ignore_ascii_case(&format!("{}:80", crate::identity::CA_HOST)))
            || uri.path_and_query().map(|value| value.as_str()) != Some("/ca")
        {
            return Err(Error::new(404, "Certificate download not found"));
        }
        if request.method() != hyper::Method::GET && request.method() != hyper::Method::HEAD {
            let mut response = error_reply(Error::new(405, "Use GET or HEAD to download the CA"));
            response.headers_mut().insert(
                "allow",
                hyper::header::HeaderValue::from_static("GET, HEAD"),
            );
            return Ok(response);
        }
        let pem = std::fs::read(&self.authority.ca_path)?;
        let certificate = rustls_pemfile::certs(&mut pem.as_slice())
            .next()
            .transpose()?
            .ok_or_else(|| Error::new(500, "The local CA certificate is missing"))?;
        Ok(Response::builder()
            .header("content-type", "application/x-x509-ca-cert")
            .header(
                "content-disposition",
                format!("attachment; filename=\"{}\"", crate::identity::CA_FILENAME),
            )
            .header("content-length", certificate.as_ref().len())
            .header("cache-control", "no-store")
            .header("x-content-type-options", "nosniff")
            .body(full(if request.method() == hyper::Method::HEAD {
                Bytes::new()
            } else {
                Bytes::copy_from_slice(certificate.as_ref())
            }))
            .unwrap())
    }
    async fn websocket_proxy(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        target: Option<url::Url>,
        cancel: CancellationToken,
        application: Option<crate::application::Application>,
    ) -> Result<Reply> {
        if request.method() != hyper::Method::GET {
            return Err(Error::bad("WebSocket handshake requires GET"));
        }
        let url = if let Some(target) = target {
            let url = target
                .join(&request.uri().to_string())
                .map_err(Error::bad)?;
            if url.origin() != target.origin() {
                return Err(Error::bad("TLS tunnel destination mismatch"));
            }
            url
        } else {
            let uri = request.uri().to_string();
            parse_url(
                &uri.replacen("ws://", "http://", 1)
                    .replacen("wss://", "https://", 1),
            )?
        };
        let settings = self.settings.snapshot();
        let policy = self
            .gateway_policy(&settings, &url, false, "GET", true)
            .await;
        let record = Arc::new(Mutex::new(Recording::new_hidden_for_application(
            self.history.clone(),
            "proxy",
            "GET",
            url.as_str(),
            from_headers(request.headers()),
            self.config.preview,
            policy.bypass
                || ((policy.block_request || policy.block_response) && !policy.show_blocked),
            application,
        )));
        {
            let mut recording = record.lock().unwrap();
            recording.session.client_ip = request
                .extensions()
                .get::<std::net::IpAddr>()
                .map(ToString::to_string);
            if policy.matched {
                recording.intercepted_by("网关");
            }
        }
        let mut guard = FinishGuard::new(record.clone());
        if policy.block_request || policy.block_response {
            return Err(Error::new(403, "Blocked by WebSocket gateway rule"));
        }
        if policy.suspend_request || policy.suspend_response {
            tokio::time::sleep(self.config.timeout).await;
            return Err(Error::new(504, "Suspended by WebSocket gateway rule"));
        }
        let permit = self.tunnels.clone().try_acquire_owned().map_err(|_| {
            Error::new(503, "At most100 tunnels or WebSockets may run concurrently")
        })?;
        let incoming_upgrade = hyper::upgrade::on(&mut request);
        let req = RequestData {
            method: "GET".into(),
            url: url.to_string(),
            headers: from_headers(request.headers()),
            body: Some(String::new()),
            binary_body: None,
        };
        let original_url = req.url.clone();
        let (req, selected) = self.rules.prepare(req)?;
        {
            let mut recording = record.lock().unwrap();
            if req.url != original_url {
                recording.intercepted_by("重写");
            }
            for kind in selected.iter().flat_map(Rule::interceptors) {
                recording.intercepted_by(kind);
            }
        }
        let mut shared = json!({});
        let (mut req, mock) = self
            .rules
            .apply_request_flow(req, &selected, &mut shared)
            .await?;
        record.lock().unwrap().rule_logs(&mut shared);
        if mock.is_some()
            || req.binary_body.is_some()
            || req.body.as_ref().is_some_and(|b| !b.is_empty())
        {
            return Err(Error::bad(
                "WebSocket handshakes cannot replace the body or use a local response",
            ));
        }
        let url = parse_url(&req.url)?;
        let host = url.host_str().unwrap();
        let port = url.port_or_known_default().unwrap();
        let mirror = settings.mirror(host, port, url.scheme());
        if mirror.is_some() {
            record.lock().unwrap().intercepted_by("镜像");
        }
        let destination = mirror
            .as_ref()
            .map(|m| parse_url(&format!("{}://{}", url.scheme(), m.target)))
            .transpose()?;
        let destination_host = destination
            .as_ref()
            .and_then(|u| u.host_str())
            .unwrap_or(host);
        let destination_port = destination.as_ref().and_then(|u| u.port()).unwrap_or(port);
        if mirror.as_ref().is_some_and(|m| m.host_policy == "target") {
            req.headers.insert(
                "host".into(),
                json!(format_authority(
                    destination_host,
                    destination_port,
                    url.scheme()
                )),
            );
        }
        let upstream = settings.upstream(host, port);
        let absolute = upstream.is_some() && url.scheme() == "http";
        let (transport, _, server_ip) = self
            .transport(
                destination_host,
                destination_port,
                url.scheme() == "https",
                upstream.as_ref(),
                if mirror.as_ref().is_some_and(|m| m.sni_policy == "target") {
                    destination_host
                } else {
                    host
                },
                false,
            )
            .await?;
        record.lock().unwrap().session.server_ip = Some(server_ip);
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(
            IdleIo::new(transport, self.config.timeout),
        ))
        .await
        .map_err(Error::upstream)?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        }));
        let request_uri = if absolute {
            let mut routed = url.clone();
            routed
                .set_host(Some(destination_host))
                .map_err(Error::bad)?;
            routed
                .set_port(Some(destination_port))
                .map_err(|_| Error::bad("Invalid mirror destination port"))?;
            routed.to_string()
        } else {
            url[url::Position::BeforePath..url::Position::AfterQuery].to_string()
        };
        let mut outgoing = Request::builder()
            .method(req.method.as_str())
            .uri(request_uri)
            .body(full(Bytes::new()))
            .map_err(Error::bad)?;
        *outgoing.headers_mut() = wire::forward_headers(&req.headers)?;
        outgoing.headers_mut().insert(
            "upgrade",
            hyper::header::HeaderValue::from_static("websocket"),
        );
        outgoing.headers_mut().insert(
            "connection",
            hyper::header::HeaderValue::from_static("Upgrade"),
        );
        if !outgoing.headers().contains_key("host") {
            outgoing.headers_mut().insert(
                "host",
                hyper::header::HeaderValue::from_str(&format_authority(host, port, url.scheme()))
                    .map_err(Error::bad)?,
            );
        }
        if absolute {
            if let Some(auth) = upstream.as_ref().and_then(|p| p.authorization()) {
                outgoing.headers_mut().insert(
                    "proxy-authorization",
                    hyper::header::HeaderValue::from_str(&auth).map_err(Error::bad)?,
                );
            }
        }
        {
            let mut recording = record.lock().unwrap();
            recording.session.url = url.to_string();
            recording.session.protocol = "WebSocket".into();
            recording.session.request_headers = header_text(&capture_headers(outgoing.headers()));
        }
        let mut response = sender
            .send_request(outgoing)
            .await
            .map_err(Error::upstream)?;
        if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
            let status = response.status();
            let headers = from_headers(response.headers());
            let bytes = read_body(
                response.body_mut(),
                self.config.request,
                self.config.timeout,
            )
            .await?;
            let mut recording = record.lock().unwrap();
            recording.session.status = status.as_u16();
            recording.session.response_headers = header_text(&headers);
            recording.response_headers = headers.clone();
            recording.response.add(&bytes);
            recording.finish(None);
            guard.armed = false;
            let mut reply = Response::builder()
                .status(status)
                .body(full(bytes))
                .map_err(Error::bad)?;
            *reply.headers_mut() = wire::forward_headers(&headers)?;
            return Ok(reply);
        }
        let changed = self
            .rules
            .apply_response_flow(
                &req,
                ResponseData {
                    status: 101,
                    headers: from_headers(response.headers()),
                    body: Some(String::new()),
                    binary_body: None,
                },
                &selected,
                &mut shared,
            )
            .await?;
        record.lock().unwrap().rule_logs(&mut shared);
        if changed.status != 101
            || changed.binary_body.is_some()
            || changed.body.as_ref().is_some_and(|b| !b.is_empty())
        {
            return Err(Error::bad(
                "WebSocket upgrade must preserve status101 and an empty body",
            ));
        }
        let compression = response
            .headers()
            .get("sec-websocket-extensions")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::websocket::Compression::parse);
        let outgoing_upgrade = hyper::upgrade::on(&mut response);
        let mut reply = Response::builder()
            .status(101)
            .body(full(Bytes::new()))
            .map_err(Error::bad)?;
        *reply.headers_mut() = wire::forward_headers(&changed.headers)?;
        reply.headers_mut().insert(
            "upgrade",
            hyper::header::HeaderValue::from_static("websocket"),
        );
        reply.headers_mut().insert(
            "connection",
            hyper::header::HeaderValue::from_static("Upgrade"),
        );
        {
            let mut recording = record.lock().unwrap();
            recording.session.status = 101;
            recording.session.response_headers = header_text(&from_headers(reply.headers()));
            recording.response_headers = changed.headers;
            recording.publish();
        }
        guard.armed = false;
        let engine = self.clone();
        self.tasks.spawn(async move {
            let _permit=permit;
            let mut guard=FinishGuard::new(record.clone());
            let transfer=async{
                let (client,server)=tokio::try_join!(incoming_upgrade,outgoing_upgrade).map_err(Error::upstream)?;
                let mut client=crate::websocket::Observed::new(TokioIo::new(client),record.clone(),true,engine.config.preview,compression);
                let mut server=crate::websocket::Observed::new(TokioIo::new(server),record.clone(),false,engine.config.preview,compression);
                tokio::io::copy_bidirectional(&mut client,&mut server).await?;Ok::<_,Error>(())
            };
            let result=tokio::select!{_ = cancel.cancelled()=>Err(Error::new(503,"WebSocket proxy stopped")),result=transfer=>result};
            record.lock().unwrap().finish(result.err().map(|e|e.message));guard.armed=false;drop(driver);
        });
        Ok(reply)
    }
    async fn execute(
        self: &Arc<Self>,
        source: &str,
        req: RequestData,
        incoming: Option<Incoming>,
        bytes: Option<Bytes>,
        options: Option<ApiExecution>,
        application: Option<crate::application::Application>,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<Reply> {
        let permit = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::new(503, "At most 100 requests may run concurrently"))?;
        let version = options
            .as_ref()
            .map(|o| o.version.as_str())
            .unwrap_or("AUTO");
        let policy = self
            .gateway_policy(
                &self.settings.snapshot(),
                &parse_url(&req.url)?,
                false,
                &req.method,
                false,
            )
            .await;
        let record = Arc::new(Mutex::new(Recording::new_hidden_for_application(
            self.history.clone(),
            source,
            &req.method,
            &req.url,
            req.headers.clone(),
            self.config.preview,
            policy.bypass
                || ((policy.block_request || policy.block_response) && !policy.show_blocked),
            application,
        )));
        {
            let mut recording = record.lock().unwrap();
            recording.session.client_ip = client_ip.map(|ip| ip.to_string());
            if policy.matched {
                recording.intercepted_by("网关");
            }
        }
        let mut guard = FinishGuard::new(record.clone());
        if policy.block_request {
            let error = Error::new(403, "Blocked by gateway request rule");
            record.lock().unwrap().finish(Some(error.message.clone()));
            return Err(error);
        }
        if policy.suspend_request {
            tokio::time::sleep(self.config.timeout).await;
            return Err(Error::new(504, "Suspended by gateway request rule"));
        }
        let response = self
            .dispatch(req, incoming, bytes, record.clone(), permit, version)
            .await;
        guard.armed = false;
        if policy.block_response {
            record
                .lock()
                .unwrap()
                .finish(Some("Blocked by gateway response rule".into()));
            return Err(Error::new(403, "Blocked by gateway response rule"));
        }
        if policy.suspend_response {
            tokio::time::sleep(self.config.timeout).await;
            record
                .lock()
                .unwrap()
                .finish(Some("Suspended by gateway response rule".into()));
            return Err(Error::new(504, "Suspended by gateway response rule"));
        }
        match response {
            Ok(mut response) if source == "composer" => {
                while let Some(frame) = response.body_mut().frame().await {
                    if let Err(error) = frame {
                        record.lock().unwrap().finish(Some(error.message));
                        break;
                    }
                }
                if let Some(mut options) = options {
                    if !options.python_script.trim().is_empty()
                        && record.lock().unwrap().session.error.is_none()
                    {
                        let messages = (|| -> Result<(RequestData, ResponseData)> {
                            let recording = record.lock().unwrap();
                            if recording.response.archive.len() != recording.response.length
                                || recording.request.archive.len() != recording.request.length
                            {
                                return Err(Error::new(
                                    413,
                                    "Body is too large for API Python scripts",
                                ));
                            }
                            let (body, binary_body) = wire::script_body(
                                &recording.request.archive,
                                &recording.request_headers,
                                self.config.request,
                            )?;
                            let request = RequestData {
                                method: recording.session.method.clone(),
                                url: recording.session.url.clone(),
                                headers: recording.request_headers.clone(),
                                body,
                                binary_body,
                            };
                            let (body, binary_body) = wire::script_body(
                                &recording.response.archive,
                                &recording.response_headers,
                                self.config.request,
                            )?;
                            Ok((
                                request,
                                ResponseData {
                                    status: recording.session.status,
                                    headers: recording.response_headers.clone(),
                                    body,
                                    binary_body,
                                },
                            ))
                        })();
                        let result = match messages {
                            Ok((request, response)) => {
                                let settings = self.settings.snapshot();
                                crate::python::execute(
                                    &settings.python_executable,
                                    &options.python_script,
                                    &request,
                                    Some(&response),
                                    &options.shared,
                                    settings.python_timeout_ms,
                                )
                                .await
                                .map(|(_, updated, shared)| (response, updated, shared))
                            }
                            Err(error) => Err(error),
                        };
                        match result {
                            Ok((previous, Some(updated), shared)) => {
                                options.shared = shared;
                                options.variables = options.shared["_context"]["env"].clone();
                                if let Some(log) = options.shared["_console"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                {
                                    options.logs.push(log.into());
                                }
                                if previous.status != updated.status
                                    || previous.headers != updated.headers
                                    || previous.body != updated.body
                                    || previous.binary_body != updated.binary_body
                                {
                                    let mut recording = record.lock().unwrap();
                                    if let Err(error) = recording.replace_response(updated) {
                                        recording.session.error = Some(error.to_string());
                                    }
                                }
                            }
                            Ok(_) => {
                                record.lock().unwrap().session.error =
                                    Some("Python onResponse returned no response".into());
                            }
                            Err(error) => {
                                record.lock().unwrap().session.error = Some(error.to_string());
                                options.logs.push(error.to_string());
                            }
                        }
                    }
                    let mut recording = record.lock().unwrap();
                    recording.session.protocol = options.version;
                    if !options.after_script.trim().is_empty() && recording.session.error.is_none()
                    {
                        let result = (|| {
                            if recording.response.archive.len() != recording.response.length {
                                return Err(Error::new(
                                    413,
                                    "Response is too large for API scripts",
                                ));
                            }
                            let request = RequestData {
                                method: recording.session.method.clone(),
                                url: recording.session.url.clone(),
                                headers: recording.request_headers.clone(),
                                body: Some(wire::rules_text(
                                    &recording.request.archive,
                                    &recording.request_headers,
                                    self.config.request,
                                )?),
                                binary_body: None,
                            };
                            let response = ResponseData {
                                status: recording.session.status,
                                headers: recording.response_headers.clone(),
                                body: Some(wire::rules_text(
                                    &recording.response.archive,
                                    &recording.response_headers,
                                    self.config.request,
                                )?),
                                binary_body: None,
                            };
                            crate::api_scripts::execute(
                                &options.after_script,
                                &request,
                                Some(&response),
                                &options.variables,
                            )
                        })();
                        match result {
                            Ok(result) => {
                                options.logs.extend(result.logs);
                                options.tests.extend(result.tests);
                                options.variables = result.variables;
                            }
                            Err(error) => {
                                recording.session.error = Some(error.to_string());
                                options.logs.push(error.to_string());
                            }
                        }
                    }
                    recording.session.script_logs.extend(options.logs);
                    recording.session.tests = options.tests;
                    recording.session.variables = Some(options.variables);
                    self.history.lock().unwrap().save(&recording.session);
                }
                Ok(json_reply(200, json!(record.lock().unwrap().session)))
            }
            Ok(response) => Ok(response),
            Err(error) => {
                record.lock().unwrap().finish(Some(error.message.clone()));
                if source == "composer" {
                    Ok(json_reply(200, json!(record.lock().unwrap().session)))
                } else {
                    Err(error)
                }
            }
        }
    }
    async fn gateway_policy(
        &self,
        settings: &Settings,
        url: &url::Url,
        l4_only: bool,
        method: &str,
        websocket: bool,
    ) -> GatewayDecision {
        let mut addresses = vec![];
        if settings.needs_gateway_addresses() {
            if let Ok(Ok(resolved)) = tokio::time::timeout(
                self.config.timeout,
                tokio::net::lookup_host((
                    url.host_str().unwrap_or("").trim_matches(['[', ']']),
                    url.port_or_known_default().unwrap_or(80),
                )),
            )
            .await
            {
                addresses.extend(resolved.map(|a| a.ip()));
            }
        }
        settings.gateway_with(url, l4_only, method, websocket, &addresses)
    }
    async fn tunnel_transport(
        &self,
        host: &str,
        port: u16,
        upstream: Option<&crate::settings::Upstream>,
    ) -> Result<TcpStream> {
        let (connect_host, connect_port) = upstream
            .map(|p| (p.host.as_str(), p.port))
            .unwrap_or((host, port));
        let mut socket = tokio::time::timeout(
            self.config.timeout,
            TcpStream::connect((connect_host.trim_matches(['[', ']']), connect_port)),
        )
        .await
        .map_err(|_| Error::new(504, "Upstream connection timed out"))??;
        if let Some(upstream) = upstream {
            let authority = format!("{host}:{port}");
            let authorization = upstream
                .authorization()
                .map(|v| format!("Proxy-Authorization: {v}\r\n"))
                .unwrap_or_default();
            socket
                .write_all(
                    format!(
                        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{authorization}\r\n"
                    )
                    .as_bytes(),
                )
                .await?;
            let read = async {
                let mut headers = Vec::new();
                let mut byte = [0; 1];
                while !headers.ends_with(b"\r\n\r\n") {
                    if headers.len() >= 32768 {
                        return Err(Error::upstream("Upstream CONNECT headers exceed limit"));
                    }
                    socket.read_exact(&mut byte).await?;
                    headers.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&headers);
                if line
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    != Some("200")
                {
                    return Err(Error::upstream(format!(
                        "Upstream proxy rejected CONNECT: {}",
                        line.lines().next().unwrap_or("")
                    )));
                }
                Ok::<_, Error>(())
            };
            tokio::time::timeout(self.config.timeout, read)
                .await
                .map_err(|_| Error::new(504, "Upstream CONNECT timed out"))??;
        }
        Ok(socket)
    }
    async fn transport(
        &self,
        host: &str,
        port: u16,
        tls: bool,
        upstream: Option<&crate::settings::Upstream>,
        sni: &str,
        allow_h2: bool,
    ) -> Result<(BoxIo, bool, String)> {
        let socket = if tls {
            self.tunnel_transport(host, port, upstream).await?
        } else {
            let (host, port) = upstream
                .map(|p| (p.host.as_str(), p.port))
                .unwrap_or((host, port));
            self.tunnel_transport(host, port, None).await?
        };
        let server_ip = socket.peer_addr()?.ip().to_string();
        if tls {
            let name = ServerName::try_from(sni.trim_matches(['[', ']']).to_string())
                .map_err(Error::bad)?;
            let mut tls_config = (*self.client_tls).clone();
            if !allow_h2 {
                tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
            }
            let connector = TlsConnector::from(Arc::new(tls_config));
            let secure = tokio::time::timeout(self.config.timeout, connector.connect(name, socket))
                .await
                .map_err(|_| Error::new(504, "Upstream TLS handshake timed out"))??;
            let h2 = secure.get_ref().1.alpn_protocol() == Some(b"h2");
            Ok((Box::new(secure), h2, server_ip))
        } else {
            Ok((Box::new(socket), false, server_ip))
        }
    }
    async fn dispatch(
        self: &Arc<Self>,
        mut req: RequestData,
        mut incoming: Option<Incoming>,
        mut bytes: Option<Bytes>,
        record: Record,
        permit: OwnedSemaphorePermit,
        version: &str,
    ) -> Result<Reply> {
        if req
            .headers
            .get("content-length")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|n| n > self.config.request)
        {
            return Err(Error::new(
                413,
                format!("Request body exceeds {} bytes", self.config.request),
            ));
        }
        let original_url = req.url.clone();
        let (prepared, selected) = self.rules.prepare(req)?;
        {
            let mut recording = record.lock().unwrap();
            if prepared.url != original_url {
                recording.intercepted_by("重写");
            }
            for kind in selected.iter().flat_map(Rule::interceptors) {
                recording.intercepted_by(kind);
            }
        }
        req = prepared;
        let mut shared = json!({});
        if selected.iter().any(Rule::request_body) {
            if let Some(mut body) = incoming.take() {
                let (data, trailers) =
                    read_body_with_trailers(&mut body, self.config.request, self.config.timeout)
                        .await?;
                record.lock().unwrap().session.request_trailers = header_text(&trailers);
                bytes = Some(data);
            }
        }
        let (original, original_binary) = if selected.iter().any(Rule::request_body) {
            if let Some(bytes) = &bytes {
                wire::script_body(bytes, &req.headers, self.config.request).or_else(|error| {
                    if selected.iter().any(Rule::replaces_request_body) {
                        Ok((Some(String::new()), None))
                    } else {
                        Err(error)
                    }
                })?
            } else {
                (Some(String::new()), None)
            }
        } else {
            (
                bytes
                    .as_ref()
                    .and_then(|b| String::from_utf8(b.to_vec()).ok()),
                None,
            )
        };
        req.body = original.clone();
        req.binary_body = original_binary.clone();
        let (mut req, mock) = self
            .rules
            .apply_request_flow(req, &selected, &mut shared)
            .await?;
        record.lock().unwrap().rule_logs(&mut shared);
        if let Some(binary) = &req.binary_body {
            if Some(binary) != original_binary.as_ref() {
                bytes = Some(Bytes::copy_from_slice(binary));
                req.headers.remove("content-encoding");
            }
        } else if req.body != original || original_binary.is_some() {
            bytes = Some(Bytes::from(req.body.clone().unwrap_or_default()));
            req.headers.remove("content-encoding");
        }
        if bytes
            .as_ref()
            .is_some_and(|b| b.len() > self.config.request)
        {
            return Err(Error::new(
                413,
                "Rewritten request body exceeds the request limit",
            ));
        }
        let url = parse_url(&req.url)?;
        req.headers.entry("host".into()).or_insert(json!(
            &url[url::Position::BeforeHost..url::Position::AfterPort]
        ));
        if let Some(body) = &bytes {
            req.headers
                .insert("content-length".into(), json!(body.len().to_string()));
        }
        {
            let mut recording = record.lock().unwrap();
            recording.session.method = req.method.clone();
            recording.session.url = req.url.clone();
            recording.session.tls = url.scheme() == "https";
            recording.request_headers = req.headers.clone();
            recording.session.request_headers = header_text(&req.headers);
            if let Some(bytes) = &bytes {
                recording.request.add(bytes);
                if recording.match_list_filters() {
                    recording.publish();
                }
            }
        }
        if let Some(mock) = mock {
            let bytes = Bytes::from(
                mock.binary_body
                    .clone()
                    .unwrap_or_else(|| mock.body.clone().unwrap_or_default().into_bytes()),
            );
            return self
                .buffered_response(&req, mock, bytes, &selected, record, &mut shared)
                .await;
        }
        if version == "HTTP/3" {
            let settings = self.settings.snapshot();
            let host = url.host_str().unwrap();
            let port = url.port_or_known_default().unwrap();
            if settings.upstream(host, port).is_some()
                || settings.mirror(host, port, url.scheme()).is_some()
            {
                return Err(Error::bad(
                    "HTTP/3 API requests require direct QUIC routing",
                ));
            }
            let (response, bytes) = crate::protocol_client::send(
                version,
                &req,
                bytes.unwrap_or_default(),
                self.client_tls.clone(),
                self.config.request,
                self.config.timeout,
            )
            .await?;
            return self
                .buffered_response(&req, response, bytes, &selected, record, &mut shared)
                .await;
        }
        let body = if let Some(bytes) = bytes {
            buffered(bytes, &record.lock().unwrap().session.request_trailers)?
        } else if let Some(incoming) = incoming {
            ObservedBody {
                inner: incoming,
                record: record.clone(),
                response: false,
                limit: self.config.request,
                finish: None,
                driver: None,
                permit: None,
                timer: None,
                timeout: self.config.timeout,
            }
            .boxed_unsync()
        } else {
            full(Bytes::new())
        };
        let settings = self.settings.snapshot();
        let host = url.host_str().unwrap().trim_matches(['[', ']']);
        let port = url.port_or_known_default().unwrap();
        let mirror = settings.mirror(host, port, url.scheme());
        if mirror.is_some() {
            record.lock().unwrap().intercepted_by("镜像");
        }
        let destination = mirror
            .as_ref()
            .map(|m| parse_url(&format!("{}://{}", url.scheme(), m.target)))
            .transpose()?;
        let destination_host = destination
            .as_ref()
            .and_then(|u| u.host_str())
            .unwrap_or(host);
        let destination_port = destination.as_ref().and_then(|u| u.port()).unwrap_or(port);
        if mirror.as_ref().is_some_and(|m| m.host_policy == "target") {
            req.headers.insert(
                "host".into(),
                json!(format_authority(
                    destination_host,
                    destination_port,
                    url.scheme()
                )),
            );
        } else if mirror.as_ref().is_some_and(|m| m.host_policy == "original") {
            req.headers.insert(
                "host".into(),
                json!(format_authority(host, port, url.scheme())),
            );
        }
        let upstream = settings.upstream(host, port);
        let (transport, negotiated_h2, server_ip) = if version == "HTTP/2" && url.scheme() == "http"
        {
            let socket = self
                .tunnel_transport(destination_host, destination_port, upstream.as_ref())
                .await?;
            let server_ip = socket.peer_addr()?.ip().to_string();
            (Box::new(socket) as BoxIo, true, server_ip)
        } else {
            self.transport(
                destination_host,
                destination_port,
                url.scheme() == "https",
                upstream.as_ref(),
                if mirror.as_ref().is_some_and(|m| m.sni_policy == "target") {
                    destination_host
                } else {
                    host
                },
                version != "HTTP/1.1",
            )
            .await?
        };
        record.lock().unwrap().session.server_ip = Some(server_ip);
        if version == "HTTP/2" && !negotiated_h2 {
            return Err(Error::upstream("Origin did not negotiate HTTP/2"));
        }
        let h2 = negotiated_h2;
        let absolute_uri = upstream.is_some() && url.scheme() == "http" && !h2;
        let (mut sender, driver) = if h2 {
            let (sender, connection) = hyper::client::conn::http2::handshake(
                TokioExecutor::new(),
                TokioIo::new(IdleIo::new(transport, self.config.timeout)),
            )
            .await
            .map_err(Error::upstream)?;
            (
                UpstreamSender::Http2(sender),
                Driver(tokio::spawn(async move {
                    let _ = connection.await;
                })),
            )
        } else {
            let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(
                IdleIo::new(transport, self.config.timeout),
            ))
            .await
            .map_err(Error::upstream)?;
            (
                UpstreamSender::Http1(sender),
                Driver(tokio::spawn(async move {
                    let _ = connection.await;
                })),
            )
        };
        record.lock().unwrap().session.protocol = if h2 { "HTTP/2" } else { "HTTP/1.1" }.into();
        let request_uri = if h2 {
            hyper::Uri::builder()
                .scheme(url.scheme())
                .authority(
                    req.headers
                        .get("host")
                        .and_then(Value::as_str)
                        .unwrap_or(&url[url::Position::BeforeHost..url::Position::AfterPort]),
                )
                .path_and_query(&url[url::Position::BeforePath..url::Position::AfterQuery])
                .build()
                .map_err(Error::bad)?
                .to_string()
        } else if absolute_uri {
            let mut routed = url.clone();
            routed
                .set_host(Some(destination_host))
                .map_err(Error::bad)?;
            routed
                .set_port(Some(destination_port))
                .map_err(|_| Error::bad("Invalid mirror destination port"))?;
            routed.to_string()
        } else {
            url[url::Position::BeforePath..url::Position::AfterQuery].to_string()
        };
        let mut outgoing = Request::builder()
            .method(req.method.as_str())
            .uri(request_uri)
            .body(body)
            .map_err(Error::bad)?;
        *outgoing.headers_mut() = wire::forward_headers(&req.headers)?;
        if h2
            && req
                .headers
                .get("content-type")
                .and_then(Value::as_str)
                .is_some_and(|v| v.starts_with("application/grpc"))
        {
            outgoing
                .headers_mut()
                .insert("te", hyper::header::HeaderValue::from_static("trailers"));
        }
        if absolute_uri {
            if let Some(authorization) = upstream.as_ref().and_then(|p| p.authorization()) {
                outgoing.headers_mut().insert(
                    "proxy-authorization",
                    hyper::header::HeaderValue::from_str(&authorization).map_err(Error::bad)?,
                );
            }
        }
        if !outgoing.headers().contains_key("host") {
            outgoing.headers_mut().insert(
                "host",
                hyper::header::HeaderValue::from_str(
                    &url[url::Position::BeforeHost..url::Position::AfterPort],
                )
                .map_err(Error::bad)?,
            );
        }
        record.lock().unwrap().session.request_headers =
            header_text(&capture_headers(outgoing.headers()));
        let response = sender.send(outgoing).await.map_err(|error| {
            if record.lock().unwrap().request.length > self.config.request {
                Error::new(
                    413,
                    format!("Request body exceeds {} bytes", self.config.request),
                )
            } else {
                wire::network_error(error)
            }
        })?;
        let (parts, mut incoming) = response.into_parts();
        let original = ResponseData {
            status: parts.status.as_u16(),
            headers: from_headers(&parts.headers),
            body: None,
            binary_body: None,
        };
        if selected.iter().any(Rule::response_body) {
            let (bytes, trailers) =
                read_body_with_trailers(&mut incoming, self.config.request, self.config.timeout)
                    .await?;
            record.lock().unwrap().session.response_trailers = header_text(&trailers);
            self.buffered_response(&req, original, bytes, &selected, record, &mut shared)
                .await
        } else {
            let changed = self
                .rules
                .apply_response_flow(&req, original, &selected, &mut shared)
                .await?;
            record.lock().unwrap().rule_logs(&mut shared);
            {
                let mut recording = record.lock().unwrap();
                recording.session.status = changed.status;
                recording.session.response_headers = header_text(&changed.headers);
                recording.response_headers = changed.headers.clone();
                if incoming.is_end_stream() {
                    recording.finish(None);
                }
            }
            let finish = Some(FinishGuard::new(record.clone()));
            let body = ObservedBody {
                inner: incoming,
                record,
                response: true,
                limit: self.config.request,
                finish,
                driver: Some(driver),
                permit: Some(permit),
                timer: None,
                timeout: self.config.timeout,
            }
            .boxed_unsync();
            let mut response = Response::builder()
                .status(changed.status)
                .body(body)
                .map_err(Error::bad)?;
            *response.headers_mut() = wire::forward_headers(&changed.headers)?;
            Ok(response)
        }
    }
    async fn buffered_response(
        &self,
        req: &RequestData,
        mut original: ResponseData,
        mut bytes: Bytes,
        selected: &[Rule],
        record: Record,
        shared: &mut Value,
    ) -> Result<Reply> {
        let (text, binary) = if selected.iter().any(Rule::response_body) {
            wire::script_body(&bytes, &original.headers, self.config.request).or_else(|error| {
                if selected.iter().any(Rule::replaces_response_body) {
                    Ok((Some(String::new()), None))
                } else {
                    Err(error)
                }
            })?
        } else {
            (None, None)
        };
        original.body = text.clone();
        original.binary_body = binary.clone();
        let mut changed = self
            .rules
            .apply_response_flow(req, original, selected, shared)
            .await?;
        record.lock().unwrap().rule_logs(shared);
        if let Some(updated) = &changed.binary_body {
            if Some(updated) != binary.as_ref() {
                bytes = Bytes::copy_from_slice(updated);
                changed.headers.remove("content-encoding");
            }
        } else if changed.body != text || binary.is_some() {
            bytes = Bytes::from(changed.body.clone().unwrap_or_default());
            changed.headers.remove("content-encoding");
        }
        if bytes.len() > self.config.request {
            return Err(Error::new(
                413,
                "Rewritten response body exceeds the request limit",
            ));
        }
        changed
            .headers
            .insert("content-length".into(), json!(bytes.len().to_string()));
        let mut response = Response::builder()
            .status(changed.status)
            .body(buffered(
                bytes.clone(),
                &record.lock().unwrap().session.response_trailers,
            )?)
            .map_err(Error::bad)?;
        *response.headers_mut() = wire::forward_headers(&changed.headers)?;
        let mut recording = record.lock().unwrap();
        recording.session.status = changed.status;
        recording.session.response_headers = header_text(&changed.headers);
        recording.response_headers = changed.headers;
        recording.response.add(&bytes);
        recording.finish(None);
        Ok(response)
    }
    async fn connect(
        self: Arc<Self>,
        mut request: Request<Incoming>,
        cancel: CancellationToken,
        application: Option<crate::application::Application>,
    ) -> Result<Reply> {
        let authority = request
            .uri()
            .authority()
            .ok_or_else(|| Error::bad("Invalid CONNECT authority"))?
            .as_str();
        if authority
            .chars()
            .any(|v| v.is_whitespace() || "/?#@".contains(v))
        {
            return Err(Error::bad("Invalid CONNECT authority"));
        }
        let target = parse_url(&format!("https://{authority}"))?;
        let permit = self
            .tunnels
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::new(503, "At most 100 CONNECT tunnels may run concurrently"))?;
        let settings = self.settings.snapshot();
        let host = target.host_str().unwrap();
        let port = target.port_or_known_default().unwrap();
        let policy = self
            .gateway_policy(&settings, &target, true, "CONNECT", false)
            .await;
        let tls_enabled = self.proxy.lock().await.intercept;
        let intercept = tls_enabled && settings.intercept(host, port) && !policy.bypass;
        let tunnel_notice = if !tls_enabled {
            "[Encrypted tunnel; HTTPS decryption is disabled. Select HTTPS in the system proxy protocols and trust the local CA on the device. CONNECT 200 only confirms tunnel establishment.]"
        } else if policy.bypass {
            "[Encrypted tunnel; bypassed by gateway policy. CONNECT 200 only confirms tunnel establishment.]"
        } else {
            "[Encrypted tunnel; bypassed by the active SSL policy. Review Debug > SSL Proxy. CONNECT 200 only confirms tunnel establishment.]"
        };
        let mut recording = Recording::new_hidden_for_application(
            self.history.clone(),
            "proxy",
            "CONNECT",
            target.as_str().trim_end_matches('/'),
            from_headers(request.headers()),
            self.config.preview,
            policy.bypass
                || (policy.block_request && !policy.show_blocked)
                || (!intercept && settings.hide_bypassed_ssl(host, port)),
            application.clone(),
        );
        let client_ip = request.extensions().get::<std::net::IpAddr>().copied();
        recording.session.client_ip = client_ip.map(|ip| ip.to_string());
        if policy.matched {
            recording.intercepted_by("网关");
        }
        if settings.mirror(host, port, "https").is_some() {
            recording.intercepted_by("镜像");
        }
        if policy.block_request {
            recording.finish(Some("Blocked by gateway L4 rule".into()));
            return Err(Error::new(403, "Blocked by gateway L4 rule"));
        }
        let prepared = async {
            if intercept {
                Ok((
                    Some(
                        self.authority
                            .certificate(
                                &settings
                                    .mirror(host, port, "https")
                                    .filter(|m| m.cn_policy == "target")
                                    .map(|m| {
                                        parse_url(&format!("https://{}", m.target))
                                            .map(|u| u.host_str().unwrap().to_string())
                                    })
                                    .transpose()?
                                    .unwrap_or_else(|| host.to_string()),
                            )
                            .await?,
                    ),
                    None,
                ))
            } else {
                let mirror = settings.mirror(host, port, "https");
                let destination = mirror
                    .as_ref()
                    .map(|m| parse_url(&format!("https://{}", m.target)))
                    .transpose()?;
                let socket = self
                    .tunnel_transport(
                        destination
                            .as_ref()
                            .and_then(|u| u.host_str())
                            .unwrap_or(host),
                        destination.as_ref().and_then(|u| u.port()).unwrap_or(port),
                        settings.upstream(host, port).as_ref(),
                    )
                    .await?;
                Ok::<_, Error>((None, Some(socket)))
            }
        }
        .await;
        let (certificate, upstream) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                recording.finish(Some(error.message.clone()));
                return Err(error);
            }
        };
        if let Some(socket) = &upstream {
            recording.session.server_ip = socket.peer_addr().ok().map(|a| a.ip().to_string());
        }
        let upgraded = hyper::upgrade::on(&mut request);
        let engine = self.clone();
        self.tasks.spawn(async move {
            let _permit = permit;
            let operation = async {
                let upgraded = upgraded.await.map_err(Error::upstream)?;
                let socket = TokioIo::new(upgraded);
                if let Some(certificate) = certificate {
                    let secure = tokio::time::timeout(engine.config.timeout, TlsAcceptor::from(certificate).accept(socket)).await.map_err(|_| Error::new(504, "Client TLS handshake timed out"))??;
                    recording.session.status = 200;
                    recording.session.response_body = "[TLS interception established; decrypted requests appear as separate sessions]".into();
                    engine.clone().serve(Box::new(secure), false, None, client_ip, Some(target), None, cancel.clone(), application).await;
                } else {
                    recording.session.status = 200;
                    recording.session.response_body = tunnel_notice.into();
                    let mut socket = IdleIo::new(socket, engine.config.timeout);
                    let mut upstream = IdleIo::new(upstream.unwrap(), engine.config.timeout);
                    let (sent, received) = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
                    recording.request.length = sent as usize; recording.response.length = received as usize;
                }
                Ok::<_, Error>(())
            };
            let result = tokio::select! { _ = cancel.cancelled() => Err(Error::new(503, "Proxy stopped")), result = operation => result };
            recording.finish(result.err().map(|e| e.message));
        });
        Ok(Response::new(full(Bytes::new())))
    }
}

pub(crate) struct Running {
    pub(crate) engine: Arc<Engine>,
    control_task: JoinHandle<()>,
    descriptor: PathBuf,
    owned_descriptor: Vec<u8>,
    pub ready: Value,
}
impl Running {
    #[cfg(target_os = "android")]
    pub(crate) async fn vpn(
        &self,
        fd: i32,
        dns: std::net::IpAddr,
        cancel: CancellationToken,
    ) -> std::result::Result<(), String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let socks = tokio::spawn(crate::android_vpn::serve(
            listener,
            self.engine.clone(),
            cancel.clone(),
        ));
        let mut args = tun2proxy::Args::default();
        args.proxy =
            tun2proxy::ProxyParameters::try_from(format!("socks5://127.0.0.1:{port}").as_str())
                .map_err(|e| e.to_string())?;
        args.tun_fd = Some(fd);
        args.close_fd_on_drop = Some(false);
        args.dns = tun2proxy::ArgDns::Direct;
        args.dns_addr = dns;
        args.ipv6_enabled = true;
        args.max_sessions = 200;
        let result = tun2proxy::general_run_async(args, 1500, false, cancel.clone()).await;
        cancel.cancel();
        let _ = socks.await;
        result.map(|_| ()).map_err(|e| e.to_string())
    }

    pub async fn stop(self) {
        self.engine.shutdown.cancel();
        Engine::stop_proxy(&mut *self.engine.proxy.lock().await).await;
        self.engine.rules.close();
        self.engine.ui.close();
        self.control_task.abort();
        let _ = self.control_task.await;
        self.engine.tasks.close();
        self.engine.tasks.wait().await;
        self.engine.history.lock().unwrap().checkpoint();
        if std::fs::read(&self.descriptor).ok().as_deref() == Some(self.owned_descriptor.as_slice())
        {
            let _ = std::fs::remove_file(&self.descriptor);
        }
    }
}

pub(crate) async fn start_embedded(
    directory: PathBuf,
    port: Option<u16>,
    token: String,
) -> Result<Running> {
    start(Config::embedded(directory, port, token)?).await
}

async fn start(config: Config) -> Result<Running> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    private_dir(&config.directory)?;
    let authority = Authority::open(&config.directory).await?;
    let history = Arc::new(Mutex::new(History::open(
        &config.directory,
        config.sessions,
    )?));
    let rules = Rules::open(&config.directory, config.breakpoint_ms, config.script_ms);
    let client_tls = client_config(config.extra_ca.as_deref())?;
    let settings = SettingsStore::open(&config.directory);
    rules.configure(&settings.snapshot());
    history
        .lock()
        .unwrap()
        .configure(&settings.snapshot(), config.request)?;
    let engine = Arc::new(Engine {
        mcp_tokens: Mutex::new(crate::mcp::AccessTokens::load(&config.directory)?),
        proxy: tokio::sync::Mutex::new(Proxy {
            port: config
                .port
                .or(settings.snapshot().proxy_port)
                .unwrap_or(9988),
            running: false,
            intercept: settings.snapshot().intercept_tls,
            cancel: CancellationToken::new(),
            listener: None,
        }),
        config,
        authority,
        client_tls,
        history,
        rules,
        settings,
        ui: crate::ui_bridge::UIBridge::new(),
        settings_change: tokio::sync::Mutex::new(()),
        reverse_listeners: tokio::sync::Mutex::new(BTreeMap::new()),
        requests: Arc::new(Semaphore::new(100)),
        tunnels: Arc::new(Semaphore::new(100)),
        socks_udp: Arc::new(Semaphore::new(64)),
        shutdown: CancellationToken::new(),
        tasks: TaskTracker::new(),
        remote_mcp: Mutex::new(None),
    });
    let control = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|error| {
            Error::new(
                503,
                format!("Cannot listen on control address 127.0.0.1:0: {error}"),
            )
        })?;
    let control_port = control.local_addr()?.port();
    let initial_settings = engine.settings.snapshot();
    let remote_mcp = if initial_settings.mcp_remote_enabled {
        Some(Engine::bind_proxy(0, std::net::Ipv4Addr::UNSPECIFIED).await?)
    } else {
        None
    };
    let prepared = engine.prepare_reverse(&initial_settings).await?;
    let mut proxy = engine.proxy.lock().await;
    let listener = Engine::bind_proxy(proxy.port, engine.config.proxy_host).await?;
    let proxy_port = listener.local_addr()?.port();
    let descriptor = engine.config.directory.join("control.json");
    let owned_descriptor = serde_json::to_vec(
        &json!({"url":format!("http://127.0.0.1:{control_port}"),"token":engine.config.token}),
    )?;
    crate::storage::atomic_write(&descriptor, &owned_descriptor)?;
    engine.activate_proxy(&mut proxy, listener, proxy_port);
    proxy.running = true;
    proxy.cancel = engine.shutdown.child_token();
    drop(proxy);
    engine.activate_reverse(&initial_settings, prepared).await;
    engine.activate_remote_mcp(remote_mcp)?;
    let control_engine = engine.clone();
    let cancel = engine.shutdown.clone();
    let control_task = tokio::spawn(async move {
        control_engine.accept(control, true, cancel).await;
    });
    let ready = json!({"controlPort": control_port, "proxyPort": proxy_port, "caPath": engine.authority.ca_path});
    Ok(Running {
        engine,
        control_task,
        descriptor,
        owned_descriptor,
        ready,
    })
}

pub async fn run() -> Result<()> {
    let running = start(Config::load()?).await?;
    println!("{}", running.ready);
    use std::io::Write;
    std::io::stdout().flush()?;
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    running.stop().await;
    Ok(())
}
