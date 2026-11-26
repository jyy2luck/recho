use crate::rules_ui::{finished, label};
use crate::theme::Theme;
use crate::worker::{RequestHandle, Worker};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use eframe::egui;
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    Claude,
    Cursor,
    Codex,
    VsCode,
    Windsurf,
    Copilot,
    Kiro,
    Gemini,
    Trae,
    Qoder,
    CodeBuddy,
    Other,
}

impl Client {
    const ALL: [Self; 12] = [
        Self::Claude,
        Self::Cursor,
        Self::Codex,
        Self::VsCode,
        Self::Windsurf,
        Self::Copilot,
        Self::Kiro,
        Self::Gemini,
        Self::Trae,
        Self::Qoder,
        Self::CodeBuddy,
        Self::Other,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Cursor => "Cursor",
            Self::Codex => "Codex",
            Self::VsCode => "VS Code",
            Self::Windsurf => "Windsurf",
            Self::Copilot => "Copilot",
            Self::Kiro => "Kiro",
            Self::Gemini => "Gemini CLI",
            Self::Trae => "TRAE",
            Self::Qoder => "Qoder",
            Self::CodeBuddy => "CodeBuddy",
            Self::Other => "更多客户端",
        }
    }

    fn instructions(self) -> &'static str {
        match self {
            Self::Claude => "在终端运行命令，或将配置合并到项目的 .mcp.json。",
            Self::Cursor => "将配置合并到 ~/.cursor/mcp.json 或项目的 .cursor/mcp.json。",
            Self::Codex => "在终端运行命令，或将配置合并到 ~/.codex/config.toml。",
            Self::VsCode => "在命令面板打开用户 MCP 配置，或编辑项目的 .vscode/mcp.json。",
            Self::Windsurf => {
                "在 Windsurf 设置中编辑 MCP，或打开 ~/.codeium/windsurf/mcp_config.json。"
            }
            Self::Copilot => "在终端运行命令，或将配置合并到 ~/.copilot/mcp-config.json。",
            Self::Kiro => "在 Kiro 设置中打开 MCP Servers，或编辑项目的 .kiro/settings/mcp.json。",
            Self::Gemini => {
                "在终端运行命令，或将配置合并到 ~/.gemini/settings.json 或项目的 .gemini/settings.json。"
            }
            Self::Trae => "打开头像下的设置 → MCP，选择添加 → 手动添加。",
            Self::Qoder => "在 Qoder 设置中打开 MCP，选择手动添加。",
            Self::CodeBuddy => "在 CodeBuddy 侧栏打开设置 → MCP，选择添加 MCP。",
            Self::Other => "将配置合并到客户端的 MCP 设置。若客户端使用表单，可分别填写下方字段。",
        }
    }
}

#[derive(Clone, Copy)]
enum Page {
    Settings,
    Clients,
    Config(Client),
    Tokens,
}

#[derive(Default)]
pub(super) struct McpUi {
    page: Option<Page>,
    token_list: bool,
    access: Option<Value>,
    reply: Option<(Reply, RequestHandle)>,
    selected: String,
    endpoint_host: String,
    http: bool,
    all_tools: bool,
    editor: Option<TokenEdit>,
    confirm_remote: bool,
    confirm_codex: Option<(PathBuf, String)>,
    error: Option<String>,
    feedback: Option<&'static str>,
}

