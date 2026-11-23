use crate::rules_ui::{finished, label};
use crate::worker::{RequestHandle, Worker};
use eframe::egui;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct McpUi {
    access: Option<Value>,
    reply: Option<(Reply, RequestHandle)>,
    selected: String,
    endpoint_host: String,
    cursor: bool,
    stdio: bool,
    all_tools: bool,
    editor: Option<TokenEdit>,
    confirm_remote: bool,
    error: Option<String>,
    copied: bool,
}

enum Reply {
    Loaded,
    Created,
    Updated,
    Remote,
}

enum TokenEdit {
    New(String),
    Rename { id: String, name: String },
    Revoke { id: String, name: String },
}

impl McpUi {
    pub(super) fn open(&mut self) {
        if self.reply.is_none() {
            self.access = None;
            self.error = None;
        }
        self.copied = false;
    }

    pub(super) fn poll(&mut self) {
        let Some((kind, result)) = self.reply.as_mut().and_then(|(kind, request)| {
            finished(request).map(|result| (std::mem::replace(kind, Reply::Loaded), result))
        }) else {
            return;
        };
        self.reply = None;
        match result {
            Ok(mut result) => {
                self.error = None;
                if matches!(kind, Reply::Remote) {
                    self.access = None;
                    self.confirm_remote = false;
                    return;
                }
                let tokens = result["tokens"].as_array_mut();
                if let Some(tokens) = tokens {
                    if matches!(kind, Reply::Created) {
                        self.selected = tokens.last().map_or("", |item| label(&item["id"])).into();
                    }
                    if !tokens.iter().any(|item| item["id"] == self.selected) {
                        self.selected = tokens.first().map_or("", |item| label(&item["id"])).into();
                    }
                }
                if result["remoteEnabled"] != true
                    || !result["addresses"].as_array().is_some_and(|addresses| {
                        addresses.iter().any(|ip| *ip == self.endpoint_host)
                    })
                {
                    self.endpoint_host.clear();
                }
                if matches!(kind, Reply::Created | Reply::Updated) {
                    self.editor = None;
                }
                self.access = Some(result);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn request(
        &mut self,
        worker: &Worker,
        method: &str,
        path: impl Into<String>,
        body: Value,
        kind: Reply,
    ) {
        self.copied = false;
        match worker.request(method, path, body) {
            Ok(reply) => {
                self.reply = Some((kind, reply));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, worker: &Worker, proxy_port: u16) {
        if self.access.is_none() && self.reply.is_none() && self.error.is_none() {
            self.request(worker, "GET", "/mcp-access", Value::Null, Reply::Loaded);
        }
        ui.horizontal(|ui| {
            ui.heading("MCP");
            if ui
                .add_enabled(self.reply.is_none(), egui::Button::new("刷新访问配置"))
                .clicked()
            {
                self.request(worker, "GET", "/mcp-access", Value::Null, Reply::Loaded);
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        let Some(access) = &self.access else {
            if self.reply.is_some() {
                ui.spinner();
            }
            return;
        };
        let busy = self.reply.is_some();
        let remote = access["remoteEnabled"] == true;
        let remote_port = access["remotePort"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok());
        let addresses = access["addresses"].as_array();
        let tokens = access["tokens"].as_array().map_or(&[][..], Vec::as_slice);
        let selected_name = tokens
            .iter()
            .find(|item| item["id"] == self.selected)
            .map_or("选择令牌", |item| label(&item["name"]));
        let mut request = None;
        ui.horizontal_wrapped(|ui| {
            let mut enabled = remote;
            if ui
                .add_enabled(
                    !busy,
                    egui::Checkbox::new(&mut enabled, "允许远程 MCP 访问"),
                )
                .changed()
            {
                if enabled {
                    self.confirm_remote = true;
                } else {
                    request = Some((
                        "PATCH",
                        "/settings".into(),
                        remote_patch(false, true),
                        Reply::Remote,
                    ));
                }
            }
            if remote && let Some(port) = remote_port {
                ui.label(format!("远程端口 {port}"));
            }
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong("访问令牌");
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("mcp_token")
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        for token in tokens {
                            let id = label(&token["id"]);
                            if ui
                                .selectable_label(self.selected == id, label(&token["name"]))
                                .clicked()
                            {
                                self.selected = id.to_owned();
                                self.copied = false;
                            }
                        }
                    });
                if ui.button("新建令牌").clicked() {
                    self.editor = Some(TokenEdit::New(String::new()));
                }
                if let Some(token) = tokens.iter().find(|item| item["id"] == self.selected) {
                    let id = label(&token["id"]);
                    let name = label(&token["name"]);
                    if ui.button("重命名令牌").clicked() {
                        self.editor = Some(TokenEdit::Rename {
                            id: id.to_owned(),
                            name: name.to_owned(),
                        });
                    }
                    if ui.button("撤销令牌").clicked() {
                        self.editor = Some(TokenEdit::Revoke {
                            id: id.to_owned(),
                            name: name.to_owned(),
                        });
                    }
                    if ui.button("复制令牌").clicked() {
                        ui.ctx().copy_text(label(&token["token"]).to_owned());
                        self.copied = true;
                    }
                }
            });
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong("客户端配置");
            self.copied &= !ui
                .selectable_value(&mut self.cursor, false, "Codex")
                .changed();
            self.copied &= !ui
                .selectable_value(&mut self.cursor, true, "Cursor")
                .changed();
            ui.separator();
            self.copied &= !ui
                .selectable_value(&mut self.stdio, false, "HTTP")
                .changed();
            self.copied &= !ui
                .selectable_value(&mut self.stdio, true, "Stdio")
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            let mut scope = if self.all_tools { "all" } else { "minimal" };
            egui::ComboBox::from_label("工具范围")
                .selected_text(scope)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut scope, "minimal", "minimal");
                    ui.selectable_value(&mut scope, "all", "all");
                });
            self.all_tools = scope == "all";
            if remote && let Some(addresses) = addresses {
                egui::ComboBox::from_label("连接地址")
                    .selected_text(if self.endpoint_host.is_empty() {
                        "本机"
                    } else {
                        &self.endpoint_host
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.endpoint_host.is_empty(), "本机")
                            .clicked()
                        {
                            self.endpoint_host.clear();
                        }
                        for addr in addresses {
                            if let Some(addr) = addr.as_str().filter(|addr| *addr != "127.0.0.1") {
                                if ui
                                    .selectable_label(self.endpoint_host == addr, addr)
                                    .clicked()
                                {
                                    self.endpoint_host = addr.to_owned();
                                }
                            }
                        }
                    });
            }
        });
        let (host, port) = if self.endpoint_host.is_empty() || !remote {
            ("127.0.0.1", proxy_port)
        } else {
            (self.endpoint_host.as_str(), remote_port.unwrap_or_default())
        };
        let token = tokens
            .iter()
            .find(|item| item["id"] == self.selected)
            .map(|item| label(&item["token"]));
        let executable = self.stdio.then(std::env::current_exe).and_then(Result::ok);
        let path = executable.as_deref().and_then(std::path::Path::to_str);
        let can_copy = token.is_some() && port != 0 && (!self.stdio || path.is_some()) && !busy;
        if let Some(preview) = config(
            self.cursor,
            self.stdio,
            host,
            port,
            scope(self.all_tools),
            path,
            "<ACCESS_TOKEN>",
        ) {
            egui::ScrollArea::both().max_height(250.0).show(ui, |ui| {
                ui.monospace(preview);
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(can_copy, egui::Button::new("复制客户端配置"))
                .clicked()
            {
                if let Some(text) = config(
                    self.cursor,
                    self.stdio,
                    host,
                    port,
                    scope(self.all_tools),
                    path,
                    token.unwrap(),
                ) {
                    ui.ctx().copy_text(text);
                    self.copied = true;
                }
            }
            if self.copied {
                ui.label("已复制");
            }
        });
        ui.weak(if self.cursor {
            "合并到 Cursor 的 mcp.json。复制的配置包含访问令牌。"
        } else {
            "合并到 Codex 的 config.toml。复制的配置包含访问令牌。"
        });
        if self.confirm_remote {
            let confirm =
                egui::Modal::new(egui::Id::new("mcp_remote_confirm")).show(ui.ctx(), |ui| {
                    ui.heading("允许远程 MCP 访问");
                    ui.label("将在独立端口监听，只有持有访问令牌的客户端可以连接。");
                    ui.horizontal(|ui| {
                        (
                            ui.button("取消").clicked(),
                            ui.add_enabled(!busy, egui::Button::new("允许连接"))
                                .clicked(),
                        )
                    })
                    .inner
                });
            if confirm.inner.1 {
                self.confirm_remote = false;
                request = Some((
                    "PATCH",
                    "/settings".into(),
                    remote_patch(true, false),
                    Reply::Remote,
                ));
            } else if confirm.inner.0 || confirm.should_close() {
                self.confirm_remote = false;
            }
        }
        if let Some(editor) = &mut self.editor {
            let response = egui::Modal::new(egui::Id::new("mcp_token_edit")).show(ui.ctx(), |ui| {
                let (title, name, revoke) = match editor {
                    TokenEdit::New(name) => ("新建令牌", name, false),
                    TokenEdit::Rename { name, .. } => ("重命名令牌", name, false),
                    TokenEdit::Revoke { name, .. } => ("撤销令牌", name, true),
                };
                ui.heading(title);
                if revoke {
                    ui.label(format!("撤销「{name}」将使使用它的客户端失去访问权限。"));
                } else {
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(name).hint_text("令牌名称"),
                    );
                }
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                ui.horizontal(|ui| {
                    let cancel = ui.add_enabled(!busy, egui::Button::new("取消")).clicked();
                    let save = ui
                        .add_enabled(
                            !busy && !name.trim().is_empty(),
                            egui::Button::new(if revoke { "撤销" } else { "保存" }),
                        )
                        .clicked();
                    (cancel, save)
                })
                .inner
            });
            if response.inner.1 {
                request = Some(match editor {
                    TokenEdit::New(name) => (
                        "POST",
                        "/mcp-access".into(),
                        json!({"name": name}),
                        Reply::Created,
                    ),
                    TokenEdit::Rename { id, name } => (
                        "PATCH",
                        format!("/mcp-access/{id}"),
                        json!({"name": name}),
                        Reply::Updated,
                    ),
                    TokenEdit::Revoke { id, .. } => (
                        "DELETE",
                        format!("/mcp-access/{id}"),
                        json!({}),
                        Reply::Updated,
                    ),
                });
            } else if !busy && (response.inner.0 || response.should_close()) {
                self.editor = None;
            }
        }
        if let Some((method, path, body, kind)) = request {
            self.request(worker, method, path, body, kind);
        }
    }
}

