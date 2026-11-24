use crate::rules_ui::{finished, label};
use crate::theme::Palette;
use crate::worker::{PortChange, PortHandle, RequestHandle, Worker};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use serde_json::{Value, json};

pub(super) struct PortDialog {
    port: String,
    pending: Option<(PortChange, PortHandle)>,
    error: Option<String>,
}

impl PortDialog {
    pub(super) fn new(port: u16) -> Self {
        Self {
            port: port.to_string(),
            pending: None,
            error: None,
        }
    }

    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        worker: &Worker,
        colors: Palette,
        current_port: u16,
    ) -> bool {
        if let Some((change, result)) = self
            .pending
            .as_mut()
            .and_then(|(change, reply)| finished(reply).map(|result| (*change, result)))
        {
            self.pending = None;
            match result {
                Ok(_) if matches!(change, PortChange::Apply(_)) => return true,
                Ok(port) => self.port = port.to_string(),
                Err(error) => self.error = Some(error),
            }
        }

        let width = (ctx.content_rect().width() - 60.0).clamp(320.0, 460.0);
        let enabled = self.pending.is_none();
        let mut change = None;
        let frame = egui::Frame::window(&ctx.global_style())
            .fill(colors.side)
            .inner_margin(egui::Margin::same(16));
        let modal = egui::Modal::new(egui::Id::new("proxy_port_dialog"))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.heading("修改端口号");
                });
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(colors.bg)
                    .stroke(egui::Stroke::new(1.0, colors.line))
                    .corner_radius(4)
                    .inner_margin(egui::Margin::symmetric(12, 18))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.visuals_mut().widgets.inactive.bg_stroke =
                                egui::Stroke::new(1.0, colors.line);
                            ui.horizontal(|ui| {
                                ui.add_space(((ui.available_width() - 120.0) * 0.5).max(0.0));
                                let input = ui.add(
                                    egui::TextEdit::singleline(&mut self.port)
                                        .font(egui::FontId::proportional(26.0))
                                        .horizontal_align(egui::Align::Center)
                                        .vertical_align(egui::Align::Center)
                                        .desired_width(120.0)
                                        .min_size(egui::vec2(120.0, 48.0))
                                        .hint_text("端口"),
                                );
                                if input.changed() {
                                    self.error = None;
                                }
                                let refresh = ui.add(
                                    egui::Button::new(crate::workbench::icon("\u{e5d5}"))
                                        .min_size(egui::vec2(32.0, 32.0))
                                        .frame_when_inactive(false),
                                );
                                refresh.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        enabled,
                                        "换一个可用端口",
                                    )
                                });
                                if refresh.on_hover_text("换一个可用端口").clicked() {
                                    change = Some(PortChange::Suggest(
                                        self.port.trim().parse().unwrap_or(current_port),
                                    ));
                                }
                            });
                            ui.add_space(16.0);
                            ui.columns(6, |columns| {
                                for (ui, port) in columns
                                    .iter_mut()
                                    .zip(["8000", "8080", "8888", "9000", "9090", "9999"])
                                {
                                    ui.vertical_centered(|ui| {
                                        if ui
                                            .add(
                                                egui::Button::selectable(self.port == port, port)
                                                    .min_size(egui::vec2(43.0, 26.0)),
                                            )
                                            .clicked()
                                        {
                                            self.port = port.into();
                                            self.error = None;
                                        }
                                    });
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(egui::RichText::new("完成").color(colors.accent))
                                .min_size(egui::vec2(60.0, 30.0))
                                .frame_when_inactive(false),
                        )
                        .clicked()
                    {
                        match self.port.trim().parse::<u16>() {
                            Ok(port) => change = Some(PortChange::Apply(port)),
                            Err(_) => self.error = Some("端口必须是 0–65535".into()),
                        }
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if let Some(error) = &self.error {
                            ui.add(
                                egui::Label::new(egui::RichText::new(error).color(colors.red))
                                    .truncate(),
                            )
                            .on_hover_text(error);
                        } else if !enabled {
                            ui.spinner();
                        }
                    });
                });
            });
        if let Some(change) = change {
            self.error = None;
            match worker.edit_port(change) {
                Ok(reply) => self.pending = Some((change, reply)),
                Err(error) => self.error = Some(error),
            }
        }
        self.pending.is_none() && modal.should_close()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum NetworkSection {
    #[default]
    Ssl,
    Access,
    Upstream,
    Gateway,
    Mirror,
    Reverse,
}

