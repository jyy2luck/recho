use crate::{
    body::{Content, hex_row, show_structured},
    worker::{DetailHandle, RequestHandle, Worker},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use eframe::egui;
use recho_core::native::{Detail, Phase, Row};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::oneshot::{Receiver, error::TryRecvError};
use uuid::Uuid;

mod operations;
mod payload;
mod protocol;
use payload::{curl, payload, query_rows, query_url, request_from_curl};
use protocol::{Protocol, Update};

const ARCHIVE: &str = "/workspace/archive";
const DEFAULT_FOLDER: &str = "我的集合";
const EDITOR_TABS: [&str; 6] = ["参数", "请求体", "请求头", "授权", "脚本", "设置"];
const RESPONSE_TABS: [&str; 4] = ["响应体", "响应头", "测试", "控制台"];
const METHODS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT", "TRACE",
];
const VERSIONS: [&str; 3] = ["HTTP/1.1", "HTTP/2", "HTTP/3"];
const RAW_BODY: &str = "GET / HTTP/1.1\r\nUser-Agent: recho/0.1.0\r\n\r\n";
const PYTHON_SCRIPT: &str = "def onRequest(context, request):\n    return request\n\ndef onResponse(context, response):\n    return response\n";

fn id() -> String {
    Uuid::new_v4().to_string()
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
fn object_text<'a>(value: &'a mut Value, key: &str) -> &'a mut String {
    let target = &mut value[key];
    if !target.is_string() {
        *target = json!("");
    }
    let Value::String(string) = target else {
        unreachable!()
    };
    string
}
fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}
fn rows_mut<'a>(value: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    let target = &mut value[key];
    if !target.is_array() {
        *target = json!([]);
    }
    target.as_array_mut().unwrap()
}
fn take<T>(handle: &mut Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match handle.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Closed) => Some(Err("引擎连接已关闭".into())),
    }
}

fn initial_fields(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "rawHTTP" => ("未命名", RAW_BODY),
        "grpc" => ("未命名", "{}"),
        "protobuf" => ("service.proto", "syntax = \"proto3\";\n"),
        _ => ("未命名", ""),
    }
}
fn draft(kind: &str) -> Value {
    let (name, body) = initial_fields(kind);
    json!({
        "id": id(), "kind": kind, "name": name, "method": "GET", "url": "",
        "protocolVersion": "HTTP/1.1", "headers":"", "editorTab":"参数", "responseTab":"响应体",
        "authorizationType": "无", "authorizationUser": "", "authorizationSecret":"",
        "bodyMode": "文本", "bodyFile": "", "bodyMIME": "", "bodyFields": [],
        "body": body,
        "scriptEnabled": false, "scriptLanguage": "python",
        "pythonScript":PYTHON_SCRIPT,
        "beforeScript":"", "afterScript":"", "timeout":30.0, "verifyTLS":true, "autoSaveHistory":true
    })
}
fn name(request: &Value) -> &str {
    let value = text(request, "name").trim();
    if value.is_empty() { "未命名" } else { value }
}
fn content_fields(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        for key in [
            "id",
            "collectionEntryID",
            "editorTab",
            "responseTab",
            "response",
            "lastResponseID",
            "rawHeaders",
        ] {
            object.remove(key);
        }
    }
    value
}
fn normalized(source: &Value) -> Result<Value, String> {
    let kind = source.get("kind").and_then(Value::as_str).unwrap_or("http");
    if !["http", "rawHTTP", "grpc", "protobuf", "websocket"].contains(&kind) {
        return Err(format!("当前编辑器不支持请求类型 {kind}"));
    }
    let object = source.as_object().ok_or("请求文档必须为 JSON 对象")?;
    let mut result = draft(kind);
    for (key, value) in object {
        if key != "id" && key != "response" && key != "collectionEntryID" {
            if result[key].is_string() && !value.is_string() {
                return Err(format!("请求字段 {key} 必须为文本"));
            }
            result[key] = value.clone();
        }
    }
    Ok(result)
}

struct Sending {
    environment: String,
    variables: Map<String, Value>,
    remember_response: bool,
    handle: RequestHandle,
}
struct Reply {
    summary: Value,
    body: Content,
    notice: Option<String>,
    detail: Option<DetailHandle>,
    mode: u8,
    structured: Option<Value>,
}
impl Reply {
    fn new(mut summary: Value, worker: &Worker) -> Self {
        let encoded = summary
            .as_object_mut()
            .and_then(|record| record.remove("responseBodyBase64"))
            .and_then(|value| match value {
                Value::String(encoded) => STANDARD.decode(encoded).ok(),
                _ => None,
            });
        let detail = if encoded.is_none() && summary["responseBodyComplete"] == true {
            worker
                .detail(text(&summary, "id").into(), Phase::Response)
                .ok()
        } else {
            None
        };
        let mut preview = summary
            .as_object_mut()
            .and_then(|record| record.remove("responseBody"));
        let bytes = encoded.unwrap_or_else(|| match preview.take() {
            Some(Value::String(value)) => value.into_bytes(),
            _ => vec![],
        });
        let has_body = summary["responseBytes"].as_u64().unwrap_or(0) > 0;
        let notice = if detail.is_some() {
            Some("正在读取完整正文…".into())
        } else if has_body && summary["responseBodyLoaded"] != true {
            Some("显示响应预览，完整归档不可用。".into())
        } else {
            None
        };
        if let Some(record) = summary.as_object_mut() {
            for key in ["requestBody", "requestBodyBase64", "rawResponseBase64"] {
                record.remove(key);
            }
        }
        let body = Content::new(bytes);
        let mode = u8::from(matches!(body, Content::Binary(_)));
        Self {
            summary,
            body,
            notice,
            detail,
            mode,
            structured: None,
        }
    }
    fn poll(&mut self) {
        if let Some(result) = self.detail.as_mut().and_then(take) {
            self.detail = None;
            match result {
                Ok(detail) => {
                    self.notice = detail.notice;
                    self.body = Content::new(detail.bytes);
                    self.structured = None;
                    if matches!(self.body, Content::Binary(_)) {
                        self.mode = 1;
                    }
                }
                Err(error) => self.notice = Some(format!("完整正文读取失败，显示预览。{error}")),
            }
        }
    }
}

struct SaveTab {
    id: String,
    new_copy: bool,
    close: bool,
    name: String,
    folder: String,
}
struct Transfer {
    kind: &'static str,
    text: String,
    target: Option<String>,
    error: Option<String>,
}
struct EnvironmentEdit {
    value: Value,
    removed: bool,
}

#[derive(Default)]
pub(super) struct Composer {
    document: Option<Value>,
    saved: Option<Value>,
    load: Option<RequestHandle>,
    save: Option<RequestHandle>,
    save_error: Option<String>,
    dirty_at: Option<Instant>,
    active: String,
    sending: HashMap<String, Sending>,
    importing: HashMap<String, DetailHandle>,
    // Only the latest displayed response retains a full body between requests.
    reply: Option<(String, Reply)>,
    errors: HashMap<String, String>,
    save_tab: Option<SaveTab>,
    transfer: Option<Transfer>,
    environment_edit: Option<EnvironmentEdit>,
    new_folder: Option<String>,
    delete_collection_entry: Option<String>,
    protocols: HashMap<String, Protocol>,
    protocol_cleanup: Vec<(bool, RequestHandle)>,
    close_after_archive: Option<String>,
    volatile_tabs: std::collections::HashSet<String>,
}

