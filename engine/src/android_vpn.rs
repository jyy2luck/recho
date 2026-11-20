//! Android's VPN excludes our UID, so proxy, DNS and UDP sockets cannot re-enter the TUN.
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
#[cfg(any(target_os = "android", test))]
use tokio::net::TcpListener;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::Semaphore,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Destination {
    pub host: String,
    pub port: u16,
}

fn address(bytes: &[u8]) -> io::Result<(Destination, usize)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid SOCKS address");
    let (host, end) = match bytes.first().copied() {
        Some(1) if bytes.len() >= 7 => (
            std::net::Ipv4Addr::from(<[u8; 4]>::try_from(&bytes[1..5]).unwrap()).to_string(),
            5,
        ),
        Some(4) if bytes.len() >= 19 => (
            std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&bytes[1..17]).unwrap()).to_string(),
            17,
        ),
        Some(3) if bytes.len() >= 2 && bytes[1] > 0 && bytes.len() >= bytes[1] as usize + 4 => {
            let end = bytes[1] as usize + 2;
            let host = std::str::from_utf8(&bytes[2..end]).map_err(|_| invalid())?;
            if host
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b".-_".contains(&b))
            {
                return Err(invalid());
            }
            (host.to_string(), end)
        }
        _ => return Err(invalid()),
    };
    let port = u16::from_be_bytes(bytes[end..end + 2].try_into().unwrap());
    Ok((Destination { host, port }, end + 2))
}