impl NetworkSection {
    const SECTIONS: [Self; 6] = [
        Self::Ssl,
        Self::Access,
        Self::Upstream,
        Self::Gateway,
        Self::Mirror,
        Self::Reverse,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Ssl => "SSL 配置",
            Self::Access => "访问控制",
            Self::Upstream => "二级代理",
            Self::Gateway => "网关",
            Self::Mirror => "镜像",
            Self::Reverse => "反向代理",
        }
    }

    fn list(self) -> &'static str {
        match self {
            Self::Ssl => "sslProfiles",
            Self::Access => "accessProfiles",
            Self::Upstream => "upstreamProfiles",
            Self::Gateway => "gateways",
            Self::Mirror => "mirrors",
            Self::Reverse => "reverseProxies",
        }
    }

    fn active(self) -> Option<&'static str> {
        match self {
            Self::Ssl => Some("activeSSLProfile"),
            Self::Access => Some("activeAccessProfile"),
            Self::Upstream => Some("activeUpstreamProfile"),
            _ => None,
        }
    }

    fn enabled(self) -> &'static str {
        match self {
            Self::Ssl => "interceptTLS",
            Self::Access => "accessEnabled",
            Self::Upstream => "upstreamEnabled",
            Self::Gateway => "gatewayEnabled",
            Self::Mirror => "mirrorEnabled",
            Self::Reverse => "reverseEnabled",
        }
    }

    fn default_item(self) -> Value {
        let mut entry = match self {
            Self::Ssl => json!({"mode": "intercept", "patterns": ["*"], "hideBypassed": false}),
            Self::Access => json!({"mode": "allow", "patterns": ["127.0.0.1"]}),
            Self::Upstream => {
                json!({"host": "127.0.0.1", "port": 8080, "mode": "bypass", "patterns": []})
            }
            Self::Gateway => {
                json!({"enabled": true, "layer": "l7", "mode": "allow", "patterns": [], "showBlocked": true, "matchers": []})
            }
            Self::Mirror => {
                json!({"enabled": true, "host": "", "target": "127.0.0.1:8080", "hostPolicy": "original", "sniPolicy": "original", "cnPolicy": "original"})
            }
            Self::Reverse => {
                json!({"enabled": true, "listenHost": "127.0.0.1", "port": 8081, "target": "http://127.0.0.1:8080", "preserveHost": false})
            }
        };
        entry["id"] = json!(uuid::Uuid::new_v4().to_string());
        entry["name"] = json!(format!("新建{}", self.label()));
        entry
    }

    fn summary(self, entry: &Value) -> String {
        match self {
            Self::Upstream => format!("{}:{}", label(&entry["host"]), entry["port"]),
            Self::Mirror => format!("{} → {}", label(&entry["host"]), label(&entry["target"])),
            Self::Reverse => format!(
                "{}:{} → {}",
                label(&entry["listenHost"]),
                entry["port"],
                label(&entry["target"])
            ),
            _ => entry["patterns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(
                    entry["matchers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|m| label(&m["pattern"])),
                )
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

#[derive(Default)]
pub(super) struct NetworkUi {
    section: NetworkSection,
    settings: Option<Value>,
    reply: Option<(Reply, RequestHandle)>,
    selected: String,
    search: String,
    edit: Option<Edit>,
    delete: Option<Value>,
    error: Option<String>,
    source_error: Option<String>,
}

enum Reply {
    Load,
    Save,
    Edit,
    Proxy(bool),
}

impl NetworkUi {
    pub(super) fn open(&mut self, section: NetworkSection) {
        if self.section != section {
            self.section = section;
            self.selected.clear();
        }
        self.settings = None;
        self.error = None;
    }

    pub(super) fn poll(&mut self) {
        let Some((kind, result)) = self.reply.as_mut().and_then(|(kind, request)| {
            finished(request).map(|result| (std::mem::replace(kind, Reply::Load), result))
        }) else {
            return;
        };
        self.reply = None;
        match result {
            Ok(settings) => {
                self.error = None;
                if let Reply::Proxy(enabled) = kind {
                    if let Some(settings) = &mut self.settings {
                        settings["interceptTLS"] = json!(enabled);
                    }
                } else {
                    self.source_error = settings["error"].as_str().map(str::to_owned);
                    self.settings = Some(settings);
                }
                if matches!(kind, Reply::Edit) {
                    self.edit = None;
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn request(&mut self, worker: &Worker, method: &str, path: &str, body: Value, kind: Reply) {
        match worker.request(method, path, body) {
            Ok(reply) => self.reply = Some((kind, reply)),
            Err(error) => self.error = Some(error),
        }
    }

    fn refresh(&mut self, worker: &Worker) {
        if self.reply.is_none() {
            self.request(worker, "GET", "/settings", Value::Null, Reply::Load);
        }
    }

    fn patch(&mut self, worker: &Worker, mut patch: Value, kind: Reply) {
        let Some(settings) = &self.settings else {
            return;
        };
        let expected: serde_json::Map<_, _> = patch
            .as_object()
            .unwrap()
            .keys()
            .map(|key| (key.clone(), settings[key].clone()))
            .collect();
        patch["_expected"] = Value::Object(expected);
        self.request(worker, "PATCH", "/settings", patch, kind);
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        if self.settings.is_none() && self.reply.is_none() && self.error.is_none() {
            self.refresh(worker);
        }
        ui.horizontal_wrapped(|ui| {
            for section in NetworkSection::SECTIONS {
                if ui
                    .selectable_label(self.section == section, section.label())
                    .clicked()
                    && self.section != section
                {
                    self.open(section);
                }
            }
            if ui.button("刷新配置").clicked() {
                self.refresh(worker);
            }
        });
        for error in [&self.source_error, &self.error].into_iter().flatten() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let Some(settings) = &self.settings else {
            if self.reply.is_some() {
                ui.spinner();
            }
            return;
        };
        let section = self.section;
        let key = section.list();
        let active = section.active();
        let active_id = active.map_or("", |key| label(&settings[key])).to_owned();
        let available = self.reply.is_none() && self.source_error.is_none();
        let enable_key = section.enabled();
        let mut enabled = settings[enable_key] == true;
        let entries = settings[key].as_array().map_or(&[][..], Vec::as_slice);
        let mut patch = None;
        let mut proxy_toggle = false;
        ui.horizontal_wrapped(|ui| {
            let caption = if section == NetworkSection::Ssl {
                "SSL 代理".into()
            } else {
                format!("启用{}", section.label())
            };
            if ui
                .add_enabled(available, egui::Checkbox::new(&mut enabled, caption))
                .changed()
            {
                if section == NetworkSection::Ssl {
                    proxy_toggle = true;
                } else {
                    patch = Some(json!({(enable_key): enabled}));
                }
            }
            if let Some(active) = active {
                let name = entries
                    .iter()
                    .find(|entry| entry["id"] == active_id)
                    .map_or("默认配置", |entry| label(&entry["name"]));
                ui.add_enabled_ui(available, |ui| {
                    egui::ComboBox::from_label("当前配置")
                        .selected_text(name)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(active_id.is_empty(), "默认配置")
                                .clicked()
                            {
                                patch = Some(json!({(active): null}));
                            }
                            for entry in entries {
                                let id = label(&entry["id"]);
                                if ui
                                    .selectable_label(active_id == id, label(&entry["name"]))
                                    .clicked()
                                {
                                    patch = Some(json!({(active): id}));
                                }
                            }
                        });
                });
            }
        });
        ui.separator();
        let mut row_action = None;
        egui::Panel::bottom("network_actions").show(ui, |ui| {
            ui.add_enabled_ui(available, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("新建配置").clicked() {
                        self.error = None;
                        self.edit = Some(Edit::new(section, section.default_item(), None));
                    }
                    let selected = entries
                        .iter()
                        .position(|entry| entry["id"] == self.selected);
                    ui.add_enabled_ui(selected.is_some(), |ui| {
                        for (title, action) in [
                            ("编辑", Action::Edit),
                            ("复制", Action::Copy),
                            ("上移", Action::Up),
                            ("下移", Action::Down),
                            ("删除", Action::Delete),
                        ] {
                            if ui.button(title).clicked() {
                                row_action = selected.map(|index| (index, action));
                            }
                        }
                    });
                });
            });
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("搜索配置名称或地址"));
        let query = self.search.to_lowercase();
        let visible = entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let summary = section.summary(entry);
                (label(&entry["name"]).to_lowercase().contains(&query)
                    || summary.to_lowercase().contains(&query))
                .then_some((i, summary))
            })
            .collect::<Vec<_>>();
        TableBuilder::new(ui)
            .id_salt("network_profiles")
            .striped(true)
            .sense(egui::Sense::click())
            .column(Column::exact(50.0))
            .column(Column::initial(180.0).clip(true))
            .column(Column::remainder().clip(true))
            .column(Column::initial(105.0).clip(true))
            .header(25.0, |mut row| {
                for title in [
                    if active.is_some() { "当前" } else { "启用" },
                    "名称",
                    "地址或匹配条件",
                    "模式",
                ] {
                    row.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(25.0, visible.len(), |mut row| {
                    let (index, summary) = &visible[row.index()];
                    let entry = &entries[*index];
                    let id = label(&entry["id"]);
                    row.set_selected(self.selected == id);
                    row.col(|ui| {
                        if let Some(active) = active {
                            if ui
                                .add_enabled(available, egui::RadioButton::new(active_id == id, ""))
                                .clicked()
                            {
                                patch = Some(json!({(active): id}));
                            }
                        } else {
                            let mut on = entry["enabled"] != false;
                            if ui
                                .add_enabled(available, egui::Checkbox::without_text(&mut on))
                                .changed()
                            {
                                row_action = Some((*index, Action::Toggle(on)));
                            }
                        }
                    });
                    for text in [
                        label(&entry["name"]),
                        summary,
                        mode_label(label(&entry["mode"])),
                    ] {
                        row.col(|ui| {
                            ui.add(egui::Label::new(text).truncate().selectable(false));
                        });
                    }
                    let response = row.response();
                    if response.clicked() || response.secondary_clicked() {
                        self.selected = id.to_owned();
                    }
                    if available && response.double_clicked() {
                        row_action = Some((*index, Action::Edit));
                    }
                    if available {
                        response.context_menu(|ui| {
                            for (title, action) in [
                                ("编辑", Action::Edit),
                                ("复制", Action::Copy),
                                ("上移", Action::Up),
                                ("下移", Action::Down),
                                ("删除", Action::Delete),
                            ] {
                                if ui.button(title).clicked() {
                                    row_action = Some((*index, action));
                                    ui.close();
                                }
                            }
                        });
                    }
                })
            });
        if proxy_toggle {
            self.request(
                worker,
                "POST",
                "/proxy",
                json!({"interceptTLS": enabled}),
                Reply::Proxy(enabled),
            );
        } else if let Some(patch) = patch {
            self.patch(worker, patch, Reply::Save);
        } else if let Some((index, action)) = row_action {
            self.act(worker, index, action);
        }
        self.dialogs(ui.ctx(), worker);
    }

    fn act(&mut self, worker: &Worker, index: usize, action: Action) {
        let key = self.section.list();
        let Some(mut entries) = self
            .settings
            .as_ref()
            .and_then(|settings| settings[key].as_array())
            .cloned()
        else {
            return;
        };
        let entry = entries[index].clone();
        match action {
            Action::Edit => {
                self.error = None;
                self.edit = Some(Edit::new(self.section, entry.clone(), Some(entry)));
            }
            Action::Delete => self.delete = Some(entry),
            Action::Toggle(enabled) => {
                entries[index]["enabled"] = json!(enabled);
                self.patch(worker, json!({(key): entries}), Reply::Save);
            }
            Action::Copy => {
                let mut copy = entry;
                copy["id"] = json!(uuid::Uuid::new_v4().to_string());
                copy["name"] = json!(format!("{} 副本", label(&copy["name"])));
                if self.section.active().is_none() {
                    copy["enabled"] = json!(false);
                }
                entries.insert(index + 1, copy);
                self.patch(worker, json!({(key): entries}), Reply::Save);
            }
            Action::Up if index > 0 => {
                entries.swap(index - 1, index);
                self.patch(worker, json!({(key): entries}), Reply::Save);
            }
            Action::Down if index + 1 < entries.len() => {
                entries.swap(index, index + 1);
                self.patch(worker, json!({(key): entries}), Reply::Save);
            }
            _ => {}
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, worker: &Worker) {
        let editing = matches!(self.reply, Some((Reply::Edit, _)));
        let mut close = false;
        let mut save = false;
        let mut refresh = false;
        if let Some(edit) = &mut self.edit {
            let response = egui::Modal::new(egui::Id::new("network_editor")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 50.0).min(680.0));
                ui.heading(format!("编辑{}", edit.section.label()));
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 180.0).max(210.0))
                    .show(ui, |ui| {
                        ui.add_enabled_ui(!editing, |ui| edit.form(ui));
                    });
                ui.separator();
                if let Some(error) = self.source_error.as_ref().or(self.error.as_ref()) {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if edit.close_confirm {
                    ui.label("放弃未保存的修改？");
                    ui.horizontal(|ui| {
                        if ui.button("继续编辑").clicked() {
                            edit.close_confirm = false;
                        }
                        close = ui.button("放弃修改").clicked();
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        let available = self.reply.is_none() && self.source_error.is_none();
                        refresh = ui
                            .add_enabled(self.reply.is_none(), egui::Button::new("刷新配置列表"))
                            .clicked();
                        if ui
                            .add_enabled(!editing, egui::Button::new("取消"))
                            .clicked()
                        {
                            if edit.dirty {
                                edit.close_confirm = true;
                            } else {
                                close = true;
                            }
                        }
                        save = ui
                            .add_enabled(
                                available,
                                egui::Button::new(if editing {
                                    "正在保存…"
                                } else {
                                    "保存配置"
                                }),
                            )
                            .clicked();
                        save |= available
                            && ui.input_mut(|input| {
                                input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)
                            });
                    });
                }
            });
            if !editing && response.should_close() {
                if edit.dirty {
                    edit.close_confirm = true;
                } else {
                    close = true;
                }
            }
        }
        if close {
            self.edit = None;
        }
        if refresh {
            self.refresh(worker);
        }
        if save {
            self.save_edit(worker);
        }
        if let Some(entry) = &self.delete {
            let result =
                egui::Modal::new(egui::Id::new("delete_network_profile")).show(ctx, |ui| {
                    ui.heading("删除配置");
                    ui.label(format!("确定删除「{}」？", label(&entry["name"])));
                    ui.horizontal(|ui| {
                        (ui.button("取消").clicked(), ui.button("删除配置").clicked())
                    })
                    .inner
                });
            if result.inner.1 {
                let entry = self.delete.take().unwrap();
                self.delete_entry(worker, &entry);
            } else if result.inner.0 || result.should_close() {
                self.delete = None;
            }
        }
    }

    fn save_edit(&mut self, worker: &Worker) {
        let (Some(edit), Some(settings)) = (&self.edit, &self.settings) else {
            return;
        };
        let key = edit.section.list();
        let mut value = edit.value.clone();
        if value.get("patterns").is_some() {
            value["patterns"] = json!(
                edit.pattern_draft
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
            );
        }
        let mut entries = settings[key].as_array().cloned().unwrap_or_default();
        if let Some(original) = &edit.original {
            let Some(index) = entries.iter().position(|entry| entry == original) else {
                self.error = Some("此配置已被修改或删除。请保留草稿，再打开当前配置。".into());
                return;
            };
            entries[index] = value;
        } else {
            entries.push(value);
        }
        self.patch(worker, json!({(key): entries}), Reply::Edit);
    }

    fn delete_entry(&mut self, worker: &Worker, entry: &Value) {
        let Some(settings) = &self.settings else {
            return;
        };
        let key = self.section.list();
        let mut entries = settings[key].as_array().cloned().unwrap_or_default();
        let Some(index) = entries.iter().position(|item| item == entry) else {
            self.error = Some("此配置已经更改，请重新选择。".into());
            return;
        };
        entries.remove(index);
        let mut patch = json!({(key): entries});
        if let Some(active) = self
            .section
            .active()
            .filter(|key| settings[*key] == entry["id"])
        {
            patch[active] = Value::Null;
        }
        self.patch(worker, patch, Reply::Save);
    }
}