impl Composer {
    pub(super) fn poll(&mut self, worker: &Worker, ready: bool, ctx: &egui::Context) {
        if ready && self.document.is_none() && self.load.is_none() && self.save_error.is_none() {
            match worker.request("GET", ARCHIVE, Value::Null) {
                Ok(handle) => self.load = Some(handle),
                Err(error) => self.save_error = Some(error),
            }
        }
        if let Some(result) = self.load.as_mut().and_then(take) {
            self.load = None;
            match result {
                Ok(mut reply) if reply["document"].is_object() => {
                    let value = reply["document"].take();
                    self.active = text(&value, "activeRequestID").into();
                    self.saved = Some(value.clone());
                    self.document = Some(value);
                }
                Ok(_) => self.save_error = Some("工作区归档格式无效".into()),
                Err(error) => self.save_error = Some(error),
            }
        }
        if let Some(result) = self.save.as_mut().and_then(take) {
            self.save = None;
            match result {
                Ok(mut reply) if reply["document"].is_object() => {
                    self.saved = Some(reply["document"].take());
                }
                Ok(_) => self.save_error = Some("保存回执格式无效".into()),
                Err(error) => self.save_error = Some(error),
            }
        }
        let mut changed = vec![];
        self.importing.retain(|id, handle| {
            if let Some(result) = take(handle) {
                changed.push((id.clone(), result));
                false
            } else {
                true
            }
        });
        for (id, result) in changed {
            match result {
                Ok(detail) => self.apply_detail(&id, &detail, &detail.bytes),
                Err(error) => {
                    self.errors.insert(id, format!("读取捕获正文失败。{error}"));
                }
            }
        }
        let mut finished = Vec::new();
        self.sending.retain(|id, pending| {
            if let Some(result) = take(&mut pending.handle) {
                finished.push((
                    id.clone(),
                    result,
                    pending.environment.clone(),
                    std::mem::take(&mut pending.variables),
                    pending.remember_response,
                ));
                false
            } else {
                true
            }
        });
        for (id, result, environment, variables, remember_response) in finished {
            match result {
                Ok(response) => {
                    if response["variables"].is_object() {
                        self.apply_script_variables(
                            &environment,
                            &variables,
                            &response["variables"],
                        );
                    }
                    if let Some(error) = response["error"].as_str() {
                        self.errors.insert(id.clone(), error.into());
                    }
                    // Retain each tab's last response ID. Reload archived bodies on demand.
                    if let Some(entry) = self.request_mut(&id) {
                        entry["lastResponseID"] =
                            if !remember_response || response["incognito"] == true {
                                Value::Null
                            } else {
                                response["id"].clone()
                            };
                        self.mark_dirty();
                    }
                    if id == self.active {
                        self.reply = Some((id, Reply::new(response, worker)));
                    }
                }
                Err(error) => {
                    self.errors.insert(id, error);
                }
            }
        }
        if let Some((id, response)) = &mut self.reply {
            if *id == self.active {
                response.poll();
            }
        }
        let mut cleanup_ids = Vec::new();
        self.protocol_cleanup
            .retain_mut(|(starting, handle)| match take(handle) {
                Some(Ok(value)) if *starting => {
                    cleanup_ids.push(text(&value, "id").to_owned());
                    false
                }
                Some(_) => false,
                None => true,
            });
        for id in cleanup_ids {
            if let Ok(handle) =
                worker.request("DELETE", format!("/grpc-client/{id}/cancel"), json!({}))
            {
                self.protocol_cleanup.push((false, handle));
            }
        }
        let mut updates = Vec::new();
        for (id, protocol) in &mut self.protocols {
            if let Some(update) = protocol.poll(worker, ctx) {
                updates.push((id.clone(), update));
            }
        }
        for (id, Update::Specification { name, source }) in updates {
            self.save_specification(&id, name, source);
        }
        if self.save.is_none()
            && self.close_after_archive.is_some()
            && self.saved.as_ref()
                == self
                    .document
                    .as_ref()
                    .map(|_| self.archive_document())
                    .as_ref()
            && let Some(id) = self.close_after_archive.take()
            && let Some(index) = self.request_index(&id)
        {
            self.close_tab(index, worker);
        }
        if let Some(since) = self.dirty_at {
            let delay = Duration::from_millis(350).saturating_sub(since.elapsed());
            if delay.is_zero() {
                self.save_archive(worker);
            } else {
                ctx.request_repaint_after(delay);
            }
        }
    }
    fn save_archive(&mut self, worker: &Worker) {
        if self.save.is_some() || self.save_error.is_some() {
            return;
        }
        let (Some(saved), Some(_)) = (&self.saved, &self.document) else {
            return;
        };
        let document = self.archive_document();
        if *saved == document {
            self.dirty_at = None;
            return;
        }
        match worker.request(
            "PUT",
            ARCHIVE,
            json!({"expected":saved,"document":document}),
        ) {
            Ok(handle) => {
                self.save = Some(handle);
                self.dirty_at = None;
            }
            Err(error) => self.save_error = Some(error),
        }
    }
    pub(super) fn flush_pending(&mut self, worker: &Worker) -> bool {
        self.save_archive(worker);
        self.save.is_none()
            && self.saved.as_ref()
                == self
                    .document
                    .as_ref()
                    .map(|_| self.archive_document())
                    .as_ref()
    }
    pub(super) fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }
    fn mark_dirty(&mut self) {
        self.dirty_at = Some(Instant::now());
    }
    fn archive_document(&self) -> Value {
        let Some(document) = &self.document else {
            return Value::Null;
        };
        let mut copy = document.clone();
        if self.volatile_tabs.contains(text(&copy, "activeRequestID")) {
            copy["activeRequestID"] = json!("");
        }
        rows_mut(&mut copy, "requests")
            .retain(|entry| !self.volatile_tabs.contains(text(entry, "id")));
        copy
    }
    pub(super) fn active(&self) -> bool {
        self.document.as_ref().is_some_and(|doc| {
            rows(doc, "requests")
                .iter()
                .any(|request| text(request, "id") == self.active)
        })
    }
    pub(super) fn show_capture(&mut self) {
        self.select(String::new());
    }
    fn select(&mut self, id: String) {
        if self.active == id {
            return;
        }
        self.active = id;
        self.reply = None;
        if let Some(document) = &mut self.document {
            document["activeRequestID"] = json!(self.active);
            self.mark_dirty();
        }
    }
    fn add(&mut self, value: Value) -> Option<String> {
        let document = self.document.as_mut()?;
        let id = text(&value, "id").to_string();
        rows_mut(document, "requests").push(value);
        self.select(id.clone());
        Some(id)
    }
    pub(super) fn open_new(&mut self) {
        self.add(draft("http"));
    }
    pub(super) fn open_raw(&mut self) {
        self.add(draft("rawHTTP"));
    }
    pub(super) fn open_websocket(&mut self) {
        self.add(draft("websocket"));
    }
    pub(super) fn open_grpc(&mut self) {
        self.add(draft("grpc"));
    }
    pub(super) fn open_protobuf(&mut self) {
        self.add(draft("protobuf"));
    }
    pub(super) fn workspace_operation_once(
        &mut self,
        id: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, String> {
        self.handle_operation(Some(id), name, arguments)
    }
    pub(super) fn open_library_capture(&mut self, session: Value) {
        let record = session.get("session").unwrap_or(&session);
        if !record["url"].is_string() || !record["method"].is_string() {
            return;
        }
        let mut request = draft("http");
        let request_body = text(record, "requestBody");
        let encoded = record["requestBodyBase64"]
            .as_str()
            .and_then(|encoded| STANDARD.decode(encoded).ok());
        let complete =
            record["requestBytes"].as_u64() == Some(0) || record["requestBodyLoaded"] == true;
        let detail = Detail {
            id: text(record, "id").into(),
            phase: Phase::Request,
            headers: text(record, "requestHeaders").into(),
            trailers: String::new(),
            bytes: vec![],
            notice: (!complete).then(|| "快照仅保留预览".into()),
        };
        request["url"] = record["url"].clone();
        request["method"] = record["method"].clone();
        if VERSIONS.contains(&text(record, "protocol")) {
            request["protocolVersion"] = record["protocol"].clone();
        }
        apply_capture_detail(
            &mut request,
            &detail,
            encoded.as_deref().unwrap_or(request_body.as_bytes()),
        );
        if let Some(id) = self.add(request)
            && record["incognito"] == true
        {
            self.volatile_tabs.insert(id);
        }
    }
    pub(super) fn open_capture(
        &mut self,
        worker: &Worker,
        row: &Row,
        detail: Option<&Detail>,
        bytes: Option<&[u8]>,
    ) {
        let mut value = draft("http");
        value["method"] = json!(row.method);
        value["url"] = json!(row.url);
        value["name"] = json!(row.host);
        value["bodyIncomplete"] = json!(true);
        if VERSIONS.contains(&row.protocol.as_str()) {
            value["protocolVersion"] = json!(row.protocol);
        }
        let Some(tab_id) = self.add(value) else {
            return;
        };
        if row.incognito {
            self.volatile_tabs.insert(tab_id.clone());
        }
        if let Some((detail, bytes)) = detail
            .zip(bytes)
            .filter(|(detail, _)| detail.notice.is_none())
        {
            self.apply_detail(&tab_id, detail, bytes);
        } else {
            match worker.detail(row.id.clone(), Phase::Request) {
                Ok(handle) => {
                    self.importing.insert(tab_id, handle);
                }
                Err(error) => {
                    self.errors.insert(tab_id, error);
                }
            }
        }
    }
    fn apply_detail(&mut self, id: &str, detail: &Detail, bytes: &[u8]) {
        if let Some(value) = self.request_mut(id) {
            apply_capture_detail(value, detail, bytes);
            self.mark_dirty();
        }
    }
    fn request_mut(&mut self, id: &str) -> Option<&mut Value> {
        rows_mut(self.document.as_mut()?, "requests")
            .iter_mut()
            .find(|entry| text(entry, "id") == id)
    }
    fn request_index(&self, id: &str) -> Option<usize> {
        rows(self.document.as_ref()?, "requests")
            .iter()
            .position(|entry| text(entry, "id") == id)
    }
    pub(super) fn save_active(&mut self, worker: &Worker) {
        let Some(index) = self.request_index(&self.active) else {
            return;
        };
        let Some(document) = &self.document else {
            return;
        };
        let request = &rows(document, "requests")[index];
        if text(request, "kind") == "protobuf" {
            self.protocols
                .entry(self.active.clone())
                .or_default()
                .save_proto(worker, request, rows(document, "specifications"));
        } else {
            self.save_dialog(index, false, false);
        }
    }
    pub(super) fn close_active(&mut self, worker: &Worker) {
        if let Some(index) = self.request_index(&self.active) {
            self.request_close(index, false, worker);
        }
    }
    fn apply_script_variables(
        &mut self,
        env_id: &str,
        before: &Map<String, Value>,
        response: &Value,
    ) {
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(environment) = rows_mut(document, "environments")
            .iter_mut()
            .find(|entry| text(entry, "id") == env_id)
        else {
            return;
        };
        let fields = rows_mut(environment, "variables");
        let Some(values) = response.as_object() else {
            return;
        };
        let mut changed = false;
        for (key, value) in values {
            if !value.is_string() || before.get(key) == Some(value) {
                continue;
            }
            if let Some(field) = fields
                .iter_mut()
                .rev()
                .find(|field| text(field, "name") == key && field["enabled"] != false)
            {
                if before.get(key) == Some(&field["value"]) {
                    field["value"] = value.clone();
                    changed = true;
                }
            } else if !before.contains_key(key) {
                fields.push(json!({"id":id(),"name":key,"value":value,"enabled":true}));
                changed = true;
            }
        }
        if changed {
            self.mark_dirty();
        }
    }
    fn archive_notice(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.save_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
            if ui
                .button(if self.document.is_some() {
                    "重试保存"
                } else {
                    "重试读取"
                })
                .clicked()
            {
                self.save_error = None;
                self.mark_dirty();
            }
        } else if self.document.is_none() {
            ui.weak("正在读取工作区…");
        }
    }
    pub(super) fn show_tabs(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        let Some(document) = &self.document else {
            return;
        };
        let requests = rows(document, "requests");
        let mut selection = None;
        let mut close = None;
        let mut duplicate = None;
        let mut save = None;
        for (index, request) in requests.iter().enumerate() {
            let tab_id = text(request, "id");
            ui.push_id(tab_id, |ui| {
                let selected = tab_id == self.active;
                let dirty = modified(document, request);
                let label = format!(
                    "{}  {}{}",
                    match text(request, "kind") {
                        "rawHTTP" => "HTTP 报文",
                        "websocket" => "WebSocket",
                        "grpc" => "gRPC",
                        "protobuf" => "Protobuf",
                        _ => text(request, "method"),
                    },
                    name(request),
                    if dirty { "  ●" } else { "" },
                );
                let response =
                    ui.add(egui::Button::selectable(selected, label).min_size([90.0, 30.0].into()));
                if response.clicked() {
                    selection = Some(tab_id.to_string());
                }
                if response.middle_clicked()
                    || ui.small_button("×").on_hover_text("关闭请求").clicked()
                {
                    close = Some((index, false));
                }
                response.context_menu(|ui| {
                    for (caption, copy) in [("保存 API", false), ("另存 API", true)] {
                        if ui.button(caption).clicked() {
                            save = Some((index, copy));
                            ui.close();
                        }
                    }
                    if ui.button("复制请求").clicked() {
                        duplicate = Some(index);
                        ui.close();
                    }
                    if ui.button("关闭").clicked() {
                        close = Some((index, false));
                        ui.close();
                    }
                    if ui.button("强制关闭").clicked() {
                        close = Some((index, true));
                        ui.close();
                    }
                });
            });
        }
        if let Some(tab) = selection {
            self.select(tab);
        }
        if let Some(index) = duplicate {
            let source = requests_at(self.document.as_ref(), index);
            let volatile = self.volatile_tabs.contains(text(&source, "id"));
            if let Ok(value) = normalized(&source)
                && let Some(id) = self.add(value)
                && volatile
            {
                self.volatile_tabs.insert(id);
            }
        }
        if let Some((index, copy)) = save {
            self.save_dialog(index, copy, false);
        }
        if let Some((index, force)) = close {
            self.request_close(index, force, worker);
        }
    }
    fn request_close(&mut self, index: usize, force: bool, worker: &Worker) {
        if force
            || !self
                .document
                .as_ref()
                .is_some_and(|doc| modified(doc, &rows(doc, "requests")[index]))
        {
            self.close_tab(index, worker);
        } else {
            self.save_dialog(index, false, true);
        }
    }
    fn close_tab(&mut self, index: usize, worker: &Worker) {
        let Some(document) = &mut self.document else {
            return;
        };
        let requests = rows_mut(document, "requests");
        if index >= requests.len() {
            return;
        }
        let value = requests.remove(index);
        let closed = text(&value, "id");
        self.volatile_tabs.remove(closed);
        if self.close_after_archive.as_deref() == Some(closed) {
            self.close_after_archive = None;
        }
        self.importing.remove(closed);
        if let Some(mut protocol) = self.protocols.remove(closed) {
            if let Some(handle) = protocol.close(worker) {
                self.protocol_cleanup.push(handle);
            }
        }
        self.sending.remove(closed);
        self.errors.remove(closed);
        if self.active == closed {
            let next = requests
                .get(index)
                .or_else(|| requests.last())
                .map_or(String::new(), |request| text(request, "id").into());
            self.select(next);
        }
        self.mark_dirty();
    }
    fn save_dialog(&mut self, index: usize, new_copy: bool, close: bool) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(request) = rows(document, "requests").get(index) else {
            return;
        };
        let folder = rows(document, "collections")
            .iter()
            .find(|entry| text(entry, "id") == text(request, "collectionEntryID"))
            .map(|entry| text(entry, "folder"))
            .unwrap_or(DEFAULT_FOLDER)
            .to_string();
        self.save_tab = Some(SaveTab {
            id: text(request, "id").into(),
            new_copy,
            close,
            name: name(request).into(),
            folder,
        });
    }
    fn store_tab(&mut self, save: SaveTab) {
        let Some(index) = self.request_index(&save.id) else {
            return;
        };
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(request) = rows_mut(document, "requests").get_mut(index) else {
            return;
        };
        let existing = text(request, "collectionEntryID");
        let entry_id = if !save.new_copy && !existing.is_empty() {
            existing.to_string()
        } else {
            id()
        };
        request["name"] = json!(save.name.trim());
        request["collectionEntryID"] = json!(entry_id);
        self.volatile_tabs.remove(&save.id);
        let mut body = request.clone();
        for key in ["collectionEntryID", "lastResponseID", "response"] {
            body.as_object_mut().unwrap().remove(key);
        }
        let entry = json!({"id": entry_id, "folder": save.folder, "request": body});
        let collections = rows_mut(document, "collections");
        if let Some(item) = collections
            .iter_mut()
            .find(|item| text(item, "id") == entry_id)
        {
            *item = entry;
        } else {
            collections.push(entry);
        }
        self.mark_dirty();
        if save.close {
            self.close_after_archive = Some(save.id);
        }
    }
    fn environments(doc: &Value) -> Map<String, Value> {
        let env_id = text(doc, "activeEnvironmentID");
        rows(doc, "environments")
            .iter()
            .find(|entry| text(entry, "id") == env_id)
            .into_iter()
            .flat_map(|entry| rows(entry, "variables"))
            .filter(|item| {
                item["enabled"] != false && item["name"].is_string() && item["value"].is_string()
            })
            .map(|item| (text(item, "name").to_owned(), item["value"].clone()))
            .collect()
    }
    fn send(&mut self, worker: &Worker) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(request) = rows(document, "requests")
            .iter()
            .find(|request| text(request, "id") == self.active)
        else {
            return;
        };
        let variables = Self::environments(document);
        let environment = text(document, "activeEnvironmentID").to_string();
        let raw = text(request, "kind") == "rawHTTP";
        if self.volatile_tabs.contains(&self.active) {
            self.errors.insert(
                self.active.clone(),
                "无痕捕获得到的草稿只在当前会话保留。请先另存 API 再发送。".into(),
            );
            return;
        }
        match payload(request, &variables).and_then(|body| {
            worker.request("POST", if raw { "/request/raw" } else { "/request" }, body)
        }) {
            Ok(handle) => {
                self.errors.remove(&self.active);
                self.sending.insert(
                    self.active.clone(),
                    Sending {
                        environment,
                        variables,
                        remember_response: !self.volatile_tabs.contains(&self.active)
                            && (!raw || request["autoSaveHistory"] != false),
                        handle,
                    },
                );
                self.reply = None;
            }
            Err(error) => {
                self.errors.insert(self.active.clone(), error);
            }
        }
    }
    fn cancel(&mut self) {
        self.sending.remove(&self.active);
        self.errors.insert(self.active.clone(), "已取消请求".into());
    }
    pub(super) fn show(&mut self, ui: &mut egui::Ui, worker: &Worker, proxy_port: u16) {
        self.archive_notice(ui);
        let Some(document) = &mut self.document else {
            return;
        };
        let Some(index) = rows(document, "requests")
            .iter()
            .position(|request| text(request, "id") == self.active)
        else {
            return;
        };
        let kind = text(&rows(document, "requests")[index], "kind");
        if matches!(kind, "websocket" | "grpc" | "protobuf") {
            let variables = Self::environments(document);
            let specs = rows(document, "specifications").to_vec();
            let request = &mut rows_mut(document, "requests")[index];
            let protocol = self.protocols.entry(self.active.clone()).or_default();
            if protocol.show(ui, worker, request, &specs, &variables, proxy_port) {
                self.mark_dirty();
            }
            return;
        }
        let mut changed = false;
        let mut saving = false;
        let mut setup_environment = false;
        let mut draft_import = None;
        let mut action = None;
        let busy = self.sending.contains_key(&self.active);
        let loading = self.importing.contains_key(&self.active);
        let environments = rows(document, "environments")
            .iter()
            .map(|entry| (text(entry, "id").to_owned(), text(entry, "name").to_owned()))
            .collect::<Vec<_>>();
        let mut active_env = text(document, "activeEnvironmentID").to_string();
        let variables = Self::environments(document);
        let request = &mut rows_mut(document, "requests")[index];
        let raw = text(request, "kind") == "rawHTTP";
        ui.push_id(&self.active, |ui| {
            ui.horizontal(|ui| {
                changed |= line(ui, request, "name", "请求名称", 156.0).changed();
                if !raw {
                    changed |= choice(ui, request, "protocolVersion", "HTTP 协议版本", &VERSIONS);
                }
                egui::ComboBox::from_id_salt("request_environment")
                    .selected_text(
                        environments
                            .iter()
                            .find(|(id, _)| id == &active_env)
                            .map_or("无环境", |(_, name)| name.as_str()),
                    )
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut active_env, String::new(), "无环境")
                            .changed();
                        for (id, name) in &environments {
                            changed |= ui
                                .selectable_value(&mut active_env, id.clone(), name)
                                .changed();
                        }
                    });
                setup_environment = ui.small_button("管理环境").clicked();
                saving = ui.button("保存 API").clicked()
                    || ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)
                    });
                ui.menu_button("请求操作", |ui| {
                    if !raw && ui.button("导入 cURL").clicked() {
                        draft_import = Some("导入 cURL");
                        ui.close();
                    }
                    if !raw && ui.button("复制为 cURL").clicked() {
                        match curl(request, &variables) {
                            Ok(command) => ui.ctx().copy_text(command),
                            Err(error) => {
                                self.errors.insert(self.active.clone(), error);
                            }
                        }
                        ui.close();
                    }
                    if ui.button("复制请求 JSON").clicked() {
                        ui.ctx().copy_text(
                            serde_json::to_string_pretty(&content_fields(request.clone()))
                                .unwrap_or_default(),
                        );
                        ui.close();
                    }
                    if ui.button("导入请求 JSON").clicked() {
                        draft_import = Some("导入请求 JSON");
                        ui.close();
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                if !raw {
                    changed |= choice(ui, request, "method", "请求方法", &METHODS);
                }
                let url = line(
                    ui,
                    request,
                    "url",
                    if raw {
                        "连接目标 URL"
                    } else {
                        "输入请求 URL"
                    },
                    (ui.available_width() - 92.0).max(140.0),
                );
                if url.changed() {
                    changed = true;
                    if text(request, "url").trim_start().starts_with("curl ") && !raw {
                        draft_import = Some("导入 cURL");
                    }
                }
                let send = ui.add_enabled(
                    !loading,
                    egui::Button::new(if busy { "取消" } else { "发送" })
                        .fill(ui.visuals().selection.bg_fill)
                        .min_size([78.0, 28.0].into()),
                );
                if send.clicked()
                    || (!busy
                        && !loading
                        && url.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                {
                    action = Some(busy);
                }
            });
            if loading {
                ui.weak("正在读取捕获正文…");
            }
            if let Some(error) = self.errors.get(&self.active) {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            if request["bodyIncomplete"] == true {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "完整请求正文不可用。补全正文后才能发送。",
                );
            }
            let height = (ui.available_height() * 0.48).max(160.0);
            egui::Panel::top("composer_request")
                .default_size(height)
                .min_size(115.0)
                .resizable(true)
                .show(ui, |ui| {
                    let tabs = if raw {
                        &["请求体", "脚本", "设置"][..]
                    } else {
                        &EDITOR_TABS[..]
                    };
                    if !tabs.contains(&text(request, "editorTab")) {
                        request["editorTab"] = json!(tabs[0]);
                    }
                    ui.horizontal(|ui| {
                        for tab in tabs {
                            if ui
                                .selectable_label(text(request, "editorTab") == *tab, *tab)
                                .clicked()
                            {
                                request["editorTab"] = json!(tab);
                                changed = true;
                            }
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("composer_request_fields")
                        .show(ui, |ui| {
                            changed |= match text(request, "editorTab") {
                                "请求头" => header_editor(ui, request),
                                "请求体" if raw => {
                                    multiline(ui, request, "body", "HTTP 报文", 9)
                                }
                                "请求体" => body_editor(ui, request),
                                "授权" => authorization(ui, request),
                                "脚本" => scripts(ui, request),
                                "设置" => settings(ui, request, raw),
                                _ => parameters(ui, request),
                            };
                        });
                });
            ui.horizontal(|ui| {
                for tab in RESPONSE_TABS {
                    if ui
                        .selectable_label(text(request, "responseTab") == tab, tab)
                        .clicked()
                    {
                        request["responseTab"] = json!(tab);
                        changed = true;
                    }
                }
            });
            if let Some((id, response)) = &mut self.reply {
                if *id == self.active {
                    show_reply(ui, response, text(request, "responseTab"));
                }
            } else if busy {
                ui.weak("正在等待响应…");
            } else if let Some(id) = request["lastResponseID"]
                .as_str()
                .filter(|id| !id.is_empty())
            {
                if ui.button("读取上次响应").clicked() {
                    match worker.request("GET", format!("/history/{id}"), Value::Null) {
                        Ok(handle) => {
                            self.sending.insert(
                                self.active.clone(),
                                Sending {
                                    environment: String::new(),
                                    variables: Map::new(),
                                    remember_response: true,
                                    handle,
                                },
                            );
                        }
                        Err(error) => {
                            self.errors.insert(self.active.clone(), error);
                        }
                    }
                }
            } else {
                ui.weak("发送请求以查看响应");
            }
        });
        if text(document, "activeEnvironmentID") != active_env {
            document["activeEnvironmentID"] = json!(active_env);
        }
        if changed {
            self.mark_dirty();
        }
        if setup_environment {
            self.edit_environment(Some(&active_env));
        }
        if saving {
            self.save_dialog(index, false, false);
        }
        if let Some(kind) = draft_import {
            let input = if kind == "导入 cURL" {
                text(&requests_at(self.document.as_ref(), index), "url")
                    .trim_start()
                    .strip_prefix("curl ")
                    .map_or(String::new(), |command| format!("curl {command}"))
            } else {
                String::new()
            };
            self.transfer = Some(Transfer {
                kind,
                text: input,
                target: (kind == "导入 cURL").then(|| self.active.clone()),
                error: None,
            });
        }
        match action {
            Some(true) => self.cancel(),
            Some(false) => self.send(worker),
            None => {}
        }
    }
    pub(super) fn show_sidebar(&mut self, ui: &mut egui::Ui, _: &Worker, environments: bool) {
        self.archive_notice(ui);
        let Some(document) = &mut self.document else {
            return;
        };
        ui.heading(if environments {
            "环境变量"
        } else {
            "集合"
        });
        ui.separator();
        let mut changed = false;
        let mut edit_env = None;
        let mut open_saved = None;
        let mut delete_saved = None;
        let mut remove_folder = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            if environments {
                let current = text(document, "activeEnvironmentID").to_owned();
                let mut active = current.clone();
                ui.selectable_value(&mut active, String::new(), "无环境");
                for environment in rows(document, "environments") {
                    let id = text(environment, "id");
                    ui.push_id(id, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(active == id, name(environment))
                                .clicked()
                            {
                                active = id.into();
                            }
                            if ui.small_button("编辑").clicked() {
                                edit_env = Some(id.to_string());
                            }
                        });
                    });
                }
                if active != current {
                    document["activeEnvironmentID"] = json!(active);
                    changed = true;
                }
                if ui.button("新建环境变量").clicked() {
                    edit_env = Some(String::new());
                }
            } else {
                for folder in folders(document) {
                    egui::CollapsingHeader::new(&folder)
                        .default_open(true)
                        .show(ui, |ui| {
                            for entry in rows(document, "collections")
                                .iter()
                                .filter(|entry| text(entry, "folder") == folder)
                            {
                                let id = text(entry, "id");
                                ui.push_id(id, |ui| {
                                    let request = &entry["request"];
                                    let item = ui.selectable_label(
                                        false,
                                        format!("{}  {}", text(request, "method"), name(request)),
                                    );
                                    if item.clicked() {
                                        open_saved = Some(entry.clone());
                                    }
                                    item.context_menu(|ui| {
                                        if ui.button("删除 API").clicked() {
                                            delete_saved = Some(id.to_string());
                                            ui.close();
                                        }
                                    });
                                });
                            }
                            if folder != DEFAULT_FOLDER
                                && !rows(document, "collections")
                                    .iter()
                                    .any(|entry| text(entry, "folder") == folder)
                                && ui.small_button("删除空集合").clicked()
                            {
                                remove_folder = Some(folder.clone());
                            }
                        });
                }
                if ui.button("新建文件夹").clicked() {
                    self.new_folder = Some(String::new());
                }
                if ui.button("导入请求 JSON").clicked() {
                    self.transfer = Some(Transfer {
                        kind: "导入请求 JSON",
                        text: String::new(),
                        target: None,
                        error: None,
                    });
                }
            }
        });
        if let Some(folder) = remove_folder {
            rows_mut(document, "collectionFolders")
                .retain(|entry| entry.as_str() != Some(folder.as_str()));
            changed = true;
        }
        if let Some(id) = delete_saved {
            self.delete_collection_entry = Some(id);
        }
        if changed {
            self.mark_dirty();
        }
        if let Some(id) = edit_env {
            self.edit_environment(Some(&id));
        }
        if let Some(entry) = open_saved {
            match normalized(&entry["request"]) {
                Ok(mut request) => {
                    request["collectionEntryID"] = entry["id"].clone();
                    self.add(request);
                }
                Err(error) => self
                    .errors
                    .insert(self.active.clone(), error)
                    .map(drop)
                    .unwrap_or_default(),
            }
        }
    }
    pub(super) fn show_specifications(&mut self, ui: &mut egui::Ui) {
        let mut selected = None;
        if let Some(document) = &self.document {
            for definition in rows(document, "specifications") {
                if ui.selectable_label(false, name(definition)).clicked() {
                    selected = Some(definition.clone());
                }
            }
        }
        if let Some(definition) = selected {
            if let Some(open) = self.document.as_ref().and_then(|doc| {
                rows(doc, "requests")
                    .iter()
                    .find(|entry| text(entry, "specificationID") == text(&definition, "id"))
            }) {
                self.select(text(open, "id").into());
            } else {
                let mut request = draft("protobuf");
                request["name"] = definition["name"].clone();
                request["body"] = definition["source"].clone();
                request["specificationID"] = definition["id"].clone();
                self.add(request);
            }
        }
        if ui.button("新建 Protobuf").clicked() {
            self.open_protobuf();
        }
    }
    fn save_specification(&mut self, tab: &str, name: String, source: String) {
        let Some(request) = self.request_mut(tab) else {
            return;
        };
        let spec_id = request["specificationID"]
            .as_str()
            .map_or_else(id, str::to_owned);
        let unchanged = text(request, "body") == source;
        request["specificationID"] = json!(spec_id);
        if unchanged {
            request["name"] = json!(name);
        }
        let document = self.document.as_mut().unwrap();
        let specs = rows_mut(document, "specifications");
        let entry = json!({"id":spec_id,"name":name,"source":source});
        if let Some(old) = specs.iter_mut().find(|item| text(item, "id") == spec_id) {
            *old = entry;
        } else {
            specs.push(entry);
        }
        self.mark_dirty();
    }
    fn edit_environment(&mut self, env_id: Option<&str>) {
        let value = self
            .document
            .as_ref()
            .and_then(|document| {
                rows(document, "environments")
                    .iter()
                    .find(|entry| Some(text(entry, "id")) == env_id)
            })
            .cloned()
            .unwrap_or_else(|| json!({"id":id(), "name":"新环境", "variables":[]}));
        self.environment_edit = Some(EnvironmentEdit {
            value,
            removed: false,
        });
    }
    pub(super) fn show_dialogs(&mut self, ctx: &egui::Context, worker: &Worker) {
        if let Some(mut save) = self.save_tab.take() {
            let mut outcome = None;
            let mut cancelled = false;
            let mut open = true;
            let folders = folders(self.document.as_ref().unwrap_or(&Value::Null));
            egui::Window::new("保存 API")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.label("名称");
                    ui.text_edit_singleline(&mut save.name);
                    ui.label("保存至");
                    egui::ComboBox::from_id_salt("save_api_folder")
                        .selected_text(&save.folder)
                        .show_ui(ui, |ui| {
                            for folder in folders {
                                ui.selectable_value(&mut save.folder, folder.clone(), folder);
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!save.name.trim().is_empty(), egui::Button::new("保存"))
                            .clicked()
                        {
                            outcome = Some(true);
                        }
                        if save.close && ui.button("不保存").clicked() {
                            outcome = Some(false);
                        }
                        cancelled = ui.button("取消").clicked();
                    });
                });
            match outcome {
                Some(true) => {
                    self.store_tab(save);
                    self.save_archive(worker);
                }
                Some(false) => {
                    if let Some(index) = self.request_index(&save.id) {
                        self.close_tab(index, worker);
                    }
                }
                None if open
                    && !cancelled
                    && !ctx.input(|input| input.key_pressed(egui::Key::Escape)) =>
                {
                    self.save_tab = Some(save)
                }
                None => {}
            }
        }
        if let Some(mut item) = self.transfer.take() {
            let mut open = true;
            let mut parsed = None;
            let mut cancel = false;
            egui::Window::new(item.kind)
                .open(&mut open)
                .collapsible(false)
                .default_width(580.0)
                .show(ctx, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut item.text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(8),
                    );
                    if let Some(error) = &item.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("导入").clicked() {
                            let result = if item.kind == "导入 cURL" {
                                request_from_curl(item.text.trim())
                            } else {
                                serde_json::from_str(&item.text)
                                    .map_err(|error| error.to_string())
                                    .and_then(|value| normalized(&value))
                            };
                            match result {
                                Ok(value) => parsed = Some(value),
                                Err(error) => item.error = Some(error),
                            }
                        }
                        cancel = ui.button("取消").clicked();
                    });
                });
            if let Some(value) = parsed {
                if let Some(target) = &item.target {
                    if let Some(request) = self.request_mut(target) {
                        for key in [
                            "method",
                            "url",
                            "headers",
                            "headerFields",
                            "body",
                            "bodyMode",
                            "bodyFields",
                            "bodyFile",
                            "bodyMIME",
                            "authorizationType",
                            "authorizationUser",
                            "authorizationSecret",
                            "protocolVersion",
                        ] {
                            request[key] = value[key].clone();
                        }
                        request["bodyIncomplete"] = json!(false);
                        self.mark_dirty();
                    }
                } else {
                    self.add(value);
                }
            } else if open && !cancel {
                self.transfer = Some(item);
            }
        }
        if let Some(mut edit) = self.environment_edit.take() {
            let mut open = true;
            let mut cancel = false;
            let mut commit = false;
            let env_id = text(&edit.value, "id").to_owned();
            let existing = self.document.as_ref().is_some_and(|doc| {
                rows(doc, "environments")
                    .iter()
                    .any(|row| text(row, "id") == env_id)
            });
            egui::Window::new("编辑环境变量")
                .open(&mut open)
                .collapsible(false)
                .default_width(560.0)
                .show(ctx, |ui| {
                    line(ui, &mut edit.value, "name", "环境名称", 300.0);
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            fields(
                                ui,
                                rows_mut(&mut edit.value, "variables"),
                                "变量名称",
                                "变量值",
                                false,
                                true,
                            );
                        });
                    if edit.removed {
                        ui.label("确认删除这个环境？");
                        ui.horizontal(|ui| {
                            commit = ui.button("确认删除").clicked();
                            if ui.button("返回").clicked() {
                                edit.removed = false;
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            let valid = !text(&edit.value, "name").trim().is_empty();
                            commit = ui
                                .add_enabled(valid, egui::Button::new("保存环境"))
                                .clicked();
                            if existing && ui.button("删除环境").clicked() {
                                edit.removed = true;
                            }
                        });
                    }
                    cancel = ui.button("取消").clicked();
                });
            if commit {
                if let Some(doc) = &mut self.document {
                    let environments = rows_mut(doc, "environments");
                    if edit.removed {
                        environments.retain(|row| text(row, "id") != env_id);
                        if text(doc, "activeEnvironmentID") == env_id {
                            doc["activeEnvironmentID"] = Value::Null;
                        }
                    } else if let Some(row) = environments
                        .iter_mut()
                        .find(|row| text(row, "id") == env_id)
                    {
                        *row = edit.value;
                    } else {
                        environments.push(edit.value);
                        doc["activeEnvironmentID"] = json!(env_id);
                    }
                    self.mark_dirty();
                }
            } else if open && !cancel {
                self.environment_edit = Some(edit);
            }
        }
        if let Some(mut folder) = self.new_folder.take() {
            let mut open = true;
            let mut create = false;
            let mut cancel = false;
            egui::Window::new("新建文件夹")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.text_edit_singleline(&mut folder);
                    ui.horizontal(|ui| {
                        create = ui
                            .add_enabled(!folder.trim().is_empty(), egui::Button::new("创建"))
                            .clicked();
                        cancel = ui.button("取消").clicked();
                    });
                });
            if create {
                if let Some(doc) = &mut self.document {
                    if !folders(doc).iter().any(|entry| entry == folder.trim()) {
                        rows_mut(doc, "collectionFolders").push(json!(folder.trim()));
                        self.mark_dirty();
                    }
                }
            } else if open && !cancel {
                self.new_folder = Some(folder);
            }
        }
        if let Some(id) = self.delete_collection_entry.take() {
            let mut open = true;
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("删除已保存 API")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("删除集合中的这个请求？");
                    ui.horizontal(|ui| {
                        confirm = ui.button("删除").clicked();
                        cancel = ui.button("取消").clicked();
                    });
                });
            if confirm {
                if let Some(doc) = &mut self.document {
                    rows_mut(doc, "collections").retain(|entry| text(entry, "id") != id);
                    for request in rows_mut(doc, "requests")
                        .iter_mut()
                        .filter(|req| text(req, "collectionEntryID") == id)
                    {
                        request["collectionEntryID"] = Value::Null;
                    }
                    self.mark_dirty();
                }
            } else if open && !cancel {
                self.delete_collection_entry = Some(id);
            }
        }
    }
}

