use super::{
    RequestHandle, STANDARD, Worker, choice, line, multiline,
    payload::{file_bytes, render},
    rows, take, text, toggle,
};
use crate::files::{FilePick, pick};
use base64::Engine as _;
use eframe::egui;
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::error::TryRecvError;
use tokio_tungstenite::tungstenite::Message;

#[path = "socket.rs"]
mod socket;
use socket::{Event, Socket};

pub(super) enum Update {
    Specification { name: String, source: String },
}
enum Action {
    Inspect,
    Save { name: String, source: String },
    Call,
    Events,
    Operation,
    Finish,
}
struct Call {
    id: String,
    streaming: bool,
    finished: bool,
    sequence: u64,
    next_poll: Instant,
}
#[derive(Default)]
pub(super) struct Protocol {
    schema: Value,
    schema_digest: u64,
    request: Option<(Action, RequestHandle)>,
    files: Option<(bool, FilePick)>,
    events: VecDeque<(String, String)>,
    event_chars: usize,
    discarded: usize,
    socket: Option<Socket>,
    connected: bool,
    rpc: Option<Call>,
    error: Option<String>,
    status: Option<String>,
}
impl Protocol {
    fn event(&mut self, kind: impl Into<String>, value: String) {
        self.event_chars += value.len();
        self.events.push_back((kind.into(), value));
        while self.events.len() > 200
            || (self.event_chars > 2 * socket::LIMIT && self.events.len() > 1)
        {
            if let Some((_, removed)) = self.events.pop_front() {
                self.event_chars -= removed.len();
                self.discarded += 1;
            }
        }
    }
    fn request(
        &mut self,
        worker: &Worker,
        verb: &str,
        path: impl Into<String>,
        body: Value,
        action: Action,
    ) {
        if !matches!(action, Action::Events) {
            self.error = None;
        }
        match worker.request(verb, path, body) {
            Ok(handle) => self.request = Some((action, handle)),
            Err(error) => self.error = Some(error),
        }
    }
    pub(super) fn poll(&mut self, worker: &Worker, ctx: &egui::Context) -> Option<Update> {
        let mut socket_closed = false;
        if let Some(socket) = &mut self.socket {
            let mut events = Vec::new();
            // Keep rendering responsive if a peer sends continuously.
            for _ in 0..16 {
                match socket.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        socket_closed = true;
                        break;
                    }
                }
            }
            if !socket.events.is_empty() {
                ctx.request_repaint();
            }
            for event in events {
                match event {
                    Event::Connected(headers) => {
                        self.connected = true;
                        self.status = Some("已连接".into());
                        self.event("握手响应", headers);
                    }
                    Event::Text(text) => self.event("收到文本", text),
                    Event::Binary(bytes) => self.event("收到二进制", STANDARD.encode(bytes)),
                    Event::Error(error) => {
                        self.error = Some(error);
                        socket_closed = true;
                    }
                    Event::Closed => socket_closed = true,
                }
            }
        }
        if socket_closed {
            self.socket = None;
            self.connected = false;
            self.status = Some("连接已关闭".into());
        }
        if let Some(result) = self.request.as_mut().and_then(|(_, handle)| take(handle)) {
            let (action, _) = self.request.take().unwrap();
            match (action, result) {
                (Action::Inspect, Ok(value)) => {
                    self.schema = value;
                    self.status = Some("服务定义已加载".into());
                }
                (Action::Save { name, source }, Ok(_)) => {
                    self.status = Some("规范校验通过".into());
                    return Some(Update::Specification { name, source });
                }
                (Action::Call, Ok(value)) => {
                    self.status = Some("调用进行中".into());
                    self.rpc = Some(Call {
                        id: text(&value, "id").into(),
                        streaming: value["method"]["clientStreaming"] == true,
                        finished: value["method"]["clientStreaming"] != true,
                        sequence: 0,
                        next_poll: Instant::now(),
                    });
                }
                (Action::Events, Ok(value)) => {
                    for event in rows(&value, "events") {
                        let label = match text(event, "kind") {
                            "headers" | "metadata" => "响应元信息",
                            "message" | "received" => "响应消息",
                            "sent" => "已发送",
                            "trailers" => "尾部",
                            "status" => "调用状态",
                            "request-end" => "请求流结束",
                            kind => kind,
                        };
                        let value =
                            serde_json::to_string_pretty(&event["value"]).unwrap_or_default();
                        self.event(label, value);
                    }
                    if value["completed"] == true {
                        self.rpc = None;
                        self.status = Some(format!("调用结束 · gRPC {}", value["code"]));
                    } else if let Some(rpc) = &mut self.rpc {
                        rpc.sequence = value["sequence"].as_u64().unwrap_or(rpc.sequence);
                    }
                }
                (Action::Operation, Ok(_)) => {}
                (Action::Finish, Ok(_)) => {
                    if let Some(rpc) = &mut self.rpc {
                        rpc.finished = true;
                    }
                }
                (action, Err(error)) => {
                    if matches!(action, Action::Events) {
                        self.rpc = None;
                    }
                    self.error = Some(error);
                }
            }
        }
        if self.request.is_none() {
            if let Some(rpc) = &mut self.rpc {
                let delay = rpc.next_poll.saturating_duration_since(Instant::now());
                if delay.is_zero() {
                    rpc.next_poll = Instant::now() + Duration::from_millis(250);
                    let path = format!("/grpc-client/{}/events?after={}", rpc.id, rpc.sequence);
                    self.request(worker, "GET", path, Value::Null, Action::Events);
                } else {
                    ctx.request_repaint_after(delay);
                }
            }
        }
        None
    }
    pub(super) fn close(&mut self, worker: &Worker) -> Option<(bool, RequestHandle)> {
        if let Some(socket) = &mut self.socket {
            socket.disconnect();
        }
        if matches!(self.request.as_ref(), Some((Action::Call, _))) {
            // Start may already have allocated a call. Wait for its ID so it can be cancelled.
            return self.request.take().map(|(_, handle)| (true, handle));
        }
        let rpc = self.rpc.take()?;
        worker
            .request(
                "DELETE",
                format!("/grpc-client/{}/cancel", rpc.id),
                json!({}),
            )
            .ok()
            .map(|handle| (false, handle))
    }
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        draft: &mut Value,
        specs: &[Value],
        variables: &Map<String, Value>,
        proxy_port: u16,
    ) -> bool {
        let kind = text(draft, "kind").to_string();
        let mut changed = false;
        let salt = text(draft, "id").to_owned();
        ui.push_id(salt, |ui| {
            ui.horizontal(|ui| {
                changed |= line(
                    ui,
                    draft,
                    "name",
                    if kind == "protobuf" {
                        "规范文件名"
                    } else {
                        "请求名称"
                    },
                    260.0,
                )
                .changed();
                if let Some(status) = &self.status {
                    ui.weak(status);
                }
            });
            ui.separator();
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            let height = (ui.available_height() * 0.58).max(180.0);
            egui::Panel::top("protocol_inputs")
                .default_size(height)
                .min_size(160.0)
                .resizable(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("protocol_fields")
                        .show(ui, |ui| {
                            changed |= match kind.as_str() {
                                "websocket" => self.websocket(ui, draft, variables, proxy_port),
                                "grpc" => self.grpc(ui, worker, draft, specs, variables),
                                _ => self.protobuf(ui, worker, draft, specs),
                            };
                        });
                });
            if kind != "protobuf" {
                ui.horizontal(|ui| {
                    ui.strong("会话");
                    if self.discarded > 0 {
                        ui.weak(format!("更早的 {} 条已移出列表", self.discarded));
                    }
                    if ui.button("清空消息").clicked() {
                        self.events.clear();
                        self.event_chars = 0;
                        self.discarded = 0;
                    }
                });
                egui::ScrollArea::both()
                    .id_salt("protocol_events")
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for (kind, value) in &self.events {
                            egui::CollapsingHeader::new(format!("{kind} · {} B", value.len()))
                                .default_open(value.len() <= 512)
                                .show(ui, |ui| {
                                    if value.len() <= 512 {
                                        ui.monospace(value);
                                    } else {
                                        let end = value.floor_char_boundary(512);
                                        ui.monospace(&value[..end]);
                                        if ui.button("复制完整消息").clicked() {
                                            ui.ctx().copy_text(value.clone());
                                        }
                                    }
                                });
                        }
                    });
            }
        });
        changed
    }
    fn websocket(
        &mut self,
        ui: &mut egui::Ui,
        draft: &mut Value,
        variables: &Map<String, Value>,
        port: u16,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= line(
                ui,
                draft,
                "url",
                "WebSocket URL",
                (ui.available_width() - 138.0).max(160.0),
            )
            .changed();
            let connected = self.socket.is_some();
            if ui
                .button(if self.connected {
                    "关闭连接"
                } else if connected {
                    "取消连接"
                } else {
                    "连接"
                })
                .clicked()
            {
                if let Some(socket) = &mut self.socket {
                    socket.disconnect();
                    self.status = Some("正在关闭…".into());
                } else {
                    let options = websocket_options(draft, variables, port);
                    match options.and_then(|options| Socket::start(options, ui.ctx().clone())) {
                        Ok(socket) => {
                            self.socket = Some(socket);
                            self.error = None;
                            self.status = Some("正在连接…".into());
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
        });
        changed |= multiline(ui, draft, "headers", "握手消息头", 3);
        changed |= line(ui, draft, "subprotocols", "子协议（逗号分隔）", 340.0).changed();
        ui.horizontal(|ui| {
            changed |= toggle(ui, draft, "websocketUseProxy", "通过 HTTP 代理连接", false);
            if draft["websocketUseProxy"] == true {
                changed |= line(
                    ui,
                    draft,
                    "websocketProxyURL",
                    "代理地址（留空使用本地代理）",
                    340.0,
                )
                .changed();
            }
        });
        ui.horizontal(|ui| {
            changed |= seconds(ui, draft, "timeout", "超时（秒）", 30.0, 0.05);
            changed |= seconds(ui, draft, "pingInterval", "Ping 间隔（秒）", 30.0, 0.0);
        });
        ui.separator();
        ui.horizontal(|ui| {
            if text(draft, "websocketMode").is_empty() {
                draft["websocketMode"] = json!("Text");
            }
            changed |= choice(
                ui,
                draft,
                "websocketMode",
                "消息格式",
                &["Text", "JSON", "XML", "Base64", "Hex"],
            );
            if ui
                .add_enabled(self.connected, egui::Button::new("发送消息"))
                .clicked()
            {
                let result =
                    websocket_message(draft, variables).and_then(|(message, label, preview)| {
                        self.socket.as_ref().ok_or("连接已关闭")?.send(message)?;
                        Ok((label, preview))
                    });
                match result {
                    Ok((label, preview)) => {
                        self.error = None;
                        self.event(label, preview);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        });
        changed |= multiline(ui, draft, "body", "消息", 4);
        changed
    }
    fn grpc(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        draft: &mut Value,
        specs: &[Value],
        variables: &Map<String, Value>,
    ) -> bool {
        let mut changed = line(ui, draft, "url", "连接目标 URL", ui.available_width()).changed();
        changed |= multiline(ui, draft, "headers", "元信息（每行一条）", 3);
        ui.horizontal(|ui| {
            changed |= toggle(ui, draft, "verifyTLS", "校验服务器证书", true);
            changed |= seconds(ui, draft, "timeout", "超时（秒）", 30.0, 0.05);
        });
        changed |= line(ui, draft, "protoRoot", "入口规范文件名（可选）", 340.0).changed();
        let mut inspect = None;
        ui.horizontal(|ui| {
            let idle = self.request.is_none() && self.rpc.is_none();
            if ui
                .add_enabled(idle, egui::Button::new("加载本地规范"))
                .clicked()
            {
                inspect = Some(false);
            }
            if ui
                .add_enabled(idle, egui::Button::new("服务反射"))
                .clicked()
            {
                inspect = Some(true);
            }
        });
        if let Some(reflection) = inspect {
            draft["useReflection"] = json!(reflection);
            changed = true;
            self.schema = Value::Null;
            match schema_input(draft, specs, variables) {
                Ok(input) => {
                    self.schema_digest = signature(&input);
                    self.status = Some("正在加载服务定义…".into());
                    self.request(
                        worker,
                        "POST",
                        "/grpc-client/schema",
                        input,
                        Action::Inspect,
                    );
                }
                Err(error) => self.error = Some(error),
            }
        }
        let schema_current = self.schema.is_object()
            && schema_input(draft, specs, variables)
                .is_ok_and(|input| signature(&input) == self.schema_digest);
        if self.schema.is_object() && !schema_current {
            ui.weak("连接或规范已修改，请重新加载服务定义。");
        }
        let methods = rows(&self.schema, "services")
            .iter()
            .flat_map(|service| rows(service, "methods"));
        let mut select = None;
        if self.schema.is_object() {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("grpc_method")
                    .selected_text(draft["grpcMethod"].as_str().unwrap_or("服务 / 方法"))
                    .show_ui(ui, |ui| {
                        for method in methods {
                            let path = text(method, "path");
                            if ui
                                .selectable_label(text(draft, "grpcMethod") == path, path)
                                .clicked()
                            {
                                select = Some((path.to_string(), method["template"].clone()));
                            }
                        }
                    });
                let template = rows(&self.schema, "services")
                    .iter()
                    .flat_map(|service| rows(service, "methods"))
                    .find(|method| text(method, "path") == text(draft, "grpcMethod"))
                    .map(|method| method["template"].clone());
                if let Some(template) = template {
                    if ui.button("填充 JSON 模板").clicked() {
                        draft["body"] =
                            json!(serde_json::to_string_pretty(&template).unwrap_or_default());
                        changed = true;
                    }
                }
            });
        }
        if let Some((path, _)) = select {
            draft["grpcMethod"] = json!(path);
            changed = true;
        }
        changed |= multiline(ui, draft, "body", "请求消息 JSON", 5);
        let mut grpc_operation = None;
        ui.horizontal(|ui| {
            if let Some(rpc) = &mut self.rpc {
                if rpc.streaming && !rpc.finished {
                    if ui
                        .add_enabled(!self.mutation_pending(), egui::Button::new("发送消息"))
                        .clicked()
                    {
                        grpc_operation = Some("send");
                    }
                    if ui
                        .add_enabled(!self.mutation_pending(), egui::Button::new("结束请求流"))
                        .clicked()
                    {
                        grpc_operation = Some("finish");
                    }
                }
                if ui
                    .add_enabled(!self.mutation_pending(), egui::Button::new("取消调用"))
                    .clicked()
                {
                    grpc_operation = Some("cancel");
                }
            } else if ui
                .add_enabled(
                    schema_current
                        && self.request.is_none()
                        && !text(draft, "grpcMethod").is_empty(),
                    egui::Button::new("发起调用"),
                )
                .clicked()
            {
                grpc_operation = Some("start");
            }
        });
        if let Some(action) = grpc_operation {
            let message = || {
                render(text(draft, "body"), variables).and_then(|body| {
                    serde_json::from_str::<Value>(&body)
                        .map_err(|error| format!("请求消息 JSON 无效：{error}"))
                })
            };
            if action == "start" {
                self.events.clear();
                self.event_chars = 0;
                self.discarded = 0;
                let result = (|| {
                    let mut input = grpc_options(draft, variables)?;
                    let method = text(draft, "grpcMethod");
                    input["method"] = json!(method);
                    input["descriptorSet"] = self.schema["descriptorSet"].clone();
                    input["messages"] = json!([message()?]);
                    Ok::<_, String>(input)
                })();
                match result {
                    Ok(input) => {
                        self.request(worker, "POST", "/grpc-client/start", input, Action::Call)
                    }
                    Err(error) => self.error = Some(error),
                }
            } else if let Some(rpc) = &self.rpc {
                let path = format!("/grpc-client/{}/{action}", rpc.id);
                let body = if action == "send" {
                    message().map(|message| json!({"message":message}))
                } else {
                    Ok(json!({}))
                };
                match body {
                    Ok(body) => {
                        let command = if action == "finish" {
                            Action::Finish
                        } else {
                            Action::Operation
                        };
                        self.request(worker, "POST", path, body, command);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        changed
    }
    fn mutation_pending(&self) -> bool {
        self.request
            .as_ref()
            .is_some_and(|(action, _)| !matches!(action, Action::Events))
    }
    fn protobuf(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        draft: &mut Value,
        specs: &[Value],
    ) -> bool {
        let mut changed = false;
        if let Some((save, result)) = self
            .files
            .as_mut()
            .and_then(|(save, handle)| take(handle).map(|value| (*save, value)))
        {
            self.files = None;
            let result: Result<_, String> = match result {
                Ok(Some(path)) if save => {
                    std::fs::write(path, text(draft, "body")).map_err(|error| error.to_string())
                }
                Ok(Some(path)) => (|| {
                    let bytes = file_bytes(
                        path.to_str().ok_or("路径不是有效的 UTF-8 文本")?,
                        4 * 1024 * 1024,
                    )?;
                    let source = String::from_utf8(bytes).map_err(|error| error.to_string())?;
                    draft["name"] = json!(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("service.proto")
                    );
                    draft["body"] = json!(source);
                    changed = true;
                    Ok(())
                })(),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                self.error = Some(error);
            }
        }
        let mut save = false;
        ui.horizontal(|ui| {
            let ready = self.files.is_none() && self.request.is_none();
            if ui
                .add_enabled(ready, egui::Button::new("导入源码"))
                .clicked()
            {
                self.files = Some((false, pick("proto", false, ui.ctx())));
            }
            if ui
                .add_enabled(ready, egui::Button::new("导出源码"))
                .clicked()
            {
                self.files = Some((true, pick("proto", true, ui.ctx())));
            }
            if ui
                .add_enabled(ready, egui::Button::new("校验并保存规范"))
                .clicked()
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S))
                    && ready
            {
                save = true;
            }
        });
        changed |= multiline(ui, draft, "body", "Protobuf 源码", 14);
        if save {
            self.save_proto(worker, draft, specs);
        }
        changed
    }

    pub(super) fn save_proto(&mut self, worker: &Worker, draft: &Value, specs: &[Value]) {
        if self.request.is_some() {
            return;
        }
        let name = text(draft, "name").trim();
        let name = if name.ends_with(".proto") {
            name.to_owned()
        } else {
            format!("{name}.proto")
        };
        let specification_id = text(draft, "specificationID");
        let mut sources = specs
            .iter()
            .filter(|spec| text(spec, "id") != specification_id)
            .cloned()
            .collect::<Vec<_>>();
        if name == ".proto" || sources.iter().any(|source| text(source, "name") == name) {
            self.error = Some("规范名不能为空或与其他规范重复".into());
        } else {
            let source = text(draft, "body").to_owned();
            sources.push(json!({"name":name,"source":source}));
            self.status = Some("正在校验规范…".into());
            self.request(
                worker,
                "POST",
                "/grpc-client/schema",
                json!({"root":name,"sources":sources}),
                Action::Save { name, source },
            );
        }
    }
}
fn seconds(
    ui: &mut egui::Ui,
    draft: &mut Value,
    key: &str,
    label: &str,
    default: f64,
    minimum: f64,
) -> bool {
    let mut value = draft[key].as_f64().unwrap_or(default);
    ui.label(label);
    let changed = ui
        .add(egui::DragValue::new(&mut value).range(minimum..=3600.0))
        .changed();
    if changed {
        draft[key] = json!(value);
    }
    changed
}
fn grpc_options(draft: &Value, variables: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!({
        "url": render(text(draft,"url"),variables)?,
        "metadata": render(text(draft,"headers"),variables)?,
        "verifyTLS": draft["verifyTLS"].as_bool().unwrap_or(true),
        "timeout": draft["timeout"].as_f64().unwrap_or(30.0)
    }))
}
fn schema_input(
    draft: &Value,
    specs: &[Value],
    variables: &Map<String, Value>,
) -> Result<Value, String> {
    let mut input = grpc_options(draft, variables)?;
    let reflection = draft["useReflection"] == true;
    input["reflection"] = json!(reflection);
    if !reflection {
        input["sources"] = json!(specs);
        input["root"] = json!(text(draft, "protoRoot"));
    }
    Ok(input)
}
fn signature(value: &Value) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
fn websocket_options(
    draft: &Value,
    variables: &Map<String, Value>,
    port: u16,
) -> Result<socket::Options, String> {
    let timeout = draft["timeout"].as_f64().unwrap_or(30.0);
    let ping = draft["pingInterval"].as_f64().unwrap_or(30.0);
    if !(0.05..=3600.0).contains(&timeout) || !(0.0..=3600.0).contains(&ping) {
        return Err("超时或 Ping 间隔超出范围".into());
    }
    let proxy = if draft["websocketUseProxy"] == true {
        let proxy = render(text(draft, "websocketProxyURL"), variables)?;
        Some(if proxy.trim().is_empty() {
            format!("http://127.0.0.1:{port}")
        } else {
            proxy
        })
    } else {
        None
    };
    Ok(socket::Options {
        url: render(text(draft, "url"), variables)?,
        headers: render(text(draft, "headers"), variables)?,
        protocols: render(text(draft, "subprotocols"), variables)?,
        proxy,
        timeout: Duration::from_secs_f64(timeout),
        ping: (ping != 0.0).then(|| Duration::from_secs_f64(ping)),
    })
}
fn websocket_message(
    draft: &Value,
    variables: &Map<String, Value>,
) -> Result<(Message, &'static str, String), String> {
    let body = render(text(draft, "body"), variables)?;
    let mode = text(draft, "websocketMode");
    let bytes = match mode {
        "Base64" => Some(STANDARD.decode(&body).map_err(|_| "Base64 消息编码无效")?),
        "Hex" => {
            let hex: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) || !hex.len().is_multiple_of(2) {
                return Err("Hex 需要成对的十六进制数字".into());
            }
            Some(
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect::<Vec<_>>(),
            )
        }
        _ => None,
    };
    if bytes.as_ref().map_or(body.len(), Vec::len) > socket::LIMIT {
        return Err("消息超过 1 MiB".into());
    }
    if let Some(bytes) = bytes {
        return Ok((Message::Binary(bytes.into()), "发送二进制", body));
    }
    if mode == "JSON" {
        serde_json::from_str::<Value>(&body).map_err(|error| format!("JSON 无效：{error}"))?;
    }
    Ok((Message::Text(body.clone().into()), "发送文本", body))
}