#[derive(Clone, Copy)]
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
        if self.is_open() {
            return;
        }
        self.page = Some(Page::Clients);
        self.access = None;
        self.error = None;
        self.feedback = None;
    }

    pub(super) fn open_tokens(&mut self) {
        if self.is_open() {
            self.token_list = true;
        } else {
            self.open();
            self.page = Some(Page::Tokens);
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.page.is_some()
    }

    pub(super) fn back(&mut self) {
        if self.reply.is_some() && (self.editor.is_some() || self.confirm_remote) {
            return;
        }
        self.feedback = None;
        self.error = None;
        if self.editor.take().is_some()
            || std::mem::take(&mut self.confirm_remote)
            || self.confirm_codex.take().is_some()
        {
            return;
        }
        if std::mem::take(&mut self.token_list) {
            return;
        }
        self.page = match self.page {
            Some(Page::Config(_)) => Some(Page::Clients),
            _ => {
                self.access = None;
                None
            }
        };
    }

    pub(super) fn poll(&mut self) {
        let Some((kind, result)) = self
            .reply
            .as_mut()
            .and_then(|(kind, request)| finished(request).map(|result| (*kind, result)))
        else {
            return;
        };
        self.reply = None;
        match result {
            Ok(result) => {
                self.error = None;
                if matches!(kind, Reply::Remote) {
                    self.access = None;
                    self.confirm_remote = false;
                    return;
                }
                if let Some(tokens) = result["tokens"].as_array() {
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
                self.access = self.is_open().then_some(result);
            }
            Err(error) => {
                if matches!(kind, Reply::Loaded | Reply::Remote) {
                    self.access = None;
                    self.confirm_remote = false;
                }
                self.error = Some(error);
            }
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
        self.feedback = None;
        match worker.request(method, path, body) {
            Ok(reply) => {
                self.reply = Some((kind, reply));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn load(&mut self, worker: &Worker) {
        self.access = None;
        self.request(worker, "GET", "/mcp-access", Value::Null, Reply::Loaded);
    }

    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        worker: &Worker,
        proxy_port: u16,
        ready: bool,
    ) {
        let Some(page) = self.page else { return };
        if !ready {
            self.access = None;
        }
        if ready && self.access.is_none() && self.reply.is_none() && self.error.is_none() {
            self.load(worker);
        }
        let dialog = Theme::modal(ctx, egui::Id::new("recho_mcp_dialog")).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(360.0, 680.0));
            if self.editor.is_some() {
                self.edit_token(ui, worker);
            } else if self.confirm_remote {
                ui.heading("允许远程 MCP 访问");
                ui.label("Recho 将在独立端口监听。只有持有访问令牌的客户端可以连接。");
                footer(ui, |ui| {
                    if ui
                        .add_enabled(self.reply.is_none(), egui::Button::new("启用远程访问"))
                        .clicked()
                    {
                        self.request(
                            worker,
                            "PATCH",
                            "/settings",
                            remote_patch(true, false),
                            Reply::Remote,
                        );
                    }
                    if ui.button("取消").clicked() {
                        self.back();
                    }
                });
            } else if let Some((path, snippet)) = &self.confirm_codex {
                ui.heading("将 Recho 写入 Codex 配置");
                ui.label(path.display().to_string());
                ui.label("将添加或替换 [mcp_servers.recho] 段。访问令牌会以明文保存在此文件中。");
                let (save, cancel) = footer(ui, |ui| {
                    let save = ui.button("同意并写入").clicked();
                    let cancel = ui.button("取消").clicked();
                    (save, cancel)
                });
                if save {
                    self.error = match write_codex_config(path, snippet) {
                        Ok(()) => {
                            self.feedback = Some("已写入 Codex 配置。");
                            None
                        }
                        Err(error) => Some(error),
                    };
                    self.confirm_codex = None;
                } else if cancel {
                    self.back();
                }
            } else if self.token_list {
                self.tokens(ui, worker, ready);
            } else {
                match page {
                    Page::Settings => self.settings(ui, worker, proxy_port, ready),
                    Page::Clients => self.clients(ui),
                    Page::Config(client) => {
                        self.client_config(ui, worker, client, proxy_port, ready)
                    }
                    Page::Tokens => self.tokens(ui, worker, ready),
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(feedback) = self.feedback {
                ui.weak(feedback);
            }
        });
        if dialog.should_close() {
            self.back();
        }
    }

    fn availability(&mut self, ui: &mut egui::Ui, worker: &Worker, ready: bool) {
        if !ready {
            ui.label("正在等待 Recho 引擎就绪…");
        } else if self.reply.is_some() {
            ui.spinner();
        } else if self.access.is_none() && ui.button("重新读取访问配置").clicked() {
            self.load(worker);
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui, worker: &Worker, proxy_port: u16, ready: bool) {
        ui.heading("MCP 服务");
        ui.weak("将 Recho 接入 AI 客户端，查询抓包和调试数据。");
        ui.separator();
        self.availability(ui, worker, ready);
        let mut enabled = self
            .access
            .as_ref()
            .is_some_and(|access| access["remoteEnabled"] == true);
        let busy = self.reply.is_some() || self.access.is_none();
        if ui
            .add_enabled(!busy, egui::Checkbox::new(&mut enabled, "允许远程访问"))
            .changed()
        {
            if enabled {
                self.confirm_remote = true;
            } else {
                self.request(
                    worker,
                    "PATCH",
                    "/settings",
                    remote_patch(false, true),
                    Reply::Remote,
                );
            }
        }
        if proxy_port != 0 {
            ui.label(format!("本机端口  {proxy_port}"));
        }
        if enabled
            && let Some(port) = self
                .access
                .as_ref()
                .and_then(|access| access["remotePort"].as_u64())
        {
            ui.label(format!("远程端口  {port}"));
        }
        ui.horizontal(|ui| {
            self.manage_tokens(ui, worker, ready);
        });
        footer(ui, |ui| {
            if ui.button("关闭").clicked() {
                self.back();
            }
            if ui.button("配置 MCP 客户端").clicked() {
                self.page = Some(Page::Clients);
            }
        });
    }

    fn clients(&mut self, ui: &mut egui::Ui) {
        ui.heading("配置 MCP 客户端");
        ui.weak("选择要连接 Recho 的客户端");
        ui.separator();
        let button_width = (ui.available_width() - 16.0) / 3.0;
        egui::Grid::new("mcp_clients")
            .num_columns(3)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                for (index, client) in Client::ALL.into_iter().enumerate() {
                    if ui
                        .add_sized([button_width, 44.0], egui::Button::new(client.name()))
                        .clicked()
                    {
                        self.page = Some(Page::Config(client));
                        self.http = client == Client::Codex;
                        self.feedback = None;
                    }
                    if index % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        footer(ui, |ui| {
            if ui.button("关闭").clicked() {
                self.back();
            }
            if ui.button("服务设置").clicked() {
                self.page = Some(Page::Settings);
            }
        });
    }

    fn manage_tokens(&mut self, ui: &mut egui::Ui, worker: &Worker, ready: bool) {
        if ui.button("管理访问令牌").clicked() {
            self.token_list = true;
            self.feedback = None;
            if ready && self.reply.is_none() {
                self.load(worker);
            }
        }
    }

    fn selected_token<'a>(&self, tokens: &'a [Value]) -> Option<&'a str> {
        if self.reply.is_some() {
            return None;
        }
        tokens
            .iter()
            .find(|item| item["id"] == self.selected)
            .and_then(|item| item["token"].as_str())
            .filter(|token| !token.is_empty())
    }

    fn client_config(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        client: Client,
        proxy_port: u16,
        ready: bool,
    ) {
        let colors = Theme::current_palette(ui.ctx());
        let size = egui::vec2(ui.available_width(), 32.0);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.heading(format!("将 Recho 连接到 {}", client.name()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (http, title) in [(false, "Stdio"), (true, "HTTP")] {
                        let selected = self.http == http;
                        let text = egui::RichText::new(title).color(if selected {
                            colors.bg
                        } else {
                            colors.text
                        });
                        let button = egui::Button::selectable(selected, text)
                            .min_size(egui::vec2(68.0, 28.0));
                        let button = if selected {
                            button.fill(colors.accent)
                        } else {
                            button
                        };
                        if ui.add(button).clicked() && !selected {
                            self.http = http;
                            self.feedback = None;
                        }
                    }
                });
            },
        );
        ui.separator();
        let codex_http = client == Client::Codex && self.http;
        let codex_path = codex_http.then(codex_config_path).flatten();
        if codex_http {
            ui.weak(
                "复制下方配置并保存到 Codex 配置文件。已有 [mcp_servers.recho] 时，请替换该段。",
            );
            let path = codex_path
                .as_deref()
                .unwrap_or(Path::new("~/.codex/config.toml"));
            let directory = codex_path.as_deref().and_then(Path::parent);
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(path.display().to_string()).monospace())
                        .wrap(),
                );
                if ui
                    .add_enabled(directory.is_some(), egui::Button::new("打开配置文件夹"))
                    .clicked()
                    && let Some(directory) = directory
                {
                    self.error = open_config_folder(directory).err();
                }
            });
        } else {
            ui.weak(client.instructions());
        }
        self.availability(ui, worker, ready);
        let access = self.access.clone().unwrap_or(Value::Null);
        let tokens = access["tokens"].as_array().map_or(&[][..], Vec::as_slice);
        egui::Grid::new("mcp_connection_options")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("访问令牌");
                egui::ComboBox::from_id_salt("mcp_token")
                    .selected_text(
                        tokens
                            .iter()
                            .find(|item| item["id"] == self.selected)
                            .map_or("未选择", |item| label(&item["name"])),
                    )
                    .show_ui(ui, |ui| {
                        for token in tokens {
                            if ui
                                .selectable_value(
                                    &mut self.selected,
                                    label(&token["id"]).into(),
                                    label(&token["name"]),
                                )
                                .changed()
                            {
                                self.feedback = None;
                            }
                        }
                    });
                self.manage_tokens(ui, worker, ready);
                ui.end_row();
                ui.label("工具范围");
                egui::ComboBox::from_id_salt("mcp_scope")
                    .selected_text(scope(self.all_tools))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(&mut self.all_tools, false, "minimal · 常用工具")
                            .changed()
                            || ui
                                .selectable_value(&mut self.all_tools, true, "all · 全部工具")
                                .changed()
                        {
                            self.feedback = None;
                        }
                    });
                ui.end_row();
                ui.label("连接地址");
                egui::ComboBox::from_id_salt("mcp_endpoint")
                    .selected_text(if self.endpoint_host.is_empty() {
                        "本机 127.0.0.1"
                    } else {
                        &self.endpoint_host
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut self.endpoint_host,
                                String::new(),
                                "本机 127.0.0.1",
                            )
                            .changed()
                        {
                            self.feedback = None;
                        }
                        for addr in access["addresses"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .filter(|addr| *addr != "127.0.0.1")
                        {
                            if ui
                                .add_enabled(
                                    access["remoteEnabled"] == true,
                                    egui::Button::selectable(self.endpoint_host == addr, addr),
                                )
                                .clicked()
                            {
                                self.endpoint_host = addr.to_owned();
                                self.feedback = None;
                            }
                        }
                    });
                ui.end_row();
            });
        let (host, port) = if self.endpoint_host.is_empty() || access["remoteEnabled"] != true {
            ("127.0.0.1", proxy_port)
        } else {
            (
                &*self.endpoint_host,
                access["remotePort"]
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok())
                    .unwrap_or_default(),
            )
        };
        let token = self.selected_token(tokens);
        let can_copy = token.is_some();
        let executable = std::env::current_exe().ok();
        let path = executable.as_deref().and_then(std::path::Path::to_str);
        let config = Config::new(
            client,
            self.http,
            host,
            port,
            scope(self.all_tools),
            path,
            token.unwrap_or("<ACCESS_TOKEN>"),
        );
        let max_height = (ui.ctx().content_rect().height() - 300.0).clamp(140.0, 440.0);
        egui::ScrollArea::vertical()
            .id_salt((client.name(), self.http))
            .max_height(max_height)
            .show(ui, |ui| {
                if let Some(config) = &config {
                    if let Some(command) = config.command() {
                        code_block(
                            ui,
                            "终端命令",
                            &command,
                            can_copy,
                            "复制命令",
                            &mut self.feedback,
                        );
                    }
                    code_block(
                        ui,
                        "手动配置",
                        &config.text(),
                        can_copy,
                        "复制配置",
                        &mut self.feedback,
                    );
                    let (title, value) = if self.http {
                        ("服务器 URL", config.endpoint())
                    } else {
                        ("辅助程序路径", path.unwrap_or_default())
                    };
                    code_block(ui, title, value, true, "复制地址或路径", &mut self.feedback);
                } else {
                    ui.label("等待代理端口或 Stdio 辅助程序可用。");
                }
            });
        if !can_copy && self.access.is_some() && self.reply.is_none() {
            ui.weak("请先新建访问令牌，然后复制命令或配置。");
        }
        if self.http && access["remoteEnabled"] != true {
            ui.weak("其他设备连接时，请在服务设置中启用远程访问并选择连接地址。");
        }
        if codex_http {
            ui.weak(
                "令牌会以明文保存在配置中。保存后请完全退出并重新打开 Codex，再用 /mcp 查看连接。",
            );
        }
        footer(ui, |ui| {
            if ui.button("完成").clicked() {
                self.page = None;
                self.access = None;
                self.feedback = None;
            }
            if ui.button("返回").clicked() {
                self.back();
            }
            if codex_http {
                let target = codex_path.zip(config.as_ref()).filter(|_| can_copy);
                if ui
                    .add_enabled(target.is_some(), egui::Button::new("写入本机 Codex 配置…"))
                    .clicked()
                    && let Some((path, config)) = target
                {
                    self.error = None;
                    self.feedback = None;
                    self.confirm_codex = Some((path, config.text()));
                }
            }
            if matches!(client, Client::Cursor | Client::VsCode) {
                let link = config
                    .as_ref()
                    .filter(|_| can_copy)
                    .and_then(Config::install_link);
                if ui
                    .add_enabled(link.is_some(), egui::Button::new("在客户端安装"))
                    .clicked()
                    && let Some(link) = link
                    && !open_install_link(&link)
                {
                    self.error = Some(format!(
                        "无法打开 {}。可复制配置后手动安装。",
                        client.name()
                    ));
                }
            }
        });
    }

    fn tokens(&mut self, ui: &mut egui::Ui, worker: &Worker, ready: bool) {
        ui.heading("管理访问令牌");
        ui.separator();
        self.availability(ui, worker, ready);
        let mut edit = None;
        if let Some(tokens) = self
            .access
            .as_ref()
            .and_then(|access| access["tokens"].as_array())
        {
            if tokens.is_empty() {
                ui.weak("暂无访问令牌");
            }
            egui::ScrollArea::vertical()
                .max_height(290.0)
                .show(ui, |ui| {
                    for token in tokens {
                        let id = label(&token["id"]);
                        let name = label(&token["name"]);
                        let credential = label(&token["token"]);
                        ui.push_id(id, |ui| {
                            ui.add_enabled_ui(self.reply.is_none(), |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.strong(name);
                                        ui.monospace(mask_token(credential));
                                        ui.weak(token["lastUsedAt"].as_str().map_or_else(
                                            || "无访问记录".to_owned(),
                                            |last_used| format!("最近访问  {last_used}"),
                                        ));
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("删除").clicked() {
                                                edit = Some(TokenEdit::Revoke {
                                                    id: id.into(),
                                                    name: name.into(),
                                                });
                                            }
                                            if ui.button("复制").clicked() {
                                                ui.ctx().copy_text(credential.into());
                                                self.feedback = Some("已复制令牌");
                                            }
                                            if ui.button("重命名").clicked() {
                                                edit = Some(TokenEdit::Rename {
                                                    id: id.into(),
                                                    name: name.into(),
                                                });
                                            }
                                        },
                                    );
                                });
                            });
                        });
                        ui.separator();
                    }
                });
        }
        footer(ui, |ui| {
            if ui.button("关闭").clicked() {
                self.back();
            }
            if ui
                .add_enabled(
                    self.access.is_some() && self.reply.is_none(),
                    egui::Button::new("新建令牌"),
                )
                .clicked()
            {
                edit = Some(TokenEdit::New(String::new()));
            }
        });
        if edit.is_some() {
            self.error = None;
            self.feedback = None;
            self.editor = edit;
        }
    }

    fn edit_token(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let (title, name, revoke) = match editor {
            TokenEdit::New(name) => ("新建访问令牌", name, false),
            TokenEdit::Rename { name, .. } => ("重命名访问令牌", name, false),
            TokenEdit::Revoke { name, .. } => ("删除访问令牌", name, true),
        };
        ui.heading(title);
        let busy = self.reply.is_some();
        if revoke {
            ui.label(format!(
                "删除「{name}」后，使用它的客户端将立即失去访问权限。"
            ));
        } else {
            ui.label("令牌名称");
            let field = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(name)
                    .hint_text("请输入名称")
                    .desired_width(600.0),
            );
            if !ui.memory(|memory| memory.focused().is_some()) {
                field.request_focus();
            }
        }
        let valid = revoke
            || (!name.trim().is_empty()
                && name.trim().chars().count() <= 100
                && !name.trim().chars().any(char::is_control));
        let (save, cancel) = footer(ui, |ui| {
            let save = ui
                .add_enabled(
                    !busy && valid,
                    egui::Button::new(if revoke { "删除" } else { "完成" }),
                )
                .clicked();
            let cancel = ui.add_enabled(!busy, egui::Button::new("取消")).clicked();
            (save, cancel)
        });
        if save {
            let (method, path, body, kind) = match editor {
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
            };
            self.request(worker, method, path, body, kind);
        } else if cancel {
            self.back();
        }
    }
}

