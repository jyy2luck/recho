use super::*;
use tokio::net::TcpListener;

fn tls() -> Arc<ClientConfig> {
    crate::certificates::client_config(None).unwrap()
}

async fn fixture(
    response: &'static [u8],
    hold_open: bool,
) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).await.unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&bytes[..n]);
            if let Some(end) = received.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&received[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if received.len() >= end + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(response).await.unwrap();
        socket.flush().await.unwrap();
        if hold_open {
            let _ = socket.read(&mut [0; 1]).await;
        }
        received
    });
    (url, server)
}

#[tokio::test]
async fn raw_bytes_preserve_custom_method_target_header_order_duplicates_and_body() {
    let (url, server) = fixture(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nX-Result: first\r\nX-Result: second\r\n\r\nOK", true).await;
    let body = "\n精确正文\r\n\n";
    let raw = format!(
        "CUSTOM  /a%2fb?x=1&x=%26  HTTP/1.1\nHost: custom.test\nX-Case:  one \nX-Case: two\nContent-Length: {}\n\n{body}",
        body.len()
    );
    let session = request(
        json!({"url":url,"raw":raw}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap();
    let received = server.await.unwrap();
    let expected = format!(
        "CUSTOM  /a%2fb?x=1&x=%26  HTTP/1.1\r\nHost: custom.test\r\nX-Case:  one \r\nX-Case: two\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert_eq!(received, expected.as_bytes());
    assert_eq!(session["method"], "CUSTOM");
    assert_eq!(session["status"], 201);
    assert_eq!(session["responseBody"], "OK");
    assert_eq!(
        session["responseHeaders"],
        "Content-Length: 2\r\nX-Result: first\r\nX-Result: second"
    );
}

#[tokio::test]
async fn informational_chunked_response_and_trailers_are_decoded_without_waiting_for_close() {
    let (url, server) = fixture(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n3;ext=value\r\none\r\n3\r\ntwo\r\n0\r\nX-Trailer: done\r\n\r\n", true).await;
    let session = request(
        json!({"url":url,"raw":"GET / HTTP/1.1\n\n"}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap();
    assert_eq!(session["responseBody"], "onetwo");
    assert_eq!(session["responseTrailers"], "X-Trailer: done");
    assert!(
        String::from_utf8(server.await.unwrap())
            .unwrap()
            .contains("Host: 127.0.0.1:")
    );
}

#[tokio::test]
async fn recording_archives_complete_body_bytes_and_preserves_full_inline_response() {
    let (url, server) = fixture(
        b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nfull-response",
        true,
    )
    .await;
    let mut session = request(
        json!({"url":url,"raw":"POST / HTTP/1.1\nX-Duplicate: a\nX-Duplicate: b\n\nrequest-body"}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap();
    server.await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let history = Arc::new(Mutex::new(History::open(directory.path(), 100).unwrap()));
    record(&mut session, history.clone(), 4).unwrap();
    let id = session["id"].as_str().unwrap();
    let history = history.lock().unwrap();
    let saved = history.get(id).unwrap().unwrap();
    assert_eq!(saved.source, "raw-http");
    assert_eq!(saved.request_body_complete, Some(true));
    assert_eq!(saved.response_body_complete, Some(true));
    assert_eq!(history.body(id, "request").unwrap().0, b"request-body");
    assert_eq!(history.body(id, "response").unwrap().0, b"full-response");
    assert_eq!(session["responseBody"], "full-response");
    assert!(
        session.get("requestBodyBase64").is_none() && session.get("responseBodyBase64").is_none()
    );
    assert!(history.error.is_none());
}

#[tokio::test]
async fn script_edits_only_changed_headers_and_keeps_raw_target_and_duplicates() {
    let (url, server) = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nyes", true).await;
    let session = request(json!({"url":url,"raw":"GET /kept%2Fvalue HTTP/1.1\nX-Keep: one\nX-Keep: two\n\n","variables":{"token":"secret"},"beforeScript":"request.headers['x-token']=environment.token; request.body='changed';", "afterScript":"pm.test('body',()=>assert(response.body==='yes')); pm.environment.set('next','done');"}), tls(), 1024, Duration::from_secs(3), "", 1000).await.unwrap();
    let received = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(received.starts_with("GET /kept%2Fvalue HTTP/1.1\r\nX-Keep: one\r\nX-Keep: two\r\n"));
    assert!(received.contains("x-token: secret\r\n"));
    assert!(received.ends_with("\r\n\r\nchanged"));
    assert_eq!(session["tests"][0]["passed"], true);
    assert_eq!(session["variables"]["next"], "done");
}

#[tokio::test]
async fn python_hooks_use_raw_target_and_preserve_binary_body_and_shared_environment() {
    let (url, server) = fixture(
        b"HTTP/1.1 202 Accepted\r\nContent-Length: 3\r\n\r\nyes",
        true,
    )
    .await;
    let python = "from recho import *\ndef onRequest(context, request):\n    assert request.path == '/raw%2Fpath'\n    request.headers['X-Python'] = 'yes'\n    request.body = bytes([0,255,10,13])\n    context.shared = {'ok': 'kept'}\n    print('before')\n    return request\ndef onResponse(context, response):\n    assert context.shared['ok'] == 'kept'\n    response.body = 'changed-response'\n    print('after')\n    return response\n";
    let session = request(json!({"url":url,"raw":"POST /raw%2Fpath HTTP/1.1\nX-Keep: one\nX-Keep: two\n\n","pythonScript":python}), tls(), 1024, Duration::from_secs(5), "", 2000).await.unwrap();
    let received = server.await.unwrap();
    assert!(received.starts_with(b"POST /raw%2Fpath HTTP/1.1\r\nX-Keep: one\r\nX-Keep: two\r\n"));
    assert!(received.ends_with(&[0, 255, 10, 13]));
    assert_eq!(session["responseBody"], "changed-response");
    let logs = session["scriptLogs"].as_array().unwrap();
    assert!(logs.iter().any(|s| s.as_str().unwrap().contains("before")));
    assert!(logs.iter().any(|s| s.as_str().unwrap().contains("after")));
}

#[tokio::test]
async fn https_uses_http1_and_verification_is_explicit_and_local() {
    let directory = tempfile::tempdir().unwrap();
    let authority = crate::certificates::Authority::open(directory.path())
        .await
        .unwrap();
    let server_tls = authority.certificate("127.0.0.1").await.unwrap();
    let trusted = crate::certificates::client_config(Some(&authority.ca_path)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut completed = 0;
        for _ in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            let Ok(mut secure) = tokio_rustls::TlsAcceptor::from(server_tls.clone())
                .accept(socket)
                .await
            else {
                continue;
            };
            assert_eq!(
                secure.get_ref().1.alpn_protocol(),
                Some(b"http/1.1".as_slice())
            );
            let mut head = Vec::new();
            while !head.windows(4).any(|b| b == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let n = secure.read(&mut bytes).await.unwrap();
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&bytes[..n]);
            }
            secure
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecure")
                .await
                .unwrap();
            completed += 1;
        }
        completed
    });
    let input = json!({"url":url,"raw":"GET / HTTP/1.1\n\n"});
    assert!(
        request(input.clone(), tls(), 1024, Duration::from_secs(5), "", 1000)
            .await
            .is_err()
    );
    let mut unverified = input.clone();
    unverified["verifyTLS"] = json!(false);
    assert_eq!(
        request(unverified, tls(), 1024, Duration::from_secs(5), "", 1000)
            .await
            .unwrap()["responseBody"],
        "secure"
    );
    assert_eq!(
        request(input, trusted, 1024, Duration::from_secs(5), "", 1000)
            .await
            .unwrap()["responseBody"],
        "secure"
    );
    assert_eq!(server.await.unwrap(), 2);
}

#[tokio::test]
async fn cancellation_drops_a_stalled_connection_and_cleans_registry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let id = uuid::Uuid::new_v4().to_string();
    let cancel_id = id.clone();
    let sender = tokio::spawn(async move {
        request(
            json!({"url":url,"raw":"GET / HTTP/1.1\n\n","requestID":id}),
            tls(),
            1024,
            Duration::from_secs(20),
            "",
            1000,
        )
        .await
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let _ = socket.read(&mut [0; 1024]).await.unwrap();
    let duplicate = request(
        json!({"requestID":cancel_id}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap_err();
    assert!(duplicate.message.contains("already running"));
    assert!(cancel(&cancel_id));
    assert_eq!(sender.await.unwrap().unwrap_err().status, 499);
    assert!(!requests().lock().unwrap().active.contains_key(&cancel_id));
    assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
}

#[tokio::test]
async fn cancellation_before_send_registration_and_oversized_input_are_bounded() {
    let id = uuid::Uuid::new_v4().to_string();
    assert!(cancel(&id));
    let error = request(
        json!({"requestID":id}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, 499);
    let error = request(json!({"url":"http://127.0.0.1:1","raw":format!("POST / HTTP/1.1\n\n{}", "x".repeat(1025))}), tls(), 1024, Duration::from_secs(3), "", 1000).await.unwrap_err();
    assert_eq!(error.status, 413);
    for n in 0..1030 {
        assert!(cancel(&format!("pending-test-{n}")));
    }
    assert!(requests().lock().unwrap().pending_cancel.len() <= 1024);
}

#[tokio::test]
async fn truncated_or_oversized_response_is_reported_and_timeout_is_bounded() {
    let (url, server) = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\nshort", false).await;
    let error = request(
        json!({"url":url,"raw":"GET / HTTP/1.1\n\n"}),
        tls(),
        1024,
        Duration::from_secs(3),
        "",
        1000,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("ended early"));
    server.await.unwrap();
    let (url, server) = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 2000\r\n\r\n", false).await;
    assert_eq!(
        request(
            json!({"url":url,"raw":"GET / HTTP/1.1\n\n"}),
            tls(),
            1024,
            Duration::from_secs(3),
            "",
            1000
        )
        .await
        .unwrap_err()
        .status,
        413
    );
    server.await.unwrap();
    let (url, server) = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n", true).await;
    assert_eq!(
        request(
            json!({"url":url,"raw":"GET / HTTP/1.1\n\n","timeout":0.05}),
            tls(),
            1024,
            Duration::from_secs(3),
            "",
            1000
        )
        .await
        .unwrap_err()
        .status,
        504
    );
    server.await.unwrap();
}

#[test]
fn raw_parse_rejects_http2_and_keeps_all_body_line_endings() {
    assert!(Message::parse("GET / HTTP/2\n\n").is_err());
    let message =
        Message::parse("POST / HTTP/1.1\r\nX-Duplicate: a\r\nX-Duplicate: b\r\n\r\n\nbody\r\n")
            .unwrap();
    assert_eq!(message.body, b"\nbody\r\n");
    assert_eq!(message.headers, vec!["X-Duplicate: a", "X-Duplicate: b"]);
}