#[derive(Clone, Copy)]
enum Action {
    Edit,
    Copy,
    Delete,
    Up,
    Down,
    Toggle(bool),
}

struct Edit {
    section: NetworkSection,
    value: Value,
    original: Option<Value>,
    pattern_draft: String,
    dirty: bool,
    close_confirm: bool,
}

impl Edit {
    fn new(section: NetworkSection, value: Value, original: Option<Value>) -> Self {
        let pattern_draft = pattern_lines(&value["patterns"]);
        Self {
            section,
            value,
            original,
            pattern_draft,
            dirty: false,
            close_confirm: false,
        }
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        let value = &mut self.value;
        let mut changed = text_field(ui, value, "name", "名称", false);
        match self.section {
            NetworkSection::Ssl => {
                changed |= choice(
                    ui,
                    value,
                    "mode",
                    "模式",
                    &[("intercept", "拦截匹配流量"), ("bypass", "绕行匹配流量")],
                );
                changed |= flag(ui, value, "hideBypassed", "不显示绕行流量");
                changed |= patterns(ui, &mut self.pattern_draft, "域名或域名:端口");
            }
            NetworkSection::Access => {
                changed |= choice(
                    ui,
                    value,
                    "mode",
                    "模式",
                    &[("allow", "仅允许匹配客户端"), ("deny", "禁止匹配客户端")],
                );
                changed |= patterns(ui, &mut self.pattern_draft, "IP 地址 / CIDR");
            }
            NetworkSection::Upstream => {
                changed |= text_field(ui, value, "host", "HTTP 代理服务器地址", false);
                changed |= port(ui, value);
                changed |= text_field(ui, value, "username", "用户名（可选）", false);
                changed |= text_field(ui, value, "password", "密码（可选）", true);
                changed |= choice(
                    ui,
                    value,
                    "mode",
                    "模式",
                    &[("allow", "仅代理匹配域名"), ("bypass", "不代理匹配域名")],
                );
                changed |= patterns(ui, &mut self.pattern_draft, "域名或域名:端口");
            }
            NetworkSection::Gateway => {
                changed |= flag(ui, value, "enabled", "启用此网关");
                changed |= patterns(ui, &mut self.pattern_draft, "通配规则");
                changed |= gateway_form(ui, value);
                changed |= text_field(ui, value, "folder", "文件夹", false);
            }
            NetworkSection::Mirror => {
                changed |= flag(ui, value, "enabled", "启用此镜像");
                changed |= text_field(
                    ui,
                    value,
                    "host",
                    "来源域名（可含端口，支持 * 和 ?）",
                    false,
                );
                changed |= text_field(ui, value, "target", "镜像地址 · 主机:端口", false);
                let policies = [
                    ("original", "保留原始地址"),
                    ("target", "使用镜像地址"),
                    ("preserve", "保留原值"),
                ];
                for (key, title) in [
                    ("hostPolicy", "Host 头部"),
                    ("sniPolicy", "SNI"),
                    ("cnPolicy", "证书 CN"),
                ] {
                    changed |= choice(ui, value, key, title, &policies);
                }
                changed |= text_field(ui, value, "folder", "文件夹", false);
            }
            NetworkSection::Reverse => {
                changed |= flag(ui, value, "enabled", "启用此反向代理");
                changed |= text_field(ui, value, "listenHost", "监听 IP", false);
                changed |= port(ui, value);
                changed |= text_field(ui, value, "target", "目标 HTTP / HTTPS URL", false);
                changed |= flag(ui, value, "preserveHost", "保留原 Host");
            }
        }
        self.dirty |= changed;
    }
}