pub(super) fn capture_curl(row: &Row, detail: &Detail, bytes: &[u8]) -> Result<String, String> {
    if detail.notice.is_some() {
        return Err("完整请求正文不可用".into());
    }
    let mut request = draft("http");
    request["method"] = json!(row.method);
    request["url"] = json!(row.url);
    request["protocolVersion"] = json!(row.protocol);
    apply_capture_detail(&mut request, detail, bytes);
    if text(&request, "bodyMode") == "Base64" {
        return Err("二进制正文请在 API 编辑器中另存为文件后使用 cURL".into());
    }
    curl(&request, &Map::new())
}
fn apply_capture_detail(value: &mut Value, detail: &Detail, bytes: &[u8]) {
    value["headers"] = json!(
        detail
            .headers
            .lines()
            .filter(|line| {
                line.split_once(':').is_some_and(|(key, _)| {
                    ![
                        "content-length",
                        "content-encoding",
                        "transfer-encoding",
                        "connection",
                        "proxy-authorization",
                        "proxy-connection",
                    ]
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name))
                })
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    );
    if let Some(text) = std::str::from_utf8(bytes)
        .ok()
        .filter(|value| !value.contains('\0'))
    {
        value["body"] = json!(text);
        value["bodyMode"] = json!("文本");
    } else {
        value["bodyMode"] = json!("Base64");
        value["body"] = json!(STANDARD.encode(bytes));
    }
    value["bodyIncomplete"] = json!(detail.notice.is_some());
}

