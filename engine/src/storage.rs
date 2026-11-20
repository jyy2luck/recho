use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::wire::{Capture, Headers, Result};

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    #[cfg(windows)]
    private_windows_path(path)?;
    Ok(())
}
pub fn private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    #[cfg(windows)]
    private_windows_path(path)?;
    Ok(())
}

#[cfg(windows)]
fn private_windows_path(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT,
                SetNamedSecurityInfoW,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
    };
    // Owner Rights follows the file's owner; inherited ACLs cannot expose tokens or CA keys.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)\0"
        .encode_utf16()
        .collect();
    let mut name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut acl = std::ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        let result =
            if GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                let status = SetNamedSecurityInfoW(
                    name.as_mut_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    acl,
                    std::ptr::null_mut(),
                );
                if status == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::from_raw_os_error(status as i32))
                }
            };
        LocalFree(descriptor);
        result?;
    }
    Ok(())
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        private_file(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub source: String,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub request_headers: String,
    pub response_headers: String,
    pub request_body: String,
    pub response_body: String,
    pub started_at: String,
    pub duration: u64,
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub error: Option<String>,
    pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<crate::application::Application>,
    #[serde(default, rename = "serverIP", skip_serializing_if = "Option::is_none")]
    pub server_ip: Option<String>,
    #[serde(default, rename = "clientIP", skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interceptors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grpc_messages: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_trailers: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub response_trailers: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub websocket_frames: Vec<crate::websocket::Frame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_logs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub protocol: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub capture_group: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incognito: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capture_list_matches: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_list_filter_error: Option<String>,
}