fn mode_label(key: &str) -> &str {
    match key {
        "intercept" => "拦截",
        "bypass" => "绕行",
        "allow" => "允许",
        "deny" => "禁止",
        "blockRequest" => "屏蔽请求",
        "blockResponse" => "屏蔽响应",
        "suspendRequest" => "暂停请求",
        "suspendResponse" => "暂停响应",
        _ => key,
    }
}

fn gateway_form(ui: &mut egui::Ui, value: &mut Value) -> bool {
    let mut changed = choice(ui, value, "layer", "控制层", &[("l4", "L4"), ("l7", "L7")]);
    let l4 = value["layer"] == "l4";
    let modes = [
        "allow",
        "bypass",
        "blockRequest",
        "blockResponse",
        "suspendRequest",
        "suspendResponse",
    ];
    let entries = modes[..if l4 { 3 } else { modes.len() }]
        .iter()
        .map(|mode| (*mode, mode_label(mode)))
        .collect::<Vec<_>>();
    changed |= choice(ui, value, "mode", "行为", &entries);
    changed |= flag(ui, value, "showBlocked", "记录被屏蔽的流量");
    ui.separator();
    ui.strong("匹配条件");
    let matchers = value["matchers"].as_array_mut().unwrap();
    let mut remove = None;
    for (index, matcher) in matchers.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            let types = [
                ("domain", "域名"),
                ("ip", "IP / CIDR"),
                ("http", "HTTP"),
                ("websocket", "WebSocket"),
            ];
            changed |= choice(
                ui,
                matcher,
                "type",
                "类型",
                &types[..if l4 { 2 } else { types.len() }],
            );
            changed |= text_field(ui, matcher, "pattern", "匹配内容", false);
            if matcher["type"] == "http" {
                changed |= text_field(ui, matcher, "method", "方法 / *", false);
            }
            changed |= text_field(ui, matcher, "ports", "端口范围（可选）", false);
            if ui.small_button("移除条件").clicked() {
                remove = Some(index);
            }
            ui.separator();
        });
    }
    if let Some(index) = remove {
        matchers.remove(index);
        changed = true;
    }
    if ui.button("添加匹配条件").clicked() {
        matchers.push(json!({"type": "domain", "pattern": "", "ports": "", "method": "*"}));
        changed = true;
    }
    ui.weak("域名和 URL 可使用 * 和 ?。端口可填写 80,8080,9090-9999，留空时匹配所有端口。");
    changed
}