fn requests_at(document: Option<&Value>, index: usize) -> Value {
    document
        .and_then(|doc| rows(doc, "requests").get(index))
        .cloned()
        .unwrap_or_default()
}
fn folders(document: &Value) -> Vec<String> {
    let mut values = rows(document, "collectionFolders")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for entry in rows(document, "collections") {
        let folder = text(entry, "folder");
        if !folder.is_empty() && !values.iter().any(|item| item == folder) {
            values.push(folder.into());
        }
    }
    if values.is_empty() {
        values.push(DEFAULT_FOLDER.into());
    }
    values
}
fn modified(document: &Value, request: &Value) -> bool {
    let existing = text(request, "collectionEntryID");
    if let Some(saved) = rows(document, "collections")
        .iter()
        .find(|row| text(row, "id") == existing)
    {
        return content_fields(request.clone()) != content_fields(saved["request"].clone());
    }
    let kind = text(request, "kind");
    if kind == "protobuf"
        && !text(request, "specificationID").is_empty()
        && let Some(spec) = rows(document, "specifications")
            .iter()
            .find(|spec| text(spec, "id") == text(request, "specificationID"))
    {
        return text(request, "name") != text(spec, "name")
            || text(request, "body") != text(spec, "source");
    }
    // Compare blank fields without allocating or hashing a potentially large body every frame.
    let (default_name, default_body) = initial_fields(kind);
    const TEXT_DEFAULTS: &[(&str, &str)] = &[
        ("method", "GET"),
        ("url", ""),
        ("headers", ""),
        ("protocolVersion", "HTTP/1.1"),
        ("authorizationType", "无"),
        ("authorizationUser", ""),
        ("authorizationSecret", ""),
        ("bodyMode", "文本"),
        ("bodyFile", ""),
        ("bodyMIME", ""),
        ("beforeScript", ""),
        ("afterScript", ""),
        ("scriptLanguage", "python"),
        ("pythonScript", PYTHON_SCRIPT),
        ("grpcMethod", ""),
        ("protoRoot", ""),
        ("subprotocols", ""),
        ("websocketProxyURL", ""),
        ("websocketMode", "Text"),
    ];
    text(request, "name") != default_name
        || text(request, "body") != default_body
        || TEXT_DEFAULTS
            .iter()
            .any(|(key, default)| request[*key].as_str().unwrap_or(default) != *default)
        || [
            "scriptEnabled",
            "useReflection",
            "websocketUseProxy",
            "bodyIncomplete",
        ]
        .iter()
        .any(|key| request[*key] == true)
        || ["verifyTLS", "autoSaveHistory"]
            .iter()
            .any(|key| request[*key] == false)
        || ["timeout", "pingInterval"]
            .iter()
            .any(|key| request[*key].as_f64().unwrap_or(30.0) != 30.0)
        || !rows(request, "bodyFields").is_empty()
}
fn line(
    ui: &mut egui::Ui,
    value: &mut Value,
    key: &str,
    label: &str,
    width: f32,
) -> egui::Response {
    let response = ui.add(
        egui::TextEdit::singleline(object_text(value, key))
            .id_salt(key)
            .hint_text(label)
            .desired_width(width),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, label));
    response
}
fn multiline(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str, rows: usize) -> bool {
    let response = ui.add(
        egui::TextEdit::multiline(object_text(value, key))
            .id_salt(key)
            .hint_text(label)
            .font(egui::TextStyle::Monospace)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, label));
    response.changed()
}
fn choice(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str, choices: &[&str]) -> bool {
    let mut selected = text(value, key).to_string();
    let mut changed = false;
    egui::ComboBox::from_id_salt(key)
        .selected_text(if selected.is_empty() {
            label
        } else {
            selected.as_str()
        })
        .show_ui(ui, |ui| {
            for &choice in choices {
                changed |= ui
                    .selectable_value(&mut selected, choice.to_string(), choice)
                    .changed();
            }
        })
        .response
        .on_hover_text(label);
    if changed {
        value[key] = json!(selected);
    }
    changed
}
fn toggle(ui: &mut egui::Ui, value: &mut Value, key: &str, label: &str, default: bool) -> bool {
    let mut checked = value[key].as_bool().unwrap_or(default);
    let changed = ui.checkbox(&mut checked, label).changed();
    if changed {
        value[key] = json!(checked);
    }
    changed
}
fn fields(
    ui: &mut egui::Ui,
    fields: &mut Vec<Value>,
    name: &str,
    value: &str,
    files: bool,
    secrets: bool,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    let edit_width = ((ui.available_width() - if files { 116.0 } else { 80.0 }) * 0.48).max(62.0);
    for (index, field) in fields.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                changed |= toggle(ui, field, "enabled", "", true);
                changed |= line(ui, field, "name", name, edit_width).changed();
                let response = ui.add(
                    egui::TextEdit::singleline(object_text(field, "value"))
                        .id_salt("value")
                        .hint_text(value)
                        .desired_width(edit_width)
                        .password(secrets),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, value)
                });
                changed |= response.changed();
                if files {
                    changed |= toggle(ui, field, "isFile", "文件", false);
                }
                if ui.small_button("×").on_hover_text("删除字段").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        fields.remove(index);
        changed = true;
    }
    if ui
        .button(if secrets {
            "添加变量"
        } else {
            "添加一行"
        })
        .clicked()
    {
        fields.push(json!({"id":id(), "name":"", "value":"", "enabled":true}));
        changed = true;
    }
    changed
}

