use super::*;
use crate::storage::Session;
use base64::engine::general_purpose::STANDARD;
use std::{io::Read, path::Path};

const MAX_FILE: usize = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 200;

impl DesktopEngine {
    pub fn import_har(&self, path: &Path) -> std::result::Result<(String, usize), String> {
        let import = || -> Result<_> {
            let mut encoded = Vec::new();
            std::fs::File::open(path)?
                .take(MAX_FILE as u64 + 1)
                .read_to_end(&mut encoded)?;
            if encoded.len() > MAX_FILE {
                return Err(Error::bad("HAR exceeds 32 MiB"));
            }
            let document: Value = serde_json::from_slice(&encoded)?;
            drop(encoded);
            let entries = document["log"]["entries"]
                .as_array()
                .ok_or_else(|| Error::bad("HAR log.entries is required"))?;
            if entries.is_empty() || entries.len() > MAX_ENTRIES {
                return Err(Error::bad("HAR must contain 1 to 200 entries"));
            }
            let group = uuid::Uuid::new_v4().to_string();
            let limit = self.running.engine.config.request;
            let mut rows = Vec::with_capacity(entries.len());
            for (i, entry) in entries.iter().enumerate() {
                rows.push(
                    parse_entry(entry, &group, limit)
                        .map_err(|error| Error::bad(format!("HAR entry {}: {error}", i + 1)))?,
                );
            }
            let count = rows.len();
            self.running
                .engine
                .history
                .lock()
                .unwrap()
                .import_records(rows)?;
            Ok((group, count))
        };
        import().map_err(|error| error.to_string())
    }

    pub fn export_har(&self, path: &Path, ids: &[String]) -> std::result::Result<usize, String> {
        let export = || -> Result<_> {
            if ids.is_empty() || ids.len() > MAX_ENTRIES {
                return Err(Error::bad("Select 1 to 200 records for HAR"));
            }
            let mut entries = Vec::with_capacity(ids.len());
            let mut size = 0;
            let history = self.running.engine.history.lock().unwrap();
            for id in ids {
                let session = history
                    .get(id)?
                    .ok_or_else(|| Error::bad("Capture was removed"))?;
                if session.incognito {
                    return Err(Error::bad("Incognito records cannot be exported"));
                }
                // An explicit marker distinguishes unavailable bytes from an empty body.
                let mut content = |complete, phase| -> Result<Value> {
                    if complete != Some(true) {
                        return Ok(json!({"_rechoIncomplete":true}));
                    }
                    let (raw, headers) = history.body(id, phase)?;
                    let bytes = wire::decoded(&raw, &headers, self.running.engine.config.request)?;
                    size += bytes.len();
                    if size > MAX_FILE {
                        return Err(Error::bad("HAR export exceeds 32 MiB"));
                    }
                    Ok(match std::str::from_utf8(&bytes) {
                        Ok(text) if !text.contains('\0') => json!({"text":text,"size":bytes.len()}),
                        _ => {
                            json!({"text":STANDARD.encode(&bytes),"encoding":"base64","size":bytes.len()})
                        }
                    })
                };
                let req = content(session.request_body_complete, "request")?;
                let response = content(session.response_body_complete, "response")?;
                let headers = |text: &str| {
                    text.lines()
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| json!({"name":name,"value":value.trim()}))
                        .collect::<Vec<_>>()
                };
                let mut entry = json!({
                    "startedDateTime":session.started_at, "time":session.duration,
                    "request":{"method":session.method,"url":session.url,"httpVersion":session.protocol,
                        "headers":headers(&session.request_headers),"queryString":[],"cookies":[],
                        "headersSize":-1,"bodySize":session.request_bytes,"postData":req},
                    "response":{"status":session.status,"statusText":"","httpVersion":session.protocol,
                        "headers":headers(&session.response_headers),"cookies":[],
                        "content":response,"redirectURL":"","headersSize":-1,"bodySize":session.response_bytes},
                    "cache":{},"timings":{"send":0,"wait":session.duration,"receive":0},
                    "serverIPAddress":session.server_ip,"_rechoError":session.error,
                });
                if !session.notes.is_empty() {
                    entry["comment"] = json!(session.notes);
                }
                entries.push(entry);
            }
            drop(history);
            let bytes = serde_json::to_vec(
                &json!({"log":{"version":"1.2","creator":{"name":"recho","version":"0.1.0"},"entries":entries}}),
            )?;
            if bytes.len() > MAX_FILE {
                return Err(Error::bad("Encoded HAR exceeds 32 MiB"));
            }
            crate::storage::atomic_write(path, &bytes)?;
            Ok(ids.len())
        };
        export().map_err(|error| error.to_string())
    }
}

