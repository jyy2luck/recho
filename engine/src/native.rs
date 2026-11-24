//! Typed access for the in-process desktop client; the control API remains available.
use super::*;

#[path = "capture_conditions.rs"]
mod capture_conditions;
#[path = "har.rs"]
mod har;
pub use crate::certificates::PublicCertificate;
pub use crate::identity::CERTIFICATE_DOWNLOAD_URL;
pub use crate::local_network::proxy_addresses;
pub use capture_conditions::{Condition, ConditionSet, Field, Operator};

pub fn write_local_file(path: &std::path::Path, bytes: &[u8]) -> std::result::Result<(), String> {
    crate::storage::atomic_write(path, bytes).map_err(|error| error.to_string())
}

pub fn parse_header_lines(text: &str) -> std::result::Result<Value, String> {
    wire::parse_headers(text)
        .and_then(|headers| Ok(serde_json::to_value(headers)?))
        .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    Request,
    #[default]
    Response,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub duration_ms: u64,
    pub response_bytes: usize,
    pub host: String,
    pub application: String,
    pub server_ip: String,
    pub protocol: String,
    pub content_type: String,
    pub error: Option<String>,
    pub group: String,
    pub started_at: String,
    pub incognito: bool,
    pub list_matches: BTreeMap<String, bool>,
}
impl From<&crate::storage::Session> for Row {
    fn from(session: &crate::storage::Session) -> Self {
        Self {
            id: session.id.clone(),
            method: session.method.clone(),
            url: session.url.clone(),
            status: session.status,
            duration_ms: session.duration,
            response_bytes: session.response_bytes,
            host: url::Url::parse(&session.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default(),
            application: session
                .application
                .as_ref()
                .map(|app| app.name.clone())
                .unwrap_or_else(|| "未知来源".into()),
            server_ip: session.server_ip.clone().unwrap_or_default(),
            protocol: session.protocol.clone(),
            content_type: session
                .response_headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                        .map(|(_, value)| value.trim().to_lowercase())
                })
                .unwrap_or_default(),
            error: session.error.clone(),
            group: if session.capture_group.is_empty() {
                "default".into()
            } else {
                session.capture_group.clone()
            },
            started_at: session.started_at.clone(),
            incognito: session.incognito,
            list_matches: session.capture_list_matches.clone(),
        }
    }
}

pub struct Snapshot {
    pub revision: u64,
    pub rows: Option<Vec<Row>>,
    pub running: bool,
    pub port: u16,
    pub intercept_tls: bool,
    pub history_error: Option<String>,
    pub group: String,
    pub incognito: bool,
    pub enabled_list_filters: Vec<String>,
}

pub struct Detail {
    pub id: String,
    pub phase: Phase,
    pub headers: String,
    pub trailers: String,
    pub bytes: Vec<u8>,
    pub notice: Option<String>,
}

pub struct DesktopEngine {
    running: Running,
}
impl DesktopEngine {
    pub async fn start(
        directory: PathBuf,
        port: impl Into<Option<u16>>,
    ) -> std::result::Result<Self, String> {
        let settings = crate::settings::SettingsStore::open(&directory);
        let port = port.into().or(settings.snapshot().proxy_port).unwrap_or(0);
        start_embedded(
            directory,
            Some(port),
            uuid::Uuid::new_v4().simple().to_string(),
        )
        .await
        .map(|running| Self { running })
        .map_err(|e| e.to_string())
    }

    pub fn ca_path(&self) -> PathBuf {
        self.running.engine.authority.ca_path.clone()
    }