fn parameters(ui: &mut egui::Ui, request: &mut Value) -> bool {
    let url = text(request, "url").to_string();
    let slot = ui.id().with("query_rows");
    let mut state = ui
        .data_mut(|data| data.get_temp::<(String, Vec<Value>)>(slot))
        .filter(|draft| draft.0 == url)
        .unwrap_or_else(|| (url.clone(), query_rows(&url)));
    if !fields(ui, &mut state.1, "参数名", "参数值", false, false) {
        return false;
    }
    let next = query_url(&url, &state.1);
    let changed = url != next;
    state.0 = next;
    request["url"] = json!(state.0);
    ui.data_mut(|data| data.insert_temp(slot, state));
    changed
}
fn header_editor(ui: &mut egui::Ui, request: &mut Value) -> bool {
    if toggle(ui, request, "rawHeaders", "原始请求头", false) {
        return true;
    }
    if request["rawHeaders"] == true {
        return multiline(ui, request, "headers", "每行 Name: Value", 8);
    }
    let mut entries = rows(request, "headerFields").to_vec();
    let encode = |entries: &[Value]| {
        entries
            .iter()
            .filter(|field| field["enabled"] != false)
            .filter(|field| !text(field, "name").is_empty() || !text(field, "value").is_empty())
            .map(|field| {
                if field["raw"].is_string()
                    && field["name"] == field["originalName"]
                    && field["value"] == field["originalValue"]
                {
                    text(field, "raw").to_owned()
                } else {
                    format!("{}: {}", text(field, "name"), text(field, "value"))
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    if encode(&entries)
        != text(request, "headers")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\r\n")
    {
        entries = text(request,"headers").lines().filter(|l| !l.trim().is_empty()).map(|line| {
            let (key,value) = line.split_once(':').unwrap_or((line,""));
            json!({"name":key.trim(),"value":value.trim(),"enabled":true,"raw":line,"originalName":key.trim(),"originalValue":value.trim()})
        }).collect();
    }
    if !fields(ui, &mut entries, "请求头名称", "请求头值", false, false) {
        return false;
    }
    request["headers"] = json!(encode(&entries));
    request["headerFields"] = json!(entries);
    true
}
fn body_editor(ui: &mut egui::Ui, request: &mut Value) -> bool {
    let mut changed = choice(
        ui,
        request,
        "bodyMode",
        "正文格式",
        &["文本", "表单 URL 编码", "Multipart", "文件", "Base64"],
    );
    let multipart = text(request, "bodyMode") == "Multipart";
    let edited = match text(request, "bodyMode") {
        "表单 URL 编码" | "Multipart" => fields(
            ui,
            rows_mut(request, "bodyFields"),
            "字段名",
            "文本或文件路径",
            multipart,
            false,
        ),
        "文件" => {
            let file = line(ui, request, "bodyFile", "文件绝对路径", 370.0).changed();
            let mime = line(ui, request, "bodyMIME", "Content-Type", 370.0).changed();
            file || mime
        }
        _ => multiline(ui, request, "body", "请求正文", 8),
    };
    changed |= edited;
    if edited {
        request["bodyIncomplete"] = json!(false);
    }
    changed
}
fn authorization(ui: &mut egui::Ui, request: &mut Value) -> bool {
    let mut changed = choice(
        ui,
        request,
        "authorizationType",
        "授权类型",
        &["无", "Basic", "Bearer"],
    );
    if text(request, "authorizationType") != "无" {
        let basic = text(request, "authorizationType") == "Basic";
        if basic {
            changed |= line(ui, request, "authorizationUser", "用户名", 300.0).changed();
        }
        changed |= ui
            .add(
                egui::TextEdit::singleline(object_text(request, "authorizationSecret"))
                    .password(true)
                    .hint_text(if basic { "密码" } else { "Token" })
                    .desired_width(300.0),
            )
            .changed();
    }
    changed
}
fn scripts(ui: &mut egui::Ui, request: &mut Value) -> bool {
    let mut changed = toggle(ui, request, "scriptEnabled", "启用脚本", false);
    changed |= choice(
        ui,
        request,
        "scriptLanguage",
        "脚本语言",
        &["python", "javascript"],
    );
    if text(request, "scriptLanguage") == "python" {
        changed |= multiline(
            ui,
            request,
            "pythonScript",
            "Python onRequest / onResponse",
            8,
        );
    } else {
        ui.label("请求前 JavaScript");
        changed |= multiline(ui, request, "beforeScript", "请求前脚本", 4);
        ui.label("响应后 JavaScript");
        changed |= multiline(ui, request, "afterScript", "响应后脚本", 4);
    }
    changed
}
fn settings(ui: &mut egui::Ui, request: &mut Value, raw: bool) -> bool {
    if !raw {
        return choice(ui, request, "protocolVersion", "请求协议", &VERSIONS);
    }
    let mut changed = toggle(ui, request, "verifyTLS", "校验服务器证书", true);
    changed |= toggle(ui, request, "autoSaveHistory", "写入请求历史", true);
    ui.horizontal(|ui| {
        ui.label("超时（秒）");
        let mut timeout = request["timeout"].as_f64().unwrap_or(30.0);
        if ui
            .add(egui::DragValue::new(&mut timeout).range(0.05..=3600.0))
            .changed()
        {
            request["timeout"] = json!(timeout);
            changed = true;
        }
    });
    changed
}
fn show_reply(ui: &mut egui::Ui, reply: &mut Reply, tab: &str) {
    let summary = &reply.summary;
    ui.label(format!(
        "{}  ·  {} ms  ·  {} B  ·  {}",
        summary["status"].as_u64().unwrap_or(0),
        summary["duration"].as_u64().unwrap_or(0),
        summary["responseBytes"].as_u64().unwrap_or(0),
        text(summary, "protocol")
    ));
    if let Some(notice) = &reply.notice {
        ui.colored_label(egui::Color32::YELLOW, notice);
    }
    if tab != "响应体" {
        let values = match tab {
            "响应头" => format!(
                "{}\n{}",
                text(summary, "responseHeaders"),
                text(summary, "responseTrailers")
            ),
            "测试" => serde_json::to_string_pretty(&summary["tests"]).unwrap_or_default(),
            _ => rows(summary, "scriptLogs")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        };
        egui::ScrollArea::both().id_salt(tab).show(ui, |ui| {
            ui.monospace(values);
        });
        return;
    }
    ui.horizontal(|ui| {
        for (mode, name) in [(0, "Raw"), (1, "Hex"), (2, "JSON"), (3, "Tree")] {
            ui.selectable_value(&mut reply.mode, mode, name);
        }
        if let Content::Text { text, .. } = &reply.body {
            if ui.button("复制正文").clicked() {
                ui.ctx().copy_text(text.clone());
            }
        }
    });
    if reply.mode >= 2 {
        let value = reply.structured.get_or_insert_with(|| {
            let text = reply.body.bytes();
            if text.len() > 1024 * 1024 {
                return json!({"error": "结构化视图支持不超过 1 MiB 的 JSON 正文"});
            }
            serde_json::from_slice(text).unwrap_or_else(|_| json!({"error": "正文不是有效的 JSON"}))
        });
        ui.push_id(text(summary, "id"), |ui| {
            show_structured(ui, value, reply.mode == 3)
        });
        return;
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let scroll = egui::ScrollArea::both()
        .id_salt(("reply", text(summary, "id"), reply.mode))
        .auto_shrink([false, false]);
    if reply.mode == 1 || matches!(reply.body, Content::Binary(_)) {
        let bytes = reply.body.bytes();
        scroll.show_rows(ui, row_height, bytes.len().div_ceil(16), |ui, range| {
            for row in range {
                ui.monospace(hex_row(bytes, row));
            }
        });
    } else if let Content::Text { text, index } = &reply.body {
        scroll.show_rows(ui, row_height, index.count, |ui, range| {
            for row in index.rows(text, range) {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(row.trim_end_matches(['\r', '\n'])).monospace(),
                    )
                    .extend(),
                );
            }
        });
    }
}