fn parse_entry(
    entry: &Value,
    group: &str,
    limit: usize,
) -> Result<(Session, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let request = &entry["request"];
    let response = &entry["response"];
    let url = request["url"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing request URL"))?;
    parse_url(url)?;
    let method = request["method"]
        .as_str()
        .ok_or_else(|| Error::bad("Missing request method"))?;
    hyper::Method::from_bytes(method.as_bytes()).map_err(Error::bad)?;
    let protocol = request["httpVersion"].as_str().unwrap_or_default();
    let started_at = entry["startedDateTime"]
        .as_str()
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(crate::storage::now);
    let status = response["status"]
        .as_u64()
        .filter(|value| *value <= 999)
        .ok_or_else(|| Error::bad("Invalid status"))? as u16;
    let headers = |message: &Value| -> Result<String> {
        let mut lines = String::new();
        for header in message["headers"].as_array().into_iter().flatten() {
            let name = header["name"]
                .as_str()
                .ok_or_else(|| Error::bad("Invalid header name"))?;
            let value = header["value"]
                .as_str()
                .ok_or_else(|| Error::bad("Invalid header value"))?;
            if name.starts_with(':')
                || ["content-encoding", "content-length", "transfer-encoding"]
                    .iter()
                    .any(|excluded| name.eq_ignore_ascii_case(excluded))
            {
                continue;
            }
            hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(Error::bad)?;
            hyper::header::HeaderValue::from_str(value).map_err(Error::bad)?;
            lines.push_str(&format!("{name}: {value}\r\n"));
        }
        Ok(lines)
    };
    let body = |content: &Value, size: &Value| -> Result<Option<Vec<u8>>> {
        if content["_rechoIncomplete"] == true {
            return Ok(None);
        }
        let bytes = if let Some(text) = content["text"].as_str() {
            match content["encoding"].as_str() {
                None | Some("") => text.as_bytes().to_vec(),
                Some("base64") => STANDARD.decode(text).map_err(Error::bad)?,
                Some(encoding) => {
                    return Err(Error::bad(format!(
                        "Unsupported HAR body encoding {encoding}"
                    )));
                }
            }
        } else if size.as_u64() == Some(0) {
            Vec::new()
        } else {
            return Ok(None);
        };
        if bytes.len() > limit {
            return Err(Error::bad("HAR body exceeds the capture limit"));
        }
        Ok(Some(bytes))
    };
    let request_body = body(&request["postData"], &request["bodySize"])?;
    let response_body = body(&response["content"], &response["bodySize"])?;
    let preview = |body: &Option<Vec<u8>>| {
        body.as_deref()
            .map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).into_owned())
            .unwrap_or_default()
    };
    let duration = entry["time"].as_f64().unwrap_or_default().max(0.0) as u64;
    let session = Session {
        id: uuid::Uuid::new_v4().to_string(),
        source: "import".into(),
        method: method.into(),
        url: url.into(),
        status,
        request_headers: headers(request)?,
        response_headers: headers(response)?,
        request_body: preview(&request_body),
        response_body: preview(&response_body),
        started_at,
        duration,
        request_bytes: request_body.as_ref().map_or(0, Vec::len),
        response_bytes: response_body.as_ref().map_or(0, Vec::len),
        error: entry["_rechoError"].as_str().map(str::to_owned),
        notes: entry["comment"].as_str().unwrap_or_default().into(),
        tls: url.starts_with("https:"),
        server_ip: entry["serverIPAddress"].as_str().map(str::to_owned),
        protocol: protocol.into(),
        capture_group: group.into(),
        request_body_complete: Some(request_body.is_some()),
        response_body_complete: Some(response_body.is_some()),
        ..Session::default()
    };
    Ok((session, request_body, response_body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_har_keeps_binary_and_rejects_partial_import() {
        let directory = tempfile::tempdir().unwrap();
        let engine = DesktopEngine::start(directory.path().into(), 0)
            .await
            .unwrap();
        let input_path = directory.path().join("fixture.har");
        let exported_path = directory.path().join("exported.har");
        let entry = json!({
            "startedDateTime": "2026-11-20T10:00:00.123+00:00", "time": 7, "comment": "inspect this",
            "request": {"method":"POST","url":"http://127.0.0.1/test","httpVersion":"HTTP/1.1","bodySize":4,
                "headers":[{"name":"content-type","value":"application/octet-stream"}],
                "postData":{"text":"AAH/wQ==","encoding":"base64"}},
            "response":{"status":200,"bodySize":3,"headers":[],"content":{"text":"abc"}},
        });
        let save = |entries| {
            std::fs::write(
                &input_path,
                serde_json::to_vec(&json!({"log":{"entries":entries}})).unwrap(),
            )
            .unwrap()
        };
        save(vec![
            entry.clone(),
            json!({"request":{"method":"GET","url":"bad-url"}}),
        ]);
        assert!(engine.import_har(&input_path).is_err());
        assert!(engine.history_rows(0, 20, "", None).unwrap().is_empty());
        save(vec![entry]);
        let (group, count) = engine.import_har(&input_path).unwrap();
        assert_eq!(count, 1);
        let rows = engine.history_rows(0, 20, "", Some(&group)).unwrap();
        assert_eq!(rows[0].status, 200);
        assert_eq!(rows[0].started_at, "2026-11-20T10:00:00.123Z");
        assert_eq!(
            engine.detail(&rows[0].id, Phase::Request).unwrap().bytes,
            [0, 1, 255, 193]
        );
        assert_eq!(
            engine
                .export_har(&exported_path, &[rows[0].id.clone()])
                .unwrap(),
            1
        );
        let exported: Value =
            serde_json::from_slice(&std::fs::read(&exported_path).unwrap()).unwrap();
        assert_eq!(exported["log"]["entries"][0]["comment"], "inspect this");
        let (group, _) = engine.import_har(&exported_path).unwrap();
        let rows = engine.history_rows(0, 20, "", Some(&group)).unwrap();
        assert_eq!(
            engine.detail(&rows[0].id, Phase::Request).unwrap().bytes,
            [0, 1, 255, 193]
        );
        assert_eq!(
            engine.detail(&rows[0].id, Phase::Response).unwrap().bytes,
            b"abc"
        );
        engine.stop().await;
    }
}
