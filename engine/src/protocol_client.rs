//! Explicit HTTP/2 and HTTP/3 API transport. Never silently downgrades.
use crate::wire::{self, Error, RequestData, ResponseData, Result};
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::ServerName;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, lookup_host},
};
use tokio_rustls::TlsConnector;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
struct Driver(tokio::task::JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn send(
    version: &str,
    req: &RequestData,
    body: Bytes,
    tls: Arc<rustls::ClientConfig>,
    limit: usize,
    timeout: Duration,
) -> Result<(ResponseData, Bytes)> {
    tokio::time::timeout(timeout, async {
        match version {
            "HTTP/2" => http2(req, body, tls, limit).await,
            "HTTP/3" => http3(req, body, tls, limit).await,
            _ => Err(Error::bad("Expected HTTP/2 or HTTP/3")),
        }
    })
    .await
    .map_err(|_| Error::new(504, format!("{version} request timed out")))?
}

async fn http2(
    req: &RequestData,
    body: Bytes,
    tls: Arc<rustls::ClientConfig>,
    limit: usize,
) -> Result<(ResponseData, Bytes)> {
    let url = wire::parse_url(&req.url)?;
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    let socket = TcpStream::connect((host, url.port_or_known_default().unwrap())).await?;
    let transport: Box<dyn Io> = if url.scheme() == "https" {
        let mut config = (*tls).clone();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let socket = TlsConnector::from(Arc::new(config))
            .connect(
                ServerName::try_from(host.to_string()).map_err(Error::bad)?,
                socket,
            )
            .await?;
        if socket.get_ref().1.alpn_protocol() != Some(b"h2") {
            return Err(Error::upstream("Server did not negotiate HTTP/2"));
        }
        Box::new(socket)
    } else {
        Box::new(socket)
    };
    let (mut sender, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(transport))
            .await
            .map_err(Error::upstream)?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let mut outgoing = Request::builder()
        .method(req.method.as_str())
        .uri(req.url.as_str())
        .version(hyper::Version::HTTP_2)
        .body(Full::new(body))
        .map_err(Error::bad)?;
    *outgoing.headers_mut() = wire::forward_headers(&req.headers)?;
    let response = sender
        .send_request(outgoing)
        .await
        .map_err(wire::network_error)?;
    let (parts, incoming) = response.into_parts();
    let bytes = http_body_util::Limited::new(incoming, limit)
        .collect()
        .await
        .map_err(Error::upstream)?
        .to_bytes();
    Ok((
        ResponseData {
            binary_body: None,
            status: parts.status.as_u16(),
            headers: wire::from_headers(&parts.headers),
            body: None,
        },
        bytes,
    ))
}

async fn http3(
    req: &RequestData,
    body: Bytes,
    tls: Arc<rustls::ClientConfig>,
    limit: usize,
) -> Result<(ResponseData, Bytes)> {
    let url = wire::parse_url(&req.url)?;
    if url.scheme() != "https" {
        return Err(Error::bad("HTTP/3 requires an HTTPS URL"));
    }
    let host = url.host_str().unwrap().trim_matches(['[', ']']);
    let address = lookup_host((host, url.port_or_known_default().unwrap()))
        .await?
        .next()
        .ok_or_else(|| Error::upstream("Host has no IP address"))?;
    let local = if address.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let mut endpoint = quinn::Endpoint::client(local.parse().unwrap())?;
    let mut config = (*tls).clone();
    config.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(config).map_err(Error::bad)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
    let connection = endpoint
        .connect(address, host)
        .map_err(Error::upstream)?
        .await
        .map_err(Error::upstream)?;
    let close = connection.clone();
    let (mut driver, mut sender) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .map_err(Error::upstream)?;
    let _driver = Driver(tokio::spawn(async move {
        let _ = driver.wait_idle().await;
    }));
    let mut outgoing = Request::builder()
        .method(req.method.as_str())
        .uri(req.url.as_str())
        .version(hyper::Version::HTTP_3)
        .body(())
        .map_err(Error::bad)?;
    *outgoing.headers_mut() = wire::forward_headers(&req.headers)?;
    outgoing.headers_mut().remove("host"); // HTTP/3 carries :authority.
    let mut stream = sender
        .send_request(outgoing)
        .await
        .map_err(Error::upstream)?;
    if !body.is_empty() {
        stream.send_data(body).await.map_err(Error::upstream)?;
    }
    stream.finish().await.map_err(Error::upstream)?;
    let response = stream.recv_response().await.map_err(Error::upstream)?;
    let mut bytes = BytesMut::new();
    while let Some(mut data) = stream.recv_data().await.map_err(Error::upstream)? {
        if data.remaining() > limit.saturating_sub(bytes.len()) {
            return Err(Error::new(413, "HTTP/3 body exceeds buffering limit"));
        }
        while data.has_remaining() {
            bytes.extend_from_slice(data.chunk());
            let count = data.chunk().len();
            data.advance(count);
        }
    }
    let result = (
        ResponseData {
            binary_body: None,
            status: response.status().as_u16(),
            headers: wire::from_headers(response.headers()),
            body: None,
        },
        bytes.freeze(),
    );
    close.close(0u32.into(), b"request complete");
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificates::{Authority, client_config};
    use hyper::{Response, service::service_fn};
    use std::convert::Infallible;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn http2_uses_real_frames_and_preserves_body_and_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(socket),
                    service_fn(|request: Request<hyper::body::Incoming>| async move {
                        assert_eq!(request.version(), hyper::Version::HTTP_2);
                        assert_eq!(request.uri().path(), "/echo");
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(201)
                                .header("x-protocol", "h2")
                                .body(Full::new(bytes))
                                .unwrap(),
                        )
                    }),
                )
                .await
                .ok();
        });
        let req = RequestData {
            binary_body: None,
            method: "POST".into(),
            url: format!("http://{address}/echo"),
            headers: Default::default(),
            body: None,
        };
        let (response, body) = send(
            "HTTP/2",
            &req,
            Bytes::from_static(b"h2-body"),
            client_config(None).unwrap(),
            1024,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(body, Bytes::from_static(b"h2-body"));
        server.abort();
    }

    #[tokio::test]
    async fn http3_uses_quic_and_verifies_local_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let authority = Authority::open(dir.path()).await.unwrap();
        let mut tls = (*authority.certificate("127.0.0.1").await.unwrap()).clone();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
        let endpoint = quinn::Endpoint::server(
            quinn::ServerConfig::with_crypto(Arc::new(crypto)),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let address = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut connection = h3::server::builder()
                .build::<_, Bytes>(h3_quinn::Connection::new(connection))
                .await
                .unwrap();
            while let Ok(Some(request)) = connection.accept().await {
                tokio::spawn(async move {
                    let (request, mut stream) = request.resolve_request().await.unwrap();
                    assert_eq!(request.uri().path(), "/echo");
                    let mut body = BytesMut::new();
                    while let Some(mut data) = stream.recv_data().await.unwrap() {
                        while data.has_remaining() {
                            let n = data.chunk().len();
                            body.extend_from_slice(data.chunk());
                            data.advance(n);
                        }
                    }
                    stream
                        .send_response(
                            Response::builder()
                                .status(202)
                                .header("x-protocol", "h3")
                                .body(())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    stream.send_data(body.freeze()).await.unwrap();
                    stream.finish().await.unwrap();
                });
            }
        });
        let request = RequestData {
            binary_body: None,
            method: "POST".into(),
            url: format!("https://{address}/echo"),
            headers: Default::default(),
            body: None,
        };
        let tls = client_config(Some(&authority.ca_path)).unwrap();
        let (response, body) = send(
            "HTTP/3",
            &request,
            Bytes::from_static(b"quic-body"),
            tls,
            1024,
            Duration::from_secs(8),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 202);
        assert_eq!(body, Bytes::from_static(b"quic-body"));
        server.abort();
    }
}