async fn read_address(stream: &mut TcpStream) -> io::Result<Destination> {
    let kind = stream.read_u8().await?;
    let mut bytes = vec![kind];
    let count = match kind {
        1 => 6,
        4 => 18,
        3 => {
            let size = stream.read_u8().await?;
            bytes.push(size);
            size as usize + 2
        }
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    let offset = bytes.len();
    bytes.resize(offset + count, 0);
    stream.read_exact(&mut bytes[offset..]).await?;
    Ok(address(&bytes)?.0)
}

async fn reply(stream: &mut TcpStream, port: u16) -> io::Result<()> {
    let mut bytes = [5, 0, 0, 1, 127, 0, 0, 1, 0, 0];
    bytes[8..].copy_from_slice(&port.to_be_bytes());
    stream.write_all(&bytes).await
}

async fn handshake(stream: &mut TcpStream) -> io::Result<(u8, Destination)> {
    if stream.read_u8().await? != 5 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let count = stream.read_u8().await?;
    let mut methods = vec![0; count as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 255]).await?;
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    stream.write_all(&[5, 0]).await?;
    let mut header = [0; 3];
    stream.read_exact(&mut header).await?;
    if header[0] != 5 || header[2] != 0 || ![1, 3].contains(&header[1]) {
        stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok((header[1], read_address(stream).await?))
}

async fn udp_association(mut stream: TcpStream, permits: Arc<Semaphore>) -> io::Result<()> {
    let socket = Arc::new(UdpSocket::bind(("127.0.0.1", 0)).await?);
    reply(&mut stream, socket.local_addr()?.port()).await?;
    let mut client: Option<SocketAddr> = None;
    let mut packets = [0; 65535];
    let mut tasks = JoinSet::new();
    let mut flows =
        std::collections::BTreeMap::<Destination, tokio::sync::mpsc::Sender<Vec<u8>>>::new();
    let mut closed = [0; 1];
    loop {
        tokio::select! {
            _ = stream.read(&mut closed) => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            packet = socket.recv_from(&mut packets) => {
                let (size, sender) = packet?;
                if !sender.ip().is_loopback() || client.is_some_and(|v| v != sender) { continue; }
                if size < 4 || packets[..3] != [0, 0, 0] { continue; }
                let Ok((destination, end)) = address(&packets[3..size]) else { continue; };
                if destination.port == 0 { continue; }
                client = Some(sender);
                let payload = packets[3 + end..size].to_vec();
                flows.retain(|_, queue| !queue.is_closed());
                if let Some(queue) = flows.get(&destination) {
                    let _ = queue.try_send(payload);
                    continue;
                }
                if flows.len() >= 64 { continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let prefix = packets[..3 + end].to_vec();
                let (queue, mut pending) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                let _ = queue.try_send(payload);
                flows.insert(destination.clone(), queue);
                let socket = socket.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let operation = async {
                        let peer = tokio::net::lookup_host((destination.host.as_str(), destination.port))
                            .await?.next().ok_or(io::ErrorKind::NotFound)?;
                        let upstream = UdpSocket::bind(if peer.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).await?;
                        upstream.connect(peer).await?;
                        let mut response = vec![0; 65535];
                        loop {
                            let active = async {
                                tokio::select! {
                                    payload = pending.recv() => {
                                        let Some(payload) = payload else { return Ok::<_, io::Error>(false); };
                                        upstream.send(&payload).await?;
                                    },
                                    received = upstream.recv(&mut response) => {
                                        let length = received?;
                                        let mut packet = prefix.clone();
                                        packet.extend_from_slice(&response[..length]);
                                        socket.send_to(&packet, sender).await?;
                                    }
                                }
                                Ok(true)
                            };
                            match tokio::time::timeout(Duration::from_secs(60), active).await {
                                Ok(Ok(true)) => {},
                                Ok(Err(error)) => return Err(error),
                                _ => break,
                            }
                        }
                        Ok::<_, io::Error>(())
                    };
                    let _ = operation.await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

pub(crate) async fn serve_connection(
    mut stream: TcpStream,
    engine: Arc<crate::server::Engine>,
    cancel: CancellationToken,
    udp_permits: Arc<Semaphore>,
) {
    let operation = async {
        let result = tokio::time::timeout(Duration::from_secs(5), handshake(&mut stream)).await;
        if let Ok(Ok((command, destination))) = result {
            if command == 3 {
                let _ = udp_association(stream, udp_permits).await;
            } else if destination.port > 0 && reply(&mut stream, 0).await.is_ok() {
                if let Err(error) = engine.vpn_tcp(stream, destination, cancel.clone()).await {
                    eprintln!("SOCKS TCP: {error}");
                }
            }
        }
    };
    tokio::select! { _ = cancel.cancelled() => {}, _ = operation => {} }
}

#[cfg(any(target_os = "android", test))]
pub(crate) async fn serve(
    listener: TcpListener,
    engine: Arc<crate::server::Engine>,
    cancel: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(200));
    let udp_permits = Arc::new(Semaphore::new(64));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            incoming = listener.accept() => {
                let Ok((stream, peer)) = incoming else { break; };
                if !peer.ip().is_loopback() { continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let engine = engine.clone();
                let cancel = cancel.clone();
                let udp_permits = udp_permits.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    serve_connection(stream, engine, cancel, udp_permits).await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[cfg(target_os = "android")]
mod native {
    use super::*;
    use std::sync::Mutex;
    static TUNNEL: Mutex<Option<CancellationToken>> = Mutex::new(None);

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_local_recho_recho_CaptureService_prepareTunnel(
        _: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
    ) -> i32 {
        let Ok(mut active) = TUNNEL.lock() else {
            return -1;
        };
        if active.is_some() {
            return -1;
        }
        *active = Some(CancellationToken::new());
        0
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_local_recho_recho_CaptureService_runTunnel(
        _: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
        fd: i32,
        dns_high: i64,
        dns_low: i64,
        dns_v6: u8,
    ) -> i32 {
        let cancel = {
            let Ok(active) = TUNNEL.lock() else {
                return -1;
            };
            let Some(cancel) = active.as_ref() else {
                return -1;
            };
            cancel.clone()
        };
        let dns = if dns_v6 != 0 {
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                ((dns_high as u128) << 64) | dns_low as u64 as u128,
            ))
        } else {
            std::net::IpAddr::V4(std::net::Ipv4Addr::from(dns_low as u32))
        };
        let result = if fd < 0 {
            Err("VPN interface was not established".into())
        } else {
            crate::embedded::run_vpn(fd, dns, cancel)
        };
        if let Ok(mut active) = TUNNEL.lock() {
            *active = None;
        }
        match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("VPN failed: {error}");
                -1
            }
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn Java_local_recho_recho_CaptureService_stopTunnel(
        _: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
    ) {
        if let Ok(active) = TUNNEL.lock() {
            if let Some(cancel) = active.as_ref() {
                cancel.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_addresses_reject_truncation_and_invalid_domains() {
        let domain = b"\x03\x0bexample.com\x01\xbb";
        assert_eq!(
            address(domain).unwrap(),
            (
                Destination {
                    host: "example.com".into(),
                    port: 443
                },
                domain.len()
            )
        );
        for size in 0..domain.len() {
            assert!(address(&domain[..size]).is_err());
        }
        assert!(address(b"\x03\x03a/b\x00\x50").is_err());
        assert_eq!(
            address(&[1, 127, 0, 0, 1, 0, 80]).unwrap().0.host,
            "127.0.0.1"
        );
        assert_eq!(
            address(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 80])
                .unwrap()
                .0
                .host,
            "::1"
        );
    }

    #[tokio::test]
    async fn socks_http_uses_shared_engine_and_cancellation_releases_listener() {
        let data = tempfile::tempdir().unwrap();
        let running =
            crate::server::start_embedded(data.path().to_owned(), Some(0), "vpn-test-token".into())
                .await
                .unwrap();
        let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = origin.local_addr().unwrap().port();
        let received = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            assert!(head.starts_with(b"GET /vpn HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nvpn-ok",
                )
                .await
                .unwrap();
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(serve(listener, running.engine.clone(), cancel.clone()));
        let mut client = TcpStream::connect(local).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0; 2];
        client.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);
        let mut connect = vec![5, 1, 0, 1, 127, 0, 0, 1];
        connect.extend_from_slice(&port.to_be_bytes());
        client.write_all(&connect).await.unwrap();
        let mut ready = [0; 10];
        client.read_exact(&mut ready).await.unwrap();
        assert_eq!(ready[..4], [5, 0, 0, 1]);
        client.write_all(b"G").await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        client
            .write_all(
                format!("ET /vpn HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.ends_with("vpn-ok"));
        received.await.unwrap();
        cancel.cancel();
        task.await.unwrap();
        assert!(TcpListener::bind(local).await.is_ok());
        running.stop().await;
        assert!(data.path().join("history.sqlite").exists());
    }

    #[tokio::test]
    async fn udp_payload_round_trips_and_closes_with_control_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let task = tokio::spawn(udp_association(server, Arc::new(Semaphore::new(64))));
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        let port = u16::from_be_bytes(reply[8..].try_into().unwrap());
        let upstream = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let echo_port = upstream.local_addr().unwrap().port();
        let echo = tokio::spawn(async move {
            let mut data = [0; 512];
            let mut source = None;
            for _ in 0..2 {
                let (size, peer) = upstream.recv_from(&mut data).await.unwrap();
                if let Some(previous) = source {
                    assert_eq!(peer, previous);
                }
                source = Some(peer);
                for _ in 0..2 {
                    upstream.send_to(&data[..size], peer).await.unwrap();
                }
            }
        });
        let sender = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
        packet.extend_from_slice(&echo_port.to_be_bytes());
        packet.extend_from_slice(b"dns-or-udp");
        let mut data = [0; 512];
        for _ in 0..2 {
            sender.send_to(&packet, ("127.0.0.1", port)).await.unwrap();
            for _ in 0..2 {
                let size = tokio::time::timeout(Duration::from_secs(2), sender.recv(&mut data))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(&data[..size], packet.as_slice());
            }
        }
        echo.await.unwrap();
        drop(client);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