    /// Trusted desktop calls reuse the same validation and handlers as the authenticated HTTP API.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Value,
    ) -> std::result::Result<Value, String> {
        let result = async {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(full(Bytes::from(serde_json::to_vec(&body)?)))
                .map_err(Error::bad)?;
            let response = self.running.engine.clone().control_local(request).await;
            let status = response.status();
            let bytes = response.into_body().collect().await?.to_bytes();
            let value: Value = serde_json::from_slice(&bytes)?;
            if status.is_success() {
                Ok(value)
            } else {
                Err(Error::new(
                    status.as_u16(),
                    value["error"].as_str().unwrap_or("Request failed"),
                ))
            }
        };
        result
            .await
            .map_err(|error| format!("{}: {}", error.status, error.message))
    }

    pub fn pending_breakpoints(&self) -> Value {
        self.running.engine.rules.pending_summary()
    }

    pub fn pending_workspace_operations(&self) -> Value {
        self.running.engine.ui.pending()
    }

    pub async fn snapshot(&self, revision: Option<u64>) -> Snapshot {
        let engine = &self.running.engine;
        let proxy = engine.proxy.lock().await;
        let history = engine.history.lock().unwrap();
        let settings = engine.settings.snapshot();
        Snapshot {
            revision: history.revision,
            rows: (revision != Some(history.revision))
                .then(|| history.sessions.iter().rev().map(Row::from).collect()),
            running: proxy.running,
            port: proxy.port,
            intercept_tls: proxy.intercept,
            history_error: history.error.clone(),
            group: settings.capture_group,
            incognito: settings.incognito,
            enabled_list_filters: history
                .capture_list_filters
                .items
                .iter()
                .filter(|filter| filter.enabled)
                .map(|filter| filter.id.clone())
                .collect(),
        }
    }

    pub fn history_rows(
        &self,
        offset: usize,
        limit: usize,
        query: &str,
        group: Option<&str>,
    ) -> std::result::Result<Vec<Row>, String> {
        if !(1..=200).contains(&limit) || offset > i64::MAX as usize {
            return Err("Invalid history pagination".into());
        }
        self.running
            .engine
            .history
            .lock()
            .unwrap()
            .map_search(offset, limit, query, group, |session| Row::from(&session))
            .map_err(|error| error.to_string())
    }

    pub async fn configure(
        &self,
        running: bool,
        intercept_tls: bool,
    ) -> std::result::Result<(), String> {
        self.running
            .engine
            .configure_proxy(&json!({"running": running, "interceptTLS": intercept_tls}))
            .await
            .map_err(|e| e.to_string())
    }

    pub fn clear(&self) -> std::result::Result<(), String> {
        self.running.engine.history.lock().unwrap().clear_live();
        Ok(())
    }

    pub fn clear_group(&self, group: &str) -> std::result::Result<(), String> {
        self.running
            .engine
            .history
            .lock()
            .unwrap()
            .clear_group(group);
        Ok(())
    }

    pub async fn set_port(&self, port: u16) -> std::result::Result<(), String> {
        self.running
            .engine
            .configure_proxy(&json!({"port": port}))
            .await
            .map_err(|e| e.to_string())
    }

    pub fn detail(&self, id: &str, phase: Phase) -> std::result::Result<Detail, String> {
        let result = || -> Result<Detail> {
            let history = self.running.engine.history.lock().unwrap();
            let session = history
                .get(id)?
                .ok_or_else(|| Error::new(404, "Capture no longer available"))?;
            let (headers, trailers, preview, complete) = match phase {
                Phase::Request => (
                    session.request_headers,
                    session.request_trailers,
                    session.request_body,
                    session.request_body_complete,
                ),
                Phase::Response => (
                    session.response_headers,
                    session.response_trailers,
                    session.response_body,
                    session.response_body_complete,
                ),
            };
            let (bytes, notice) = if complete == Some(true) {
                let (bytes, parsed_headers) = history.body(id, phase.name())?;
                drop(history);
                (
                    wire::decoded(&bytes, &parsed_headers, self.running.engine.config.request)?,
                    None,
                )
            } else {
                (
                    preview.into_bytes(),
                    Some("仅显示预览：完整正文仍在接收、未归档或超过抓包上限。".into()),
                )
            };
            Ok(Detail {
                id: id.into(),
                phase,
                headers,
                trailers,
                bytes,
                notice,
            })
        };
        result().map_err(|e| e.to_string())
    }

    pub async fn stop(self) {
        self.running.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn typed_capture_preserves_archives_and_lifecycle() {
        let directory = tempfile::tempdir().unwrap();
        let engine = DesktopEngine::start(directory.path().into(), 0)
            .await
            .unwrap();
        let initial = engine.snapshot(None).await;
        assert!(initial.running && initial.rows.unwrap().is_empty());
        assert!(engine.snapshot(Some(initial.revision)).await.rows.is_none());
        let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        let body = "archive tail: 你好\n".repeat(6000);
        let expected = body.clone();
        let serving = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.unwrap();
            let mut input = [0; 4096];
            socket.read(&mut input).await.unwrap();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });
        let mut client = TcpStream::connect(("127.0.0.1", initial.port))
            .await
            .unwrap();
        client.write_all(format!("GET http://127.0.0.1:{origin_port}/native HTTP/1.1\r\nHost: 127.0.0.1:{origin_port}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        serving.await.unwrap();
        assert!(response.ends_with(expected.as_bytes()));
        let snapshot = engine.snapshot(Some(initial.revision)).await;
        let rows = snapshot.rows.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, 200);
        let detail = engine.detail(&rows[0].id, Phase::Response).unwrap();
        assert!(detail.notice.is_none());
        assert_eq!(detail.bytes, expected.as_bytes());
        assert!(engine.detail("../../missing", Phase::Response).is_err());
        engine.configure(false, false).await.unwrap();
        assert!(!engine.snapshot(Some(snapshot.revision)).await.running);
        engine.configure(true, false).await.unwrap();
        assert!(engine.snapshot(Some(snapshot.revision)).await.running);
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert!(
            engine
                .set_port(occupied.local_addr().unwrap().port())
                .await
                .is_err()
        );
        assert_eq!(engine.snapshot(None).await.port, initial.port);
        engine.clear().unwrap();
        assert!(
            engine
                .snapshot(Some(snapshot.revision))
                .await
                .rows
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            engine.detail(&rows[0].id, Phase::Response).unwrap().bytes,
            expected.as_bytes()
        );
        engine.stop().await;
        assert!(!directory.path().join("control.json").exists());
        std::net::TcpListener::bind(("0.0.0.0", initial.port)).unwrap();
    }
}
