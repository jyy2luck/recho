use super::{RequestHandle, Value, Worker, egui, finished, json, label};
use crate::theme::Theme;
use recho_core::native::parse_header_lines;
use std::fmt::Write as _;

pub(super) fn title(value: &Value) -> String {
    let phase = if value["phase"] == "request" {
        "请求"
    } else {
        "响应"
    };
    format!("{phase}断点 · {}", label(&value["ruleName"]))
}

pub(super) struct Breakpoint {
    pub id: String,
    summary: Value,
    request: Option<RequestHandle>,
    action: Option<RequestHandle>,
    draft: Option<Message>,
    error: Option<String>,
}

impl Breakpoint {
    pub fn new(summary: Value, worker: &Worker) -> Self {
        let mut item = Self {
            id: label(&summary["id"]).to_owned(),
            summary,
            request: None,
            action: None,
            draft: None,
            error: None,
        };
        item.fetch(worker);
        item
    }

    fn fetch(&mut self, worker: &Worker) {
        self.error = None;
        let path = format!("/breakpoints/{}", self.id);
        match worker.request("GET", path, Value::Null) {
            Ok(reply) => self.request = Some(reply),
            Err(error) => self.error = Some(error),
        }
    }

    pub fn poll(&mut self) -> bool {
        if let Some(result) = self.request.as_mut().and_then(finished) {
            self.request = None;
            match result {
                Ok(mut value) => {
                    let phase = label(&self.summary["phase"]);
                    if let Some(data) = value.get_mut(phase).filter(|data| data.is_object()) {
                        self.draft = Some(Message::from_value(phase == "request", data.take()));
                    } else {
                        self.error = Some("断点报文不可用。".into());
                    }
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.action.as_mut().and_then(finished) {
            self.action = None;
            match result {
                Ok(_) => return true,
                Err(error) => self.error = Some(error),
            }
        }
        false
    }

    pub fn show(&mut self, ctx: &egui::Context, worker: &Worker, live: bool) -> bool {
        let mut action = None;
        let waiting = self.action.is_some();
        let reply =
            Theme::modal(ctx, egui::Id::new("paused_message").with(&self.id)).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 60.0).min(740.0));
                ui.heading(title(&self.summary));
                ui.add(
                    egui::Label::new(format!(
                        "{}  {}",
                        label(&self.summary["request"]["method"]),
                        label(&self.summary["request"]["url"])
                    ))
                    .truncate(),
                );
                ui.weak(format!("到期时间 {}", label(&self.summary["expiresAt"])));
                if !live {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "断点已失效，请求已结束、取消或超时。",
                    );
                }
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                if let Some(message) = &mut self.draft {
                    ui.add_enabled_ui(live && !waiting, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height((ctx.content_rect().height() * 0.62).min(510.0))
                            .show(ui, |ui| message.show(ui));
                    });
                } else if self.request.is_some() {
                    ui.spinner();
                } else if live && ui.button("重新读取报文").clicked() {
                    self.fetch(worker);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let defer = ui
                        .add_enabled(
                            !waiting,
                            egui::Button::new(if live { "稍后处理" } else { "关闭" }),
                        )
                        .clicked();
                    if ui
                        .add_enabled(live && !waiting, egui::Button::new("中止"))
                        .clicked()
                    {
                        action = Some("drop");
                    }
                    if ui
                        .add_enabled(
                            live && !waiting && self.draft.is_some(),
                            egui::Button::new("继续"),
                        )
                        .clicked()
                    {
                        action = Some("continue");
                    }
                    defer
                })
                .inner
            });
        if let Some(action) = action {
            let phase = label(&self.summary["phase"]);
            let mut body = json!({"action": action});
            let result = (|| -> Result<RequestHandle, String> {
                if action == "continue" {
                    let draft = self.draft.as_ref().ok_or("断点报文不可用。")?;
                    if draft.changed {
                        body[phase] = draft.value()?;
                    }
                }
                worker.request("POST", format!("/breakpoints/{}", self.id), body)
            })();
            match result {
                Ok(reply) => {
                    self.action = Some(reply);
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
        reply.inner
    }
}

struct Message {
    request: bool,
    method: String,
    url: String,
    status: u16,
    headers: String,
    body: Option<String>,
    binary: bool,
    body_length: usize,
    original_headers: Value,
    headers_changed: bool,
    changed: bool,
}

impl Message {
    fn from_value(request: bool, mut value: Value) -> Self {
        let binary = value.get("bodyBase64").is_some();
        let body = take_string(&mut value, if binary { "bodyBase64" } else { "body" });
        let body_length = body.as_ref().map_or(0, String::len);
        let original_headers = value["headers"].take();
        Self {
            request,
            method: take_string(&mut value, "method").unwrap_or_default(),
            url: take_string(&mut value, "url").unwrap_or_default(),
            status: value["status"].as_u64().unwrap_or(200) as u16,
            headers: header_lines(&original_headers),
            body,
            binary,
            body_length,
            original_headers,
            headers_changed: false,
            changed: false,
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        if self.request {
            ui.horizontal(|ui| {
                ui.label("方法");
                self.changed |= ui
                    .add(egui::TextEdit::singleline(&mut self.method).desired_width(85.0))
                    .changed();
                ui.label("URL");
                self.changed |= ui
                    .add(egui::TextEdit::singleline(&mut self.url).desired_width(f32::INFINITY))
                    .changed();
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("状态码");
                self.changed |= ui
                    .add(egui::DragValue::new(&mut self.status).speed(1.0))
                    .changed();
            });
        }
        ui.label("消息头");
        let changed = text_area(ui, &mut self.headers, 4);
        self.headers_changed |= changed;
        self.changed |= changed;
        ui.label(if self.binary {
            "Base64 正文"
        } else {
            "正文"
        });
        // shortcut: large payloads keep their original bytes or accept complete replacement, until virtual body editing is available.
        if self.body_length > 256 * 1024 {
            ui.label(format!(
                "正文为 {} 字节，直接继续会保留已有正文。",
                self.body_length
            ));
            if ui.button("替换完整正文").clicked() {
                self.body_length = 0;
                self.body = Some(String::new());
                self.changed = true;
            }
        } else if let Some(body) = &mut self.body {
            self.changed |= text_area(ui, body, 8);
        } else if ui.button("添加正文").clicked() {
            self.body = Some(String::new());
            self.changed = true;
        }
    }

    fn value(&self) -> Result<Value, String> {
        let headers = if self.headers_changed {
            parse_header_lines(&self.headers)?
        } else {
            self.original_headers.clone()
        };
        let mut value = if self.request {
            json!({"method": self.method, "url": self.url, "headers": headers})
        } else {
            json!({"status": self.status, "headers": headers})
        };
        if let Some(body) = &self.body {
            value[if self.binary { "bodyBase64" } else { "body" }] = json!(body);
        }
        Ok(value)
    }
}

pub(super) fn take_string(value: &mut Value, key: &str) -> Option<String> {
    match value[key].take() {
        Value::String(text) => Some(text),
        _ => None,
    }
}

pub(super) fn header_lines(headers: &Value) -> String {
    let mut text = String::new();
    for (name, value) in headers.as_object().into_iter().flatten() {
        if let Some(values) = value.as_array() {
            for value in values {
                let _ = writeln!(text, "{name}: {}", label(value));
            }
        } else if let Some(value) = value.as_str() {
            let _ = writeln!(text, "{name}: {value}");
        }
    }
    text
}

pub(super) fn text_area(ui: &mut egui::Ui, text: &mut String, rows: usize) -> bool {
    ui.add(
        egui::TextEdit::multiline(text)
            .font(egui::TextStyle::Monospace)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    )
    .changed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_edit_preserves_binary_and_duplicate_headers() {
        let initial =
            json!({"status": 200, "headers": {"set-cookie": ["a=1", "b=2"]}, "bodyBase64": "AP8="});
        let mut message = Message::from_value(false, initial.clone());
        message.status = 204;
        message.headers = "Set-Cookie: a=1\nset-cookie: b=2\n".into();
        message.headers_changed = true;
        let value = message.value().unwrap();
        assert_eq!(value["bodyBase64"], initial["bodyBase64"]);
        assert_eq!(value["headers"], initial["headers"]);
        assert_eq!(value["status"], 204);
        message.headers = "bad header".into();
        assert!(message.value().is_err());
    }
}