fn footer<T>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> T) -> T {
    ui.separator();
    let size = egui::vec2(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::right_to_left(egui::Align::Center),
        contents,
    )
    .inner
}

fn code_block(
    ui: &mut egui::Ui,
    title: &str,
    text: &str,
    can_copy: bool,
    button: &'static str,
    feedback: &mut Option<&'static str>,
) {
    let colors = Theme::current_palette(ui.ctx());
    egui::Frame::new()
        .fill(ui.visuals().code_bg_color)
        .stroke(egui::Stroke::new(1.0, colors.line))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let size = egui::vec2(ui.available_width(), 26.0);
            ui.allocate_ui_with_layout(
                size,
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.strong(title);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(can_copy, egui::Button::new(button))
                            .clicked()
                        {
                            ui.ctx().copy_text(text.into());
                            *feedback = Some("已复制到剪贴板");
                        }
                    });
                },
            );
            ui.add(egui::Label::new(egui::RichText::new(text).monospace()).wrap());
        });
}

fn mask_token(token: &str) -> String {
    match (token.get(..4), token.get(token.len().saturating_sub(4)..)) {
        (Some(start), Some(end)) if token.len() >= 12 => format!("{start}••••••••{end}"),
        _ => "••••••••".into(),
    }
}

fn scope(all_tools: bool) -> &'static str {
    if all_tools { "all" } else { "minimal" }
}

