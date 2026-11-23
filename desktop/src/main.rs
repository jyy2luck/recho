#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod body;
mod capture_spaces;
mod composer;
mod files;
mod library_ui;
mod mcp_ui;
#[cfg(target_os = "macos")]
mod native_menu;
mod network_ui;
mod platform;
mod rules_ui;
mod theme;
mod workbench;
mod worker;

use body::{Content, hex_row};
use eframe::egui::{self, RichText};
use recho_core::native::{Detail, Phase, Row};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use worker::{Command, DetailHandle, HistoryHandle, RequestHandle, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Navigation {
    Capture,
    Collections,
    Environments,
    History,
    Tools,
}

struct Viewer {
    detail: Detail,
    content: Content,
    structured: Option<Result<serde_json::Value, String>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BodyFormat {
    Raw,
    Hex,
    Json,
    Tree,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTab {
    Overview,
    Raw,
    Headers,
    Body,
}
struct MessagePanel {
    tab: MessageTab,
    format: BodyFormat,
    viewer: Option<Viewer>,
}
impl MessagePanel {
    fn new(tab: MessageTab) -> Self {
        Self {
            tab,
            format: BodyFormat::Raw,
            viewer: None,
        }
    }
}

struct Recho {
    worker: Worker,
    theme: theme::Theme,
    theme_path: PathBuf,
    composer: composer::Composer,
    rules: rules_ui::RulesUi,
    rules_open: bool,
    library: library_ui::LibraryUi,
    network: network_ui::NetworkUi,
    network_open: bool,
    mcp: mcp_ui::McpUi,
    mcp_open: bool,
    workspace_ops: Vec<Value>,
    active_workspace_op: Option<(String, Result<Value, String>, Option<RequestHandle>)>,
    spaces: capture_spaces::Spaces,
    capture_filters: Vec<String>,
    condition_editor: Option<(bool, recho_core::native::ConditionSet)>,
    live_rows: Vec<Row>,
    har_file: Option<(files::FilePick, Option<Vec<String>>)>,
    notice: Option<String>,
    certificates: platform::Certificates,
    system_proxy: (bool, bool),
    proxy_preferences: platform::ProxyPreferences,
    proxy_preferences_saving: bool,
    confirm_proxy_auto: bool,
    navigation: Navigation,
    history_rows: Vec<Row>,
    history_query: String,
    history_offset: usize,
    history_load: Option<HistoryHandle>,
    history_loaded: bool,
    curl_copy: Option<(Row, DetailHandle)>,
    closing: bool,
    closing_proxy: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    discard_on_close: bool,
    ready: bool,
    ca_path: PathBuf,
    running: bool,
    intercept_tls: bool,
    port: u16,
    port_editor: String,
    rows: Vec<Row>,
    visible: Vec<usize>,
    sequence: BTreeMap<String, usize>,
    next_sequence: usize,
    filter: String,
    host_filter: String,
    app_filter: String,
    protocol_filter: String,
    status_filter: u16,
    type_filter: String,
    sort_column: usize,
    ascending: bool,
    selected: Option<String>,
    selection: BTreeSet<String>,
    selection_anchor: Option<String>,
    generation: u64,
    messages: [MessagePanel; 2],
    details_open: bool,
    sidebar_visible: bool,
    connection_open: bool,
    clear_confirmation: bool,
    error: Option<String>,
    history_error: Option<String>,
}
impl Recho {
    fn new(cc: &eframe::CreationContext<'_>, directory: PathBuf, port: Option<u16>) -> Self {
        let theme_path = directory.join("desktop-theme.json");
        let (theme, theme_error) = match theme::Theme::load(&theme_path) {
            Ok(theme) => (theme, None),
            Err(error) => (theme::Theme::default(), Some(error)),
        };
        theme.apply(&cc.egui_ctx);
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".into(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../assets/fonts/DroidSansFallback.ttf"
            ))),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".into());
        }
        fonts.font_data.insert(
            "icons".into(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../assets/fonts/MaterialIcons-Regular.ttf"
            ))),
        );
        fonts
            .families
            .insert(egui::FontFamily::Name("icons".into()), vec!["icons".into()]);
        cc.egui_ctx.set_fonts(fonts);
        Self {
            theme,
            theme_path,
            spaces: capture_spaces::Spaces::new(directory.join("desktop-captures.json")),
            worker: Worker::start(directory, port, cc.egui_ctx.clone()),
            live_rows: Vec::new(),
            capture_filters: Vec::new(),
            condition_editor: None,
            har_file: None,
            notice: None,
            network: network_ui::NetworkUi::default(),
            network_open: false,
            mcp: mcp_ui::McpUi::default(),
            mcp_open: false,
            workspace_ops: Vec::new(),
            active_workspace_op: None,
            composer: composer::Composer::default(),
            rules: rules_ui::RulesUi::default(),
            rules_open: false,
            library: library_ui::LibraryUi::default(),
            certificates: platform::Certificates::default(),
            system_proxy: (false, false),
            proxy_preferences: platform::ProxyPreferences::default(),
            proxy_preferences_saving: false,
            confirm_proxy_auto: false,
            navigation: Navigation::Capture,
            history_rows: Vec::new(),
            history_query: String::new(),
            history_offset: 0,
            history_load: None,
            history_loaded: false,
            curl_copy: None,
            closing: false,
            closing_proxy: None,
            discard_on_close: false,
            ready: false,
            ca_path: PathBuf::new(),
            running: false,
            intercept_tls: false,
            port: 0,
            port_editor: String::new(),
            rows: Vec::new(),
            visible: Vec::new(),
            sequence: BTreeMap::new(),
            next_sequence: 1,
            filter: String::new(),
            host_filter: String::new(),
            app_filter: String::new(),
            protocol_filter: String::new(),
            status_filter: 0,
            type_filter: String::new(),
            sort_column: 0,
            ascending: true,
            selected: None,
            selection: BTreeSet::new(),
            selection_anchor: None,
            generation: 0,
            messages: [
                MessagePanel::new(MessageTab::Overview),
                MessagePanel::new(MessageTab::Body),
            ],
            details_open: false,
            sidebar_visible: true,
            connection_open: false,
            clear_confirmation: false,
            error: theme_error,
            history_error: None,
        }
    }

    fn set_theme(&mut self, next: theme::Theme, ctx: &egui::Context) {
        self.theme = next;
        next.apply(ctx);
        if let Err(error) = next.save(&self.theme_path) {
            self.error = Some(error);
        }
    }

    fn send(&mut self, command: Command) {
        if let Err(error) = self.worker.send(command) {
            self.error = Some(error);
        }
    }
    fn set_proxy_preferences(&mut self, preferences: platform::ProxyPreferences) {
        if self.proxy_preferences_saving {
            return;
        }
        match self
            .worker
            .send(Command::SystemProxyPreferences(preferences))
        {
            Ok(()) => self.proxy_preferences_saving = true,
            Err(error) => self.error = Some(error),
        }
    }
    fn toggle_proxy_auto(&mut self) {
        if self.proxy_preferences.auto {
            self.set_proxy_preferences(platform::ProxyPreferences {
                auto: false,
                ..self.proxy_preferences
            });
        } else {
            self.confirm_proxy_auto = true;
        }
    }
    fn toggle_capture(&mut self) {
        self.send(Command::Configure {
            running: !self.running,
            intercept_tls: self.intercept_tls,
        });
    }
    fn select(&mut self, id: Option<String>) {
        self.selection = id.iter().cloned().collect();
        self.selection_anchor = id.clone();
        self.select_detail(id);
    }
    fn select_detail(&mut self, id: Option<String>) {
        self.selected = id;
        self.generation += 1;
        for message in &mut self.messages {
            message.viewer = None;
        }
        if self.details_open {
            self.load_details();
        }
    }
    fn select_row(&mut self, id: String, modifiers: egui::Modifiers) {
        if self.navigation == Navigation::History || (!modifiers.command && !modifiers.shift) {
            self.select(Some(id));
            return;
        }
        if modifiers.shift {
            let position = |id: &str| self.visible.iter().position(|&i| self.rows[i].id == id);
            if let (Some(anchor), Some(end)) = (
                self.selection_anchor.as_deref().and_then(position),
                position(&id),
            ) {
                if !modifiers.command {
                    self.selection.clear();
                }
                self.selection.extend(
                    self.visible[anchor.min(end)..=anchor.max(end)]
                        .iter()
                        .map(|&i| self.rows[i].id.clone()),
                );
            } else {
                self.selection.insert(id.clone());
                self.selection_anchor = Some(id.clone());
            }
        } else {
            self.selection_anchor = Some(id.clone());
            if !self.selection.remove(&id) {
                self.selection.insert(id.clone());
            }
        }
        let detail = if self.selection.contains(&id) {
            Some(id)
        } else {
            self.selection.iter().next().cloned()
        };
        self.select_detail(detail);
    }
    fn active_rows(&self) -> &[Row] {
        if self.navigation == Navigation::History {
            &self.history_rows
        } else {
            &self.rows
        }
    }
    fn navigate(&mut self, next: Navigation) {
        self.library.close();
        let was_history = self.navigation == Navigation::History;
        self.remember_capture();
        if was_history != (next == Navigation::History) {
            self.select(None);
        }
        self.navigation = next;
        if was_history && next != Navigation::History {
            if let Some(space) = self.spaces.current() {
                self.selection = space.selection.clone();
                self.details_open = space.details_open;
                self.select_detail(space.detail.clone());
                self.rebuild_capture_rows();
            }
        }
        self.sidebar_visible = true;
        if next == Navigation::History && self.ready {
            self.composer.show_capture();
            self.load_history(self.history_offset);
        }
    }
    fn set_incognito(&mut self, enabled: bool) {
        match self.worker.request(
            "PATCH",
            "/settings",
            serde_json::json!({
                "incognito": enabled, "_expected": {"incognito": self.spaces.incognito}
            }),
        ) {
            Ok(reply) => self.spaces.settings = Some(reply),
            Err(error) => self.error = Some(error),
        }
    }
    fn open_network(&mut self, section: network_ui::NetworkSection) {
        self.navigate(Navigation::Tools);
        self.rules_open = false;
        self.mcp_open = false;
        self.network_open = true;
        self.network.open(section);
    }
    fn load_history(&mut self, offset: usize) {
        match self
            .worker
            .history(offset, 100, self.history_query.trim().into(), None)
        {
            Ok(request) => {
                self.history_offset = offset;
                self.history_load = Some(request);
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn choose_har(&mut self, ctx: &egui::Context, export: bool) {
        if self.har_file.is_some() {
            return;
        }
        let selection = export.then(|| {
            self.active_rows()
                .iter()
                .filter(|row| self.selection.contains(&row.id))
                .map(|row| row.id.clone())
                .collect::<Vec<_>>()
        });
        if selection.as_ref().is_some_and(Vec::is_empty) {
            self.error = Some("请先选择要导出的记录".into());
            return;
        }
        self.har_file = Some((files::pick("har", export, ctx), selection));
    }
    fn poll_har(&mut self) {
        if let Some((reply, ids)) = &mut self.har_file {
            let result = match reply.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(Some(path)) => {
                    let command = match ids.take() {
                        Some(ids) => Command::ExportHar(path, ids),
                        None => Command::ImportHar(path),
                    };
                    self.send(command);
                }
                Ok(None) => {}
                Err(error) => self.error = Some(error),
            }
            self.har_file = None;
        }
    }
    fn edit_capture(&mut self, id: &str) {
        let rows = if self.navigation == Navigation::History {
            &self.history_rows
        } else {
            &self.rows
        };
        if let Some(row) = rows.iter().find(|row| row.id == id) {
            let viewer = self.messages[0]
                .viewer
                .as_ref()
                .filter(|viewer| viewer.detail.id == id);
            self.composer.open_capture(
                &self.worker,
                row,
                viewer.map(|viewer| &viewer.detail),
                viewer.map(|viewer| viewer.content.bytes()),
            );
        }
    }
    fn copy_curl(&mut self, id: String) {
        if let Some(row) = self.active_rows().iter().find(|row| row.id == id) {
            let row = row.clone();
            match self.worker.detail(id, Phase::Request) {
                Ok(reply) => self.curl_copy = Some((row, reply)),
                Err(error) => self.error = Some(error),
            }
        }
    }
    fn poll_curl(&mut self, ctx: &egui::Context) {
        if let Some((row, reply)) = self.curl_copy.as_mut() {
            let result = match reply.try_recv() {
                Ok(Ok(detail)) => composer::capture_curl(row, &detail, &detail.bytes),
                Ok(Err(error)) => Err(error),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            match result {
                Ok(curl) => ctx.copy_text(curl),
                Err(error) => self.error = Some(error),
            }
            self.curl_copy = None;
        }
    }
    fn poll_workspace_operations(&mut self) {
        if self.active_workspace_op.is_none() {
            let Some(operation) = self.workspace_ops.first() else {
                return;
            };
            let Some(id) = operation["id"].as_str() else {
                return;
            };
            if !self.composer.flush_pending(&self.worker) && self.composer.save_error().is_none() {
                return;
            }
            let result = match self.composer.save_error() {
                Some(error) => Err(error.into()),
                None => self.composer.workspace_operation_once(
                    id,
                    operation["name"].as_str().unwrap_or_default(),
                    &operation["arguments"],
                ),
            };
            self.active_workspace_op = Some((id.to_owned(), result, None));
        }
        let Some((id, result, reply)) = &mut self.active_workspace_op else {
            return;
        };
        if let Some(handle) = reply {
            if matches!(
                handle.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ) {
                return;
            }
            self.workspace_ops
                .retain(|operation| operation["id"] != id.as_str());
            self.active_workspace_op = None;
            return;
        }
        if !self
            .workspace_ops
            .iter()
            .any(|operation| operation["id"] == id.as_str())
        {
            self.active_workspace_op = None;
            return;
        }
        let result = if let Err(error) = result {
            json!({"error": error})
        } else if self.composer.flush_pending(&self.worker) {
            json!({"result": result.as_ref().ok()})
        } else if let Some(error) = self.composer.save_error() {
            json!({"error": error})
        } else {
            return;
        };
        match self
            .worker
            .request("POST", format!("/ui-operations/{id}"), result)
        {
            Ok(handle) => *reply = Some(handle),
            Err(_) => {}
        }
    }
    fn load_details(&mut self) {
        if let Some(id) = self.selected.clone() {
            for phase in [Phase::Request, Phase::Response] {
                self.send(Command::Detail {
                    generation: self.generation,
                    id: id.clone(),
                    phase,
                });
            }
        }
    }
    fn refilter(&mut self) {
        let query = self.filter.trim().to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let query_match = query.is_empty()
                    || row.url.to_lowercase().contains(&query)
                    || row.method.to_lowercase().contains(&query)
                    || row.status.to_string().contains(&query);
                let protocol_match = self.protocol_filter.is_empty()
                    || match self.protocol_filter.as_str() {
                        "HTTPS" => row.url.starts_with("https:"),
                        "HTTP" => row.url.starts_with("http:"),
                        "HTTP1" => row.protocol.contains('1'),
                        "HTTP2" => row.protocol.contains('2'),
                        "HTTP3" => row.protocol.contains('3'),
                        _ => false,
                    };
                let mut applicable = self
                    .capture_filters
                    .iter()
                    .filter_map(|id| row.list_matches.get(id))
                    .peekable();
                let automation_match =
                    applicable.peek().is_none() || applicable.any(|value| *value);
                (query_match
                    && automation_match
                    && self
                        .spaces
                        .matches
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&row.id))
                    && protocol_match
                    && (self.host_filter.is_empty() || row.host == self.host_filter)
                    && (self.app_filter.is_empty() || row.application == self.app_filter)
                    && (self.status_filter == 0 || row.status / 100 == self.status_filter)
                    && (self.type_filter.is_empty()
                        || row.content_type.contains(&self.type_filter)))
                .then_some(index)
            })
            .collect();
        let rows = &self.rows;
        let numbers = &self.sequence;
        let column = self.sort_column;
        self.visible.sort_by(|&a, &b| {
            let a = &rows[a];
            let b = &rows[b];
            let order = match column {
                2 => a.method.cmp(&b.method),
                3 => a.url.cmp(&b.url),
                4 => a.application.cmp(&b.application),
                5 => a.status.cmp(&b.status),
                6 => a.server_ip.cmp(&b.server_ip),
                7 => a.duration_ms.cmp(&b.duration_ms),
                8 => a.response_bytes.cmp(&b.response_bytes),
                _ => numbers.get(&a.id).cmp(&numbers.get(&b.id)),
            };
            if self.ascending {
                order
            } else {
                order.reverse()
            }
        });
    }
    fn receive(&mut self) {
        let inbox = std::mem::take(&mut *self.worker.inbox.lock().unwrap());
        if let Some(ready) = inbox.ready {
            match ready {
                Ok(path) => {
                    self.ca_path = path;
                    self.ready = true;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(snapshot) = inbox.snapshot {
            self.running = snapshot.running;
            self.intercept_tls = snapshot.intercept_tls;
            if self.port != snapshot.port {
                self.port = snapshot.port;
                self.port_editor = self.port.to_string();
            }
            self.history_error = snapshot.history_error;
            self.spaces.incognito = snapshot.incognito;
            self.open_capture_space(snapshot.group);
            if self.capture_filters != snapshot.enabled_list_filters {
                self.capture_filters = snapshot.enabled_list_filters;
                self.refilter();
            }
            if let Some(rows) = snapshot.rows {
                self.live_rows = rows;
                self.rebuild_capture_rows();
                if self.messages.iter().any(|panel| {
                    panel
                        .viewer
                        .as_ref()
                        .is_some_and(|viewer| viewer.detail.notice.is_some())
                }) {
                    self.select_detail(self.selected.clone());
                }
            }
        }
        for (index, result) in inbox.details.into_iter().enumerate() {
            if let Some((generation, result)) = result {
                if generation == self.generation {
                    match result {
                        Ok(mut detail) => {
                            let content = Content::new(std::mem::take(&mut detail.bytes));
                            let panel = &mut self.messages[index];
                            if matches!(content, Content::Binary(_)) {
                                panel.format = BodyFormat::Hex;
                            }
                            panel.viewer = Some(Viewer {
                                detail,
                                content,
                                structured: None,
                            });
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
        }
        if let Some(error) = inbox.error {
            self.error = Some(error);
        }
        if let Some(pending) = inbox.breakpoints {
            self.rules.update_pending(pending);
        }
        if let Some(mut pending) = inbox.workspace_operations {
            self.workspace_ops = pending["operations"]
                .take()
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        if let Some(status) = inbox.system_proxy {
            self.system_proxy = status;
        }
        if let Some(preferences) = inbox.proxy_preferences {
            self.proxy_preferences = preferences;
            self.proxy_preferences_saving = false;
        }
        if let Some(notice) = inbox.notice {
            self.notice = Some(notice);
        }
        if let Some((group, name, count)) = inbox.imported {
            self.spaces.items.insert(
                group.clone(),
                capture_spaces::Space {
                    name,
                    ..Default::default()
                },
            );
            self.navigate(Navigation::Capture);
            self.composer.show_capture();
            self.activate_capture(group);
            self.notice = Some(format!("已导入 {count} 条 HAR 记录"));
        }
        if let Some(request) = &mut self.history_load {
            match request.try_recv() {
                Ok(Ok(rows)) => {
                    if self.navigation == Navigation::History
                        && self
                            .selected
                            .as_ref()
                            .is_some_and(|id| !rows.iter().any(|row| &row.id == id))
                    {
                        self.select(None);
                    }
                    self.history_rows = rows;
                    self.history_loaded = true;
                    self.history_load = None;
                }
                Ok(Err(error)) => {
                    self.error = Some(error);
                    self.history_load = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.history_load = None;
                }
            }
        }
    }
}
impl eframe::App for Recho {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.receive();
        self.rules.poll();
        self.library.poll(&self.worker);
        self.network.poll();
        self.mcp.poll();
        self.poll_capture_spaces();
        self.remember_capture();
        self.poll_curl(ctx);
        self.poll_har();
        self.composer.poll(&self.worker, self.ready, ctx);
        self.poll_workspace_operations();
        let close = ctx.input(|input| input.viewport().close_requested());
        self.spaces.persist(ctx, close);
        if close && self.system_proxy.1 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.closing_proxy.is_none() {
                match self.worker.restore_system_proxy() {
                    Ok(request) => self.closing_proxy = Some(request),
                    Err(error) => self.error = Some(error),
                }
            }
            return;
        }
        if let Some(request) = &mut self.closing_proxy {
            let result = match request.try_recv() {
                Ok(result) => result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(error) => Err(error.to_string()),
            };
            self.closing_proxy = None;
            match result {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) => {
                    self.error = Some(format!(
                        "退出前恢复系统代理失败。请在连接设置中恢复后重试。{error}"
                    ));
                    self.connection_open = true;
                }
            }
            return;
        }
        if close && !self.discard_on_close && !self.composer.flush_pending(&self.worker) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        } else if self.closing && self.composer.flush_pending(&self.worker) {
            self.closing = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        workbench::show(self, ui);
        if self.closing {
            let message = self.composer.save_error().map(str::to_owned);
            if let Some(message) = message {
                egui::Window::new("工作区保存失败")
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(message);
                        if ui.button("返回工作区").clicked() {
                            self.closing = false;
                        }
                        if ui.button("退出并放弃未保存的更改").clicked() {
                            self.closing = false;
                            self.discard_on_close = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
            }
        }
    }
}
fn options() -> Result<(PathBuf, Option<u16>), String> {
    let directory = if let Some(directory) = std::env::var_os("RECHO_DATA_DIR") {
        PathBuf::from(directory)
    } else {
        let base = std::env::var_os(if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        })
        .ok_or("Set RECHO_DATA_DIR to an absolute user data directory")?;
        PathBuf::from(base).join(if cfg!(target_os = "macos") {
            "Library/Application Support/recho"
        } else {
            "recho"
        })
    };
    if !directory.is_absolute() {
        return Err("RECHO_DATA_DIR must be absolute".into());
    }
    let port = std::env::var("RECHO_PROXY_PORT")
        .ok()
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| "RECHO_PROXY_PORT must be 0–65535".to_string())
        })
        .transpose()?;
    Ok((directory, port))
}
fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--mcp") {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())
            .and_then(|runtime| runtime.block_on(recho_core::run(true)));
        if let Err(error) = result {
            eprintln!("MCP: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let (directory, port) = options()
        .map_err(|error| eframe::Error::AppCreation(Box::new(std::io::Error::other(error))))?;
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("recho")
            .with_inner_size([1450.0, 900.0])
            .with_min_inner_size([1050.0, 600.0])
            .with_app_id("io.github.jyy2luck.recho")
            .with_icon(egui::IconData::default()),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "recho",
        native,
        Box::new(move |cc| Ok(Box::new(Recho::new(cc, directory, port)))),
    )
}
