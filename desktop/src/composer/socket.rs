use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use std::{thread, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{
    client_async_tls_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
        protocol::WebSocketConfig,
    },
};
use url::{Position, Url};

pub(super) const LIMIT: usize = 1024 * 1024;

pub(super) enum Event {
    Connected(String),
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    Closed,
}
pub(super) struct Socket {
    outgoing: mpsc::Sender<Message>,
    pub(super) events: mpsc::Receiver<Event>,
    cancel: Option<oneshot::Sender<()>>,
}
pub(super) struct Options {
    pub(super) url: String,
    pub(super) headers: String,
    pub(super) protocols: String,
    pub(super) proxy: Option<String>,
    pub(super) timeout: Duration,
    pub(super) ping: Option<Duration>,
}
impl Socket {
    pub(super) fn start(options: Options, ctx: egui::Context) -> Result<Self, String> {
        let (outgoing, mut commands) = mpsc::channel(4);
        let (events, incoming) = mpsc::channel(8);
        let (cancel, mut close) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        thread::Builder::new()
            .name("recho-websocket".into())
            .spawn(move || runtime.block_on(async {
                let result = tokio::select! {
                    _ = &mut close => return,
                    result = tokio::time::timeout(options.timeout, connect(&options)) => result,
                };
                let (mut socket, response) = match result {
                    Ok(Ok(connected)) => connected,
                    result => {
                        let error = match result {
                            Ok(Err(error)) => error,
                            Err(_) => "连接超时".into(),
                            _ => unreachable!(),
                        };
                        let _ = publish(&events, &ctx, Event::Error(error)).await;
                        return;
                    }
                };
                let response = response.headers().iter().filter_map(|(name,value)| {
                    value.to_str().ok().map(|value| format!("{name}: {value}"))
                }).collect::<Vec<_>>().join("\n");
                if !publish(&events, &ctx, Event::Connected(response)).await {
                    return;
                }
                let mut ping = tokio::time::interval(options.ping.unwrap_or(Duration::from_secs(3600)));
                ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                ping.tick().await;
                loop {
                    let result = tokio::select! {
                        _ = &mut close => {
                            let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
                            break;
                        }
                        message = commands.recv() => match message {
                            Some(message) => tokio::time::timeout(Duration::from_secs(2), socket.send(message))
                                .await.map_err(|_| "发送超时".to_string()).and_then(|result| result.map_err(|error| error.to_string())),
                            None => break,
                        },
                        _ = ping.tick(), if options.ping.is_some() => {
                            tokio::time::timeout(Duration::from_secs(2), socket.send(Message::Ping(Vec::new().into())))
                                .await.map_err(|_| "Ping 超时".to_string()).and_then(|result| result.map_err(|error| error.to_string()))
                        }
                        message = socket.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                if !publish(&events,&ctx,Event::Text(text.to_string())).await { break; }
                                Ok(())
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                if !publish(&events,&ctx,Event::Binary(bytes.to_vec())).await { break; }
                                Ok(())
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => Ok(()),
                            Some(Err(error)) => Err(error.to_string()),
                        },
                    };
                    if let Err(error) = result {
                        let _ = publish(&events, &ctx, Event::Error(error)).await;
                        return;
                    }
                }
                let _ = publish(&events, &ctx, Event::Closed).await;
            })).map_err(|error| error.to_string())?;
        Ok(Self {
            outgoing,
            events: incoming,
            cancel: Some(cancel),
        })
    }
    pub(super) fn send(&self, message: Message) -> Result<(), String> {
        self.outgoing
            .try_send(message)
            .map_err(|_| "连接已关闭或发送队列已满".into())
    }
    pub(super) fn disconnect(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

async fn publish(events: &mpsc::Sender<Event>, ctx: &egui::Context, event: Event) -> bool {
    ctx.request_repaint();
    match events.try_send(event) {
        Ok(()) => {
            ctx.request_repaint();
            true
        }
        // Stop a connection which is producing messages faster than the UI can present.
        // A bounded send also lets dropping the tab release the connection immediately.
        Err(mpsc::error::TrySendError::Full(event)) => {
            let ok = tokio::time::timeout(Duration::from_millis(350), events.send(event))
                .await
                .is_ok_and(|result| result.is_ok());
            ctx.request_repaint();
            ok
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

type Connection = (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    tokio_tungstenite::tungstenite::handshake::client::Response,
);

async fn connect(options: &Options) -> Result<Connection, String> {
    let url = Url::parse(&options.url).map_err(|error| error.to_string())?;
    let host = url.host_str().ok_or("请输入 WebSocket 主机")?;
    let port = match url.scheme() {
        "ws" => url.port().unwrap_or(80),
        "wss" => url.port().unwrap_or(443),
        _ => return Err("请输入 ws:// 或 wss:// URL".into()),
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("WebSocket URL 不能包含内嵌凭据或片段".into());
    }
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    for line in options
        .headers
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let (key, value) = line
            .split_once(':')
            .ok_or("握手消息头需要 Name: Value 格式")?;
        let key =
            HeaderName::from_bytes(key.trim().as_bytes()).map_err(|error| error.to_string())?;
        if [
            "host",
            "connection",
            "upgrade",
            "sec-websocket-key",
            "sec-websocket-version",
            "sec-websocket-extensions",
            "sec-websocket-protocol",
            "proxy-authorization",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("{key} 由 WebSocket 握手管理"));
        }
        request.headers_mut().append(
            key,
            HeaderValue::from_str(value.trim()).map_err(|error| error.to_string())?,
        );
    }
    if !options.protocols.trim().is_empty() {
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_str(options.protocols.trim()).map_err(|error| error.to_string())?,
        );
    }

    let socket = if let Some(proxy) = &options.proxy {
        let proxy = Url::parse(proxy).map_err(|error| error.to_string())?;
        if proxy.scheme() != "http"
            || proxy.host_str().is_none()
            || !proxy.username().is_empty()
            || proxy.password().is_some()
            || proxy.path() != "/"
            || proxy.query().is_some()
            || proxy.fragment().is_some()
        {
            return Err("代理地址应为 http://主机:端口".into());
        }
        let mut socket = TcpStream::connect((
            unbracket(proxy.host_str().unwrap()),
            proxy.port().unwrap_or(80),
        ))
        .await
        .map_err(|error| error.to_string())?;
        // CONNECT preserves TLS verification and is understood by the capture engine.
        let authority = format!("{}:{port}", &url[Position::BeforeHost..Position::AfterHost]);
        socket
            .write_all(
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            if response.len() >= 32768 {
                return Err("代理响应头超过 32 KiB".into());
            }
            response.push(socket.read_u8().await.map_err(|error| error.to_string())?);
        }
        let status = std::str::from_utf8(&response)
            .map_err(|error| error.to_string())?
            .lines()
            .next()
            .and_then(|line| line.split_ascii_whitespace().nth(1))
            .unwrap_or("");
        if status != "200" {
            return Err(format!("代理拒绝连接（{status}）"));
        }
        socket
    } else {
        TcpStream::connect((unbracket(host), port))
            .await
            .map_err(|error| error.to_string())?
    };
    socket
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    let config = WebSocketConfig::default()
        .read_buffer_size(4 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * LIMIT)
        .max_message_size(Some(LIMIT))
        .max_frame_size(Some(LIMIT));
    client_async_tls_with_config(request, socket, Some(config), None)
        .await
        .map_err(|error| error.to_string())
}

fn unbracket(host: &str) -> &str {
    host.trim_matches(['[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn exchanges_text_and_binary_and_releases_closed_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if message.is_close() {
                    break;
                }
                if message.is_text() || message.is_binary() {
                    ws.send(message).await.unwrap();
                }
            }
        });
        let mut socket = Socket::start(
            Options {
                url: format!("ws://127.0.0.1:{port}/echo"),
                headers: "X-Test: recho".into(),
                protocols: String::new(),
                proxy: None,
                timeout: Duration::from_secs(3),
                ping: None,
            },
            egui::Context::default(),
        )
        .unwrap();
        async fn receive(socket: &mut Socket) -> Event {
            tokio::time::timeout(Duration::from_secs(3), socket.events.recv())
                .await
                .unwrap()
                .unwrap()
        }
        assert!(matches!(receive(&mut socket).await, Event::Connected(_)));
        socket.send(Message::Text("测试文本".into())).unwrap();
        assert!(matches!(receive(&mut socket).await, Event::Text(text) if text == "测试文本"));
        socket
            .send(Message::Binary(vec![0, 255, 7].into()))
            .unwrap();
        assert!(matches!(receive(&mut socket).await, Event::Binary(bytes) if bytes == [0,255,7]));
        socket.disconnect();
        assert!(matches!(receive(&mut socket).await, Event::Closed));
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }
}