fn remote_patch(enabled: bool, expected: bool) -> Value {
    json!({"mcpRemoteEnabled": enabled, "_expected": {"mcpRemoteEnabled": expected}})
}

fn codex_config_path() -> Option<PathBuf> {
    let directory = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))?;
    std::path::absolute(directory.join("config.toml")).ok()
}

fn open_config_folder(directory: &Path) -> Result<(), String> {
    match fs::metadata(directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("Codex 配置文件夹路径指向一个文件。".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(
                "Codex 配置文件夹尚未创建。可先启动 Codex，或使用「写入本机 Codex 配置…」。".into(),
            );
        }
        Err(error) => return Err(format!("无法访问 Codex 配置文件夹：{error}")),
    }
    #[cfg(windows)]
    let opened = {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null;
        use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};

        let path: Vec<_> = directory.as_os_str().encode_wide().chain([0]).collect();
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                null(),
                path.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            ) as isize
                > 32
        }
    };
    #[cfg(not(windows))]
    let opened = url::Url::from_directory_path(directory).is_ok_and(|url| open_install_link(&url));
    opened
        .then_some(())
        .ok_or_else(|| "无法打开 Codex 配置文件夹，可复制上方路径手动打开。".into())
}

fn write_codex_config(path: &Path, snippet: &str) -> Result<(), String> {
    let path = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => {
            fs::canonicalize(path).map_err(|error| format!("无法解析 Codex 配置路径：{error}"))?
        }
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(format!("无法读取 Codex 配置：{error}")),
    };
    let read = || match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => fs::read_to_string(&path).map(Some),
        Ok(_) => Err(io::Error::other("配置路径不是普通文件")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let original = read().map_err(|error| format!("无法读取 Codex 配置：{error}"))?;
    let mut document = original
        .as_deref()
        .unwrap_or_default()
        .parse::<DocumentMut>()
        .map_err(|_| "Codex 配置不是有效的 TOML。请先修复原文件，再重试。")?;
    let mut snippet = snippet
        .parse::<DocumentMut>()
        .map_err(|_| "无法解析 Recho 配置片段。")?;
    let mut entry = snippet
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .and_then(|servers| servers.remove("recho"))
        .and_then(|entry| entry.into_table().ok())
        .ok_or("Recho 配置片段缺少 mcp_servers.recho。")?;
    let servers = document.entry("mcp_servers").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let group_position = servers.as_table().and_then(Table::position);
    let servers = servers
        .as_table_like_mut()
        .ok_or("Codex 配置中的 mcp_servers 不是表。请手动合并配置。")?;
    let previous = servers.get("recho").and_then(Item::as_table);
    // Table positions belong to the destination document.
    entry.set_position(
        previous
            .and_then(Table::position)
            .filter(|position| group_position.is_none_or(|group| *position > group)),
    );
    if let Some(previous) = previous {
        *entry.decor_mut() = previous.decor().clone();
    }
    servers.insert("recho", Item::Table(entry));
    if read().map_err(|error| format!("无法读取 Codex 配置：{error}"))? != original {
        return Err("Codex 配置刚被修改，请重新确认后再试。".into());
    }
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)
            .map_err(|error| format!("无法创建 Codex 配置目录：{error}"))?;
    }
    recho_core::native::write_local_file(&path, document.to_string().as_bytes())
        .map_err(|error| format!("无法写入 Codex 配置：{error}"))
}