pub struct History {
    db: Connection,
    pub sessions: Vec<Session>,
    epoch: Uuid,
    pub revision: u64,
    session_revisions: BTreeMap<String, u64>,
    pub capture_list_filters: crate::capture_filters::Filters,
    limit: usize,
    pub error: Option<String>,
    directory: PathBuf,
    incognito: bool,
    capture_group: String,
    retention_days: u32,
    full_limit: usize,
    volatile_bodies: BTreeMap<(String, String), Vec<u8>>,
}
impl History {
    pub fn open(directory: &Path, limit: usize) -> Result<Self> {
        private_dir(directory)?;
        let path = directory.join("history.sqlite");
        let db = Connection::open(&path)?;
        private_file(&path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, started_at TEXT NOT NULL, document TEXT NOT NULL); CREATE INDEX IF NOT EXISTS sessions_started_at ON sessions(started_at DESC);")?;
        let history = Self {
            db,
            sessions: vec![],
            epoch: Uuid::new_v4(),
            revision: 0,
            session_revisions: BTreeMap::new(),
            capture_list_filters: crate::capture_filters::Filters::load(directory)?,
            limit,
            error: None,
            directory: directory.to_path_buf(),
            incognito: false,
            capture_group: "default".into(),
            retention_days: 0,
            full_limit: 16 * 1024 * 1024,
            volatile_bodies: BTreeMap::new(),
        };
        private_dir(&directory.join("bodies"))?;
        // The live buffer belongs to this engine run. Saved records remain
        // available through page/search/get, without replaying them as live.
        Ok(history)
    }
    #[cfg(test)]
    pub fn page(&self, offset: usize, limit: usize) -> Result<Vec<Session>> {
        self.search(offset, limit, "")
    }
    pub fn search(&self, offset: usize, limit: usize, query: &str) -> Result<Vec<Session>> {
        // shortcut: substring search scans saved previews; use an FTS index if large histories stall capture.
        let mut statement = self.db.prepare(
            "SELECT document FROM sessions WHERE ?1 = '' OR instr(lower(document), lower(?1)) > 0 ORDER BY started_at DESC, rowid DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(params![query, limit as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn insert(&mut self, session: &Session) {
        if let Err(error) = self.prune() {
            self.error = Some(format!("Cannot apply retention policy: {error}"));
        }
        self.revision += 1;
        self.session_revisions
            .insert(session.id.clone(), self.revision);
        self.sessions.insert(0, session.clone());
        for removed in self.sessions.drain(self.limit.min(self.sessions.len())..) {
            self.session_revisions.remove(&removed.id);
        }
        if session.incognito {
            return;
        }
        if let Err(error) = self.db.execute(
            "INSERT INTO sessions(id, started_at, document) VALUES (?, ?, ?)",
            params![
                session.id,
                session.started_at,
                serde_json::to_string(session).unwrap()
            ],
        ) {
            self.error = Some(format!("Cannot save request history: {error}"));
        }
    }
    pub fn save(&mut self, session: &Session) {
        if let Some(item) = self.sessions.iter_mut().find(|s| s.id == session.id) {
            *item = session.clone();
            self.revision += 1;
            self.session_revisions
                .insert(session.id.clone(), self.revision);
        }
        if session.incognito {
            return;
        }
        if let Err(error) = self.db.execute(
            "UPDATE sessions SET document = ? WHERE id = ?",
            params![serde_json::to_string(session).unwrap(), session.id],
        ) {
            self.error = Some(format!("Cannot save request history: {error}"));
        }
    }
    pub fn clear_live(&mut self) {
        self.sessions.clear();
        self.revision += 1;
        self.session_revisions.clear();
        self.volatile_bodies.clear();
    }
    pub fn clear(&mut self) -> Result<()> {
        self.db.execute_batch("DELETE FROM sessions; PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        self.sessions.clear();
        self.revision += 1;
        self.session_revisions.clear();
        self.volatile_bodies.clear();
        for entry in fs::read_dir(self.directory.join("bodies"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
        }
        self.error = None;
        Ok(())
    }
    pub fn configure(
        &mut self,
        settings: &crate::settings::Settings,
        full_limit: usize,
    ) -> Result<()> {
        self.incognito = settings.incognito;
        self.capture_group = settings.capture_group.clone();
        self.retention_days = settings.retention_days;
        self.full_limit = full_limit;
        self.prune()
    }
    pub fn cursor(&self) -> String {
        format!("{}:{}", self.epoch, self.revision)
    }
    pub fn cursor_revision(&self, cursor: &str) -> Option<u64> {
        let (epoch, revision) = cursor.split_once(':')?;
        let revision = revision.parse::<u64>().ok()?;
        (Uuid::parse_str(epoch).ok()? == self.epoch && revision <= self.revision)
            .then_some(revision)
    }
    pub fn sessions_since(&self, revision: u64) -> Vec<&Session> {
        self.sessions
            .iter()
            .filter(|s| self.session_revisions.get(&s.id).copied().unwrap_or(0) > revision)
            .collect()
    }
    pub fn prune(&mut self) -> Result<()> {
        if self.retention_days == 0 {
            return Ok(());
        }
        let before = (Utc::now() - chrono::Duration::days(i64::from(self.retention_days)))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let ids = {
            let mut statement = self
                .db
                .prepare("SELECT id FROM sessions WHERE started_at < ?")?;
            statement
                .query_map([&before], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for id in ids {
            self.delete(&id)?;
        }
        Ok(())
    }
    pub fn delete(&mut self, id: &str) -> Result<()> {
        if id.contains('/') || id.contains('\\') {
            return Err(crate::wire::Error::bad("Invalid history ID"));
        }
        self.db.execute("DELETE FROM sessions WHERE id=?", [id])?;
        self.sessions.retain(|s| s.id != id);
        self.revision += 1;
        self.session_revisions.remove(id);
        self.volatile_bodies.retain(|(key, _), _| key != id);
        for phase in ["request", "response"] {
            let path = self.body_path(id, phase);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Option<Session>> {
        if let Some(session) = self.sessions.iter().find(|s| s.id == id) {
            return Ok(Some(session.clone()));
        }
        let mut statement = self
            .db
            .prepare("SELECT document FROM sessions WHERE id=?")?;
        let mut rows = statement.query([id])?;
        rows.next()?
            .map(|row| -> Result<Session> { Ok(serde_json::from_str(&row.get::<_, String>(0)?)?) })
            .transpose()
    }
    fn body_path(&self, id: &str, phase: &str) -> PathBuf {
        self.directory
            .join("bodies")
            .join(format!("{id}-{phase}.bin"))
    }
    pub fn body(&self, id: &str, phase: &str) -> Result<(Vec<u8>, Headers)> {
        if !["request", "response"].contains(&phase) {
            return Err(crate::wire::Error::bad("phase must be request or response"));
        }
        let session = self
            .get(id)?
            .ok_or_else(|| crate::wire::Error::new(404, "History record not found"))?;
        let complete = if phase == "request" {
            session.request_body_complete
        } else {
            session.response_body_complete
        };
        if complete != Some(true) {
            return Err(crate::wire::Error::new(
                409,
                "A complete body is unavailable for this record",
            ));
        }
        let data = if session.incognito {
            self.volatile_bodies
                .get(&(id.into(), phase.into()))
                .cloned()
                .ok_or_else(|| crate::wire::Error::new(404, "Incognito body has expired"))?
        } else {
            fs::read(self.body_path(id, phase))?
        };
        let headers = crate::wire::parse_headers(if phase == "request" {
            &session.request_headers
        } else {
            &session.response_headers
        })?;
        Ok((data, headers))
    }
    fn archive(&mut self, session: &Session, phase: &str, capture: &Capture) -> Result<bool> {
        // A deleted in-flight record must not resurrect a file when it completes.
        if self.get(&session.id)?.is_none() {
            return Ok(false);
        }
        let complete = capture.length == capture.archive.len();
        if session.incognito {
            self.volatile_bodies
                .insert((session.id.clone(), phase.into()), capture.archive.clone());
            self.volatile_bodies
                .retain(|(id, _), _| self.sessions.iter().any(|s| &s.id == id));
        } else {
            atomic_write(&self.body_path(&session.id, phase), &capture.archive)?;
        }
        Ok(complete)
    }
    pub fn checkpoint(&self) {
        let _ = self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

pub struct Recording {
    pub session: Session,
    history: Arc<Mutex<History>>,
    began: Instant,
    pub request: Capture,
    pub response: Capture,
    pub request_headers: Headers,
    pub response_headers: Headers,
    finished: bool,
    suppressed: bool,
    capture_list_filters: Vec<crate::capture_filters::Filter>,
    filter_body_limit: usize,
}
impl Recording {
    pub fn rule_logs(&mut self, shared: &mut serde_json::Value) {
        for context in shared
            .as_object_mut()
            .into_iter()
            .flat_map(|v| v.values_mut())
        {
            if let Some(serde_json::Value::String(log)) =
                context.as_object_mut().and_then(|v| v.remove("_console"))
            {
                if !log.is_empty() {
                    self.session.script_logs.push(log);
                }
            }
        }
    }
    pub fn intercepted_by(&mut self, kind: &str) {
        if !self.session.interceptors.iter().any(|v| v == kind) {
            self.session.interceptors.push(kind.into());
        }
    }

    pub fn new_hidden(
        history: Arc<Mutex<History>>,
        source: &str,
        method: &str,
        url: &str,
        headers: Headers,
        preview: usize,
        suppressed: bool,
    ) -> Self {
        Self::new_hidden_for_application(
            history, source, method, url, headers, preview, suppressed, None,
        )
    }
    pub fn new_hidden_for_application(
        history: Arc<Mutex<History>>,
        source: &str,
        method: &str,
        url: &str,
        headers: Headers,
        preview: usize,
        suppressed: bool,
        application: Option<crate::application::Application>,
    ) -> Self {
        let (incognito, capture_group, full_limit, capture_list_filters) = {
            let history = history.lock().unwrap();
            (
                history.incognito,
                history.capture_group.clone(),
                history.full_limit,
                history
                    .capture_list_filters
                    .items
                    .iter()
                    .filter(|f| f.enabled)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let session = Session {
            id: Uuid::new_v4().to_string(),
            source: source.into(),
            method: method.into(),
            url: url.into(),
            status: 0,
            request_headers: crate::wire::header_text(&headers),
            response_headers: String::new(),
            request_body: String::new(),
            response_body: String::new(),
            started_at: now(),
            duration: 0,
            request_bytes: 0,
            response_bytes: 0,
            error: None,
            tls: url.starts_with("https:"),
            application,
            server_ip: None,
            client_ip: None,
            interceptors: vec![],
            grpc_messages: vec![],
            request_trailers: String::new(),
            response_trailers: String::new(),
            raw_response_base64: None,
            websocket_frames: vec![],
            script_logs: vec![],
            tests: vec![],
            variables: None,
            protocol: String::new(),
            incognito,
            capture_group,
            request_body_complete: None,
            response_body_complete: None,
            capture_list_matches: capture_list_filters
                .iter()
                .map(|f| (f.id.clone(), false))
                .collect(),
            capture_list_filter_error: None,
        };
        if !suppressed {
            history.lock().unwrap().insert(&session);
        }
        Self {
            session,
            history,
            began: Instant::now(),
            request: Capture::with_archive(preview, full_limit),
            response: Capture::with_archive(preview, full_limit),
            request_headers: headers,
            response_headers: Headers::new(),
            finished: false,
            suppressed,
            capture_list_filters,
            filter_body_limit: full_limit,
        }
    }

    pub fn match_list_filters(&mut self) -> bool {
        let filters = std::mem::take(&mut self.capture_list_filters);
        if filters.is_empty() {
            return false;
        }
        // Match complete archived bytes, never a possibly valid but truncated preview.
        let body = if filters.iter().any(|f| f.url == self.session.url) {
            let decoded = if self.request.length == self.request.archive.len() {
                crate::wire::decoded(
                    &self.request.archive,
                    &self.request_headers,
                    self.filter_body_limit,
                )
            } else {
                Err(crate::wire::Error::bad(
                    "Capture list filter requires the complete request body; archive limit exceeded",
                ))
            };
            match decoded {
                Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).ok(),
                Err(error) => {
                    self.session.capture_list_filter_error = Some(error.message);
                    None
                }
            }
        } else {
            None
        };
        self.session.capture_list_matches = filters
            .iter()
            .map(|f| (f.id.clone(), f.matches(&self.session.url, body.as_ref())))
            .collect();
        true
    }
    pub fn replace_response(&mut self, mut response: crate::wire::ResponseData) -> Result<()> {
        let bytes = response
            .binary_body
            .take()
            .unwrap_or_else(|| response.body.take().unwrap_or_default().into_bytes());
        response.headers.remove("content-encoding");
        response.headers.insert(
            "content-length".into(),
            serde_json::json!(bytes.len().to_string()),
        );
        self.response.reset(&bytes);
        self.response_headers = response.headers;
        self.session.status = response.status;
        self.session.response_headers = crate::wire::header_text(&self.response_headers);
        self.session.response_body = self.response.preview(&self.response_headers);
        self.session.response_bytes = bytes.len();
        if !self.suppressed {
            let mut history = self.history.lock().unwrap();
            self.session.response_body_complete =
                Some(history.archive(&self.session, "response", &self.response)?);
            history.save(&self.session);
        }
        Ok(())
    }
    pub fn publish(&mut self) {
        if !self.suppressed {
            self.session.duration = self.began.elapsed().as_millis() as u64;
            self.session.request_bytes = self.request.length;
            self.session.response_bytes = self.response.length;
            self.history.lock().unwrap().save(&self.session);
        }
    }
    pub fn finish(&mut self, error: Option<String>) -> Session {
        if !self.finished {
            if error.is_none() {
                self.match_list_filters();
            }
            self.finished = true;
            self.session.duration = self.began.elapsed().as_millis() as u64;
            self.session.request_bytes = self.request.length;
            self.session.response_bytes = self.response.length;
            if self.session.method != "CONNECT" {
                self.session.request_body = self.request.preview(&self.request_headers);
                self.session.response_body = self.response.preview(&self.response_headers);
            }
            self.session.grpc_messages = crate::grpc::messages(
                &self.request.archive,
                &self.request_headers,
                "request",
                self.request.archive.len().max(1024),
            );
            self.session.grpc_messages.extend(crate::grpc::messages(
                &self.response.archive,
                &self.response_headers,
                "response",
                self.response.archive.len().max(1024),
            ));
            self.session.error = error;
            let mut history = self.history.lock().unwrap();
            if self.suppressed {
                return self.session.clone();
            }
            if self.session.method != "CONNECT" {
                match history.archive(&self.session, "request", &self.request) {
                    Ok(value) => self.session.request_body_complete = Some(value),
                    Err(error) => {
                        history.error = Some(format!("Cannot archive request body: {error}"))
                    }
                }
                match history.archive(&self.session, "response", &self.response) {
                    Ok(value) => self.session.response_body_complete = Some(value),
                    Err(error) => {
                        history.error = Some(format!("Cannot archive response body: {error}"))
                    }
                }
            }
            history.save(&self.session);
        }
        self.session.clone()
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.finish(Some(
            "Client disconnected before the request completed".into(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capture_list_filters_snapshot_activation_and_match_complete_json_only() {
        const URL: &str = "http://test.api-shoulei-ssl.xunlei.com/flowhub/v1/slots:batchGet?client_id=Xp6vsxz_7IYVw2BB";
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(Mutex::new(History::open(dir.path(), 200).unwrap()));
        let record = |url: &str| {
            Recording::new_hidden(
                history.clone(),
                "proxy",
                "POST",
                url,
                Headers::new(),
                8,
                false,
            )
        };
        let mut before = record(URL);
        let filter = history
            .lock()
            .unwrap()
            .capture_list_filters
            .update(
                "POST",
                None,
                json!({"url":URL,"body":{"member":"id","equals":"Activity-Center-anzhuo"}}),
            )
            .unwrap();
        let id = filter["id"].as_str().unwrap();
        before.request.add(br#"{"id":"Activity-Center-anzhuo"}"#);
        assert!(before.finish(None).capture_list_matches.is_empty());
        let mut saved_id = String::new();
        for (url, body, expected) in [
            (URL, r#"{"id":"Activity-Center-anzhuo"}"#, true),
            (
                URL,
                " { \"other\": 1, \"id\": \"Activity-Center-anzhuo\" } ",
                true,
            ),
            (URL, r#"{"id":"activity-center-anzhuo"}"#, false),
            (URL, r#"{"id":1}"#, false),
            (URL, r#"{"id":null}"#, false),
            (URL, r#"{"other":"Activity-Center-anzhuo"}"#, false),
            (URL, r#"[{"id":"Activity-Center-anzhuo"}]"#, false),
            (URL, r#"{"id":"Activity-Center-anzhuo"} garbage"#, false),
            (URL, "not json", false),
            (URL, "", false),
            (
                "https://test.api-shoulei-ssl.xunlei.com/flowhub/v1/slots:batchGet?client_id=Xp6vsxz_7IYVw2BB",
                r#"{"id":"Activity-Center-anzhuo"}"#,
                false,
            ),
            (
                "http://test.api-shoulei-ssl.xunlei.com/flowhub/v1/other?client_id=Xp6vsxz_7IYVw2BB",
                r#"{"id":"Activity-Center-anzhuo"}"#,
                false,
            ),
            (
                "http://test.api-shoulei-ssl.xunlei.com/flowhub/v1/slots:batchGet?client_id=other",
                r#"{"id":"Activity-Center-anzhuo"}"#,
                false,
            ),
        ] {
            let mut current = record(url);
            current.request.add(body.as_bytes());
            let saved = current.finish(None);
            assert_eq!(saved.capture_list_matches[id], expected, "{url} {body}");
            if expected {
                saved_id = saved.id;
            }
        }
        let mut truncated = record(URL);
        truncated.request = Capture::with_archive(8, 31);
        truncated
            .request
            .add(br#"{"id":"Activity-Center-anzhuo"}   invalid"#);
        assert!(!truncated.finish(None).capture_list_matches[id]);
        assert!(truncated.session.capture_list_filter_error.is_some());
        history
            .lock()
            .unwrap()
            .capture_list_filters
            .update("PATCH", Some(id), json!({"enabled":false}))
            .unwrap();
        let mut disabled = record(URL);
        assert!(disabled.finish(None).capture_list_matches.is_empty());
        let reopened = History::open(dir.path(), 200).unwrap();
        assert!(!reopened.capture_list_filters.items[0].enabled);
        assert!(
            reopened
                .get(&saved_id)
                .unwrap()
                .unwrap()
                .capture_list_matches[id]
        );
    }

    #[test]
    fn node_history_schema_remains_readable_and_private_without_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, started_at TEXT NOT NULL, document TEXT NOT NULL)").unwrap();
        let document = json!({"id":"legacy", "source":"composer", "method":"POST", "url":"https://example.test/", "status":201, "requestHeaders":"X-Old: yes", "responseHeaders":"", "requestBody":"旧请求", "responseBody":"旧响应", "startedAt":"2026-11-12T00:00:00.000Z", "duration":10, "requestBytes":9, "responseBytes":9, "error":null, "tls":true});
        old.execute(
            "INSERT INTO sessions VALUES (?, ?, ?)",
            params!["legacy", "2026-11-12T00:00:00.000Z", document.to_string()],
        )
        .unwrap();
        drop(old);
        let mut history = History::open(dir.path(), 200).unwrap();
        assert_eq!(
            serde_json::to_value(&history.get("legacy").unwrap().unwrap()).unwrap(),
            document
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(history.sessions.is_empty());
        let mut changed = history.get("legacy").unwrap().unwrap();
        changed.response_body = "updated".into();
        history.save(&changed);
        assert_eq!(history.page(0, 200).unwrap()[0].response_body, "updated");
        history.clear().unwrap();
        assert!(history.page(0, 200).unwrap().is_empty());
        assert_eq!(
            fs::metadata(dir.path().join("history.sqlite-wal"))
                .unwrap()
                .len(),
            0
        );
    }
}