fn scope(all_tools: bool) -> &'static str {
    if all_tools { "all" } else { "minimal" }
}

fn remote_patch(enabled: bool, expected: bool) -> Value {
    json!({"mcpRemoteEnabled": enabled, "_expected": {"mcpRemoteEnabled": expected}})
}

fn config(
    cursor: bool,
    stdio: bool,
    host: &str,
    port: u16,
    scope: &str,
    path: Option<&str>,
    token: &str,
) -> Option<String> {
    let value = if stdio {
        json!({ "command": path?, "args": ["--mcp", "--host", host, "--port", &port.to_string(), "--scope", scope], "env": {"RECHO_TOKEN": token} })
    } else {
        let host = url::Host::parse(host).ok()?;
        json!({"url": format!("http://{host}:{port}/mcp?scope={scope}"), "headers": {"Authorization": format!("Bearer {token}")}})
    };
    Some(if cursor {
        serde_json::to_string_pretty(&json!({"mcpServers": {"recho": value}})).ok()?
    } else if stdio {
        format!(
            "[mcp_servers.recho]\ncommand = {}\nargs = {}\nenv = {{ RECHO_TOKEN = {} }}",
            value["command"], value["args"], value["env"]["RECHO_TOKEN"]
        )
    } else {
        format!(
            "[mcp_servers.recho]\nurl = {}\nhttp_headers = {{ Authorization = {} }}",
            value["url"], value["headers"]["Authorization"]
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_configs_share_endpoint_scope_and_token() {
        let value = config(
            true,
            true,
            "127.0.0.1",
            9988,
            "minimal",
            Some("/app/recho"),
            "fixture",
        )
        .unwrap();
        let value: Value = serde_json::from_str(&value).unwrap();
        let server = &value["mcpServers"]["recho"];
        assert_eq!(
            server["args"],
            json!([
                "--mcp",
                "--host",
                "127.0.0.1",
                "--port",
                "9988",
                "--scope",
                "minimal"
            ])
        );
        assert_eq!(server["env"]["RECHO_TOKEN"], "fixture");
        let http = config(false, false, "127.0.0.1", 9988, "minimal", None, "fixture").unwrap();
        assert!(http.contains("http://127.0.0.1:9988/mcp?scope=minimal"));
        assert!(http.contains("Bearer fixture"));
    }
}