struct Config {
    client: Client,
    http: bool,
    server: Value,
}

impl Config {
    fn new(
        client: Client,
        http: bool,
        host: &str,
        port: u16,
        scope: &str,
        path: Option<&str>,
        token: &str,
    ) -> Option<Self> {
        if port == 0 {
            return None;
        }
        let server = if http {
            let host = url::Host::parse(host).ok()?;
            let endpoint = format!("http://{host}:{port}/mcp?scope={scope}");
            let mut server =
                json!({"url": endpoint, "headers": {"Authorization": format!("Bearer {token}")}});
            let endpoint_key = match client {
                Client::Gemini => "httpUrl",
                Client::Windsurf => "serverUrl",
                _ => "url",
            };
            if endpoint_key != "url" {
                let endpoint = server.as_object_mut()?.remove("url")?;
                server[endpoint_key] = endpoint;
            }
            match client {
                Client::Claude
                | Client::Cursor
                | Client::VsCode
                | Client::Copilot
                | Client::CodeBuddy => server["type"] = json!("http"),
                Client::Qoder => server["type"] = json!("streamable-http"),
                _ => {}
            }
            server
        } else {
            let mut server = json!({ "command": path?, "args": ["--mcp", "--host", host, "--port", &port.to_string(), "--scope", scope], "env": {"RECHO_TOKEN": token} });
            if matches!(
                client,
                Client::Claude | Client::VsCode | Client::Copilot | Client::CodeBuddy
            ) {
                server["type"] = json!("stdio");
            }
            server
        };
        let mut config = Self {
            client,
            http,
            server,
        };
        match client {
            Client::Copilot => config.server["tools"] = json!(["*"]),
            Client::Kiro => config.server["disabled"] = json!(false),
            _ => {}
        }
        Some(config)
    }