fn text_field(
    ui: &mut egui::Ui,
    value: &mut Value,
    key: &str,
    title: &str,
    password: bool,
) -> bool {
    let mut text = match value[key].take() {
        Value::String(text) => text,
        _ => String::new(),
    };
    let changed = ui
        .horizontal(|ui| {
            ui.label(title);
            ui.add(
                egui::TextEdit::singleline(&mut text)
                    .password(password)
                    .desired_width(f32::INFINITY),
            )
            .changed()
        })
        .inner;
    if text.is_empty() && matches!(key, "username" | "password" | "folder") {
        value.as_object_mut().unwrap().remove(key);
    } else {
        value[key] = Value::String(text);
    }
    changed
}

fn pattern_lines(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

fn patterns(ui: &mut egui::Ui, text: &mut String, title: &str) -> bool {
    ui.label(format!("{title}（每行一项）"));
    ui.add(
        egui::TextEdit::multiline(text)
            .desired_rows(5)
            .desired_width(f32::INFINITY),
    )
    .changed()
}

fn port(ui: &mut egui::Ui, value: &mut Value) -> bool {
    let mut port = value["port"].as_u64().unwrap_or_default() as u16;
    let changed = ui
        .horizontal(|ui| {
            ui.label("端口");
            ui.add(egui::DragValue::new(&mut port).range(1..=65535))
                .changed()
        })
        .inner;
    if changed {
        value["port"] = json!(port);
    }
    changed
}

fn choice(
    ui: &mut egui::Ui,
    value: &mut Value,
    key: &str,
    title: &str,
    options: &[(&str, &str)],
) -> bool {
    let current = label(&value[key]);
    let mut selected = current;
    let name = options
        .iter()
        .find(|(key, _)| *key == selected)
        .map_or(selected, |(_, name)| name);
    egui::ComboBox::from_label(title)
        .selected_text(name)
        .show_ui(ui, |ui| {
            for (key, name) in options {
                ui.selectable_value(&mut selected, *key, *name);
            }
        });
    if current != selected {
        value[key] = json!(selected);
        true
    } else {
        false
    }
}

fn flag(ui: &mut egui::Ui, value: &mut Value, key: &str, title: &str) -> bool {
    let mut enabled = value[key] == true;
    if ui.checkbox(&mut enabled, title).changed() {
        value[key] = json!(enabled);
        true
    } else {
        false
    }
}