    fn endpoint(&self) -> &str {
        label(
            &self.server[match self.client {
                Client::Gemini => "httpUrl",
                Client::Windsurf => "serverUrl",
                _ => "url",
            }],
        )
    }

    fn text(&self) -> String {
        if self.client == Client::Codex {
            if self.http {
                format!(
                    "[mcp_servers.recho]\nurl = {}\nhttp_headers = {{ Authorization = {} }}",
                    self.server["url"], self.server["headers"]["Authorization"]
                )
            } else {
                format!(
                    "[mcp_servers.recho]\ncommand = {}\nargs = {}\nenv = {{ RECHO_TOKEN = {} }}",
                    self.server["command"], self.server["args"], self.server["env"]["RECHO_TOKEN"]
                )
            }
        } else {
            let root = if self.client == Client::VsCode {
                "servers"
            } else {
                "mcpServers"
            };
            serde_json::to_string_pretty(&json!({root: {"recho": self.server}}))
                .expect("JSON values are serializable")
        }
    }

    fn command(&self) -> Option<String> {
        if self.http {
            let url = shell_quote(self.endpoint());
            let authorization = label(&self.server["headers"]["Authorization"]);
            let header = shell_quote(&format!("Authorization: {authorization}"));
            match self.client {
                Client::Claude => Some(format!(
                    "claude mcp add --transport http recho {url} --header {header}"
                )),
                Client::Gemini => Some(format!(
                    "gemini mcp add --transport http --header {header} recho {url}"
                )),
                _ => None,
            }
        } else {
            let command = shell_quote(label(&self.server["command"]));
            let args = self.server["args"]
                .as_array()?
                .iter()
                .map(|arg| shell_quote(label(arg)))
                .collect::<Vec<_>>()
                .join(" ");
            let env = shell_quote(&format!(
                "RECHO_TOKEN={}",
                label(&self.server["env"]["RECHO_TOKEN"])
            ));
            let launch = if cfg!(windows) { "& " } else { "" };
            match self.client {
                Client::Claude => Some(format!(
                    "claude mcp add --transport stdio recho --env {env} -- {command} {args}"
                )),
                Client::Gemini => Some(format!(
                    "gemini mcp add --transport stdio --env {env} recho {command} -- {args}"
                )),
                Client::Codex => Some(format!(
                    "codex mcp add recho --env {env} -- {command} {args}"
                )),
                Client::Other => {
                    let token = shell_quote(label(&self.server["env"]["RECHO_TOKEN"]));
                    let setup = if cfg!(windows) {
                        "$env:RECHO_TOKEN = "
                    } else {
                        "export RECHO_TOKEN="
                    };
                    Some(format!("{setup}{token}\n{launch}{command} {args}"))
                }
                _ => None,
            }
        }
    }

    fn install_link(&self) -> Option<url::Url> {
        match self.client {
            Client::Cursor => {
                let mut url =
                    url::Url::parse("cursor://anysphere.cursor-deeplink/mcp/install").ok()?;
                url.query_pairs_mut()
                    .append_pair("name", "recho")
                    .append_pair("config", &STANDARD.encode(self.server.to_string()));
                Some(url)
            }
            Client::VsCode => {
                let mut server = self.server.clone();
                server["name"] = json!("recho");
                let payload = url::form_urlencoded::byte_serialize(server.to_string().as_bytes())
                    .collect::<String>()
                    .replace('+', "%20");
                url::Url::parse(&format!("vscode:mcp/install?{payload}")).ok()
            }
            _ => None,
        }
    }
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

#[cfg(target_os = "macos")]
fn open_install_link(url: &url::Url) -> bool {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};
    NSURL::URLWithString(&NSString::from_str(url.as_str()))
        .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url))
}

#[cfg(target_os = "windows")]
fn open_install_link(url: &url::Url) -> bool {
    // PowerShell's call operator uses the URI as data, including any query separators.
    std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Process -FilePath $env:RECHO_MCP_INSTALL_URI",
        ])
        .env("RECHO_MCP_INSTALL_URI", url.as_str())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_install_link(url: &url::Url) -> bool {
    std::process::Command::new("xdg-open")
        .arg(url.as_str())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_share_the_authenticated_endpoint_and_use_their_config_keys() {
        let fixture = |client, http| {
            Config::new(
                client,
                http,
                "127.0.0.1",
                9988,
                "minimal",
                Some("/App 'path'/recho"),
                "fixture",
            )
            .unwrap()
        };
        for client in Client::ALL
            .into_iter()
            .filter(|client| *client != Client::Codex)
        {
            let stdio = fixture(client, false);
            let value: Value = serde_json::from_str(&stdio.text()).unwrap();
            let root = if client == Client::VsCode {
                "servers"
            } else {
                "mcpServers"
            };
            assert_eq!(
                value[root]["recho"]["args"],
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
            assert_eq!(value[root]["recho"]["env"]["RECHO_TOKEN"], "fixture");
            let http = fixture(client, true);
            let value: Value = serde_json::from_str(&http.text()).unwrap();
            let field = match client {
                Client::Gemini => "httpUrl",
                Client::Windsurf => "serverUrl",
                _ => "url",
            };
            assert_eq!(
                value[root]["recho"][field],
                "http://127.0.0.1:9988/mcp?scope=minimal"
            );
            assert_eq!(
                value[root]["recho"]["headers"]["Authorization"],
                "Bearer fixture"
            );
        }
        let codex = fixture(Client::Codex, true);
        assert!(codex.text().contains("Bearer fixture"));
        assert!(codex.command().is_none());
        let cursor = fixture(Client::Cursor, false);
        let link = cursor.install_link().unwrap();
        let payload = link
            .query_pairs()
            .find(|(key, _)| key == "config")
            .unwrap()
            .1;
        let decoded: Value =
            serde_json::from_slice(&STANDARD.decode(payload.as_bytes()).unwrap()).unwrap();
        assert_eq!(decoded, cursor.server);
        let vscode = fixture(Client::VsCode, true).install_link().unwrap();
        assert!(vscode.query().unwrap().contains("Bearer%20fixture"));
    }

    #[test]
    fn codex_http_config_creates_and_replaces_only_recho() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".codex").join("config.toml");
        let config_for_port = |port| {
            Config::new(
                Client::Codex,
                true,
                "127.0.0.1",
                port,
                "all",
                None,
                "fixture",
            )
            .unwrap()
            .text()
        };
        let snippet = config_for_port(9988);
        let snippet = snippet.as_str();

        write_codex_config(&path, snippet).unwrap();
        let initial = fs::read_to_string(&path).unwrap();
        let initial = initial.parse::<DocumentMut>().unwrap();
        assert_eq!(
            initial["mcp_servers"]["recho"]["http_headers"]["Authorization"].as_str(),
            Some("Bearer fixture")
        );

        let previous = "# Codex preferences\nmodel = 'fixture-model'\n\n[mcp_servers.recho]\ncommand = 'recho'\nargs = ['--mcp']\nenv = { RECHO_TOKEN = 'old' }\n\n[mcp_servers.other]\ncommand = 'other' # Keep this comment\n";
        fs::write(&path, previous).unwrap();
        write_codex_config(&path, snippet).unwrap();
        let updated = fs::read_to_string(&path).unwrap();
        let config = updated.parse::<DocumentMut>().unwrap();
        assert_eq!(config["model"].as_str(), Some("fixture-model"));
        assert_eq!(
            config["mcp_servers"]["other"]["command"].as_str(),
            Some("other")
        );
        assert!(updated.contains("# Codex preferences"));
        assert!(updated.contains("# Keep this comment"));
        assert!(config["mcp_servers"]["recho"].get("command").is_none());
        assert_eq!(
            config["mcp_servers"]["recho"]["url"].as_str(),
            Some("http://127.0.0.1:9988/mcp?scope=all")
        );
        write_codex_config(&path, snippet).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), updated);

        write_codex_config(&path, &config_for_port(9090)).unwrap();
        let moved = fs::read_to_string(&path).unwrap();
        let config = moved.parse::<DocumentMut>().unwrap();
        assert_eq!(
            config["mcp_servers"]["recho"]["url"].as_str(),
            Some("http://127.0.0.1:9090/mcp?scope=all")
        );
        assert_eq!(moved, updated.replace(":9988/mcp", ":9090/mcp"));
    }

    #[test]
    fn codex_http_config_keeps_recho_with_the_mcp_servers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let snippet = "[mcp_servers.recho]\nurl = 'http://127.0.0.1:9090/mcp'";
        let group = "[mcp_servers]\n\n[mcp_servers.other]\ncommand = 'other'\n\n[mcp_servers.other.env]\nVALUE = 'keep'\n\n";
        let previous =
            "# Recho comment\n[mcp_servers.recho]\nurl = 'http://127.0.0.1:9988/mcp'\n\n";
        for (layout, before) in [
            (format!("{group}{previous}"), "[mcp_servers.other.env]"),
            (format!("{previous}{group}"), "[mcp_servers]"),
            (group.to_owned(), "[mcp_servers.other.env]"),
        ] {
            fs::write(
                &path,
                format!("[model_providers.demo]\nname = 'Demo'\n\n{layout}[projects.demo]\ntrust_level = 'trusted'\n"),
            ).unwrap();
            write_codex_config(&path, snippet).unwrap();
            let updated = fs::read_to_string(&path).unwrap();
            let recho = updated.find("[mcp_servers.recho]").unwrap();
            assert!(updated.find(before).unwrap() < recho);
            assert!(recho < updated.find("[projects.demo]").unwrap());
            if layout.contains("# Recho comment") {
                assert!(updated.contains("# Recho comment\n[mcp_servers.recho]"));
            }
        }
    }

    #[test]
    fn codex_http_config_leaves_invalid_files_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let snippet = "[mcp_servers.recho]\nurl = 'http://127.0.0.1:9988/mcp'";
        for original in ["invalid = [", "mcp_servers = 42"] {
            fs::write(&path, original).unwrap();
            assert!(write_codex_config(&path, snippet).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn opening_codex_config_folder_reports_a_missing_directory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing");
        assert!(open_config_folder(&path).unwrap_err().contains("尚未创建"));
        assert!(!path.exists());
    }

    #[test]
    fn cancelling_codex_confirmation_does_not_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut ui = McpUi {
            page: Some(Page::Config(Client::Codex)),
            confirm_codex: Some((path.clone(), String::new())),
            ..Default::default()
        };
        ui.back();
        assert!(ui.confirm_codex.is_none());
        assert!(matches!(ui.page, Some(Page::Config(Client::Codex))));
        assert!(!path.exists());
    }

    #[test]
    fn selected_token_fills_the_stdin_and_http_configuration() {
        let tokens = [
            json!({"id": "first", "token": "first-fixture"}),
            json!({"id": "second", "token": "second-fixture"}),
        ];
        let mut ui = McpUi::default();
        assert_eq!(ui.selected_token(&tokens), None);

        for (id, credential) in [("first", "first-fixture"), ("second", "second-fixture")] {
            ui.selected = id.into();
            let token = ui.selected_token(&tokens).unwrap();
            assert_eq!(token, credential);
            for http in [false, true] {
                let config = Config::new(
                    Client::Codex,
                    http,
                    "127.0.0.1",
                    9988,
                    "minimal",
                    Some("/App/recho"),
                    token,
                )
                .unwrap();
                assert!(config.text().contains(credential));
                if !http {
                    assert!(config.command().unwrap().contains(credential));
                }
            }
        }
        assert_eq!(ui.selected_token(&tokens[..1]), None);
    }

    #[test]
    fn token_mask_only_shows_prefix_and_suffix() {
        assert_eq!(mask_token("1234567890abcdef"), "1234••••••••cdef");
        assert_eq!(mask_token("secret"), "••••••••");
    }
}
