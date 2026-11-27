mod breakpoint;
mod editor;
mod share;
mod workspace;

use crate::sharing::ShareUi;
use crate::theme::{Theme, dialog_columns, dialog_footer, dialog_header, input_text};
use crate::worker::{RequestHandle, Worker};
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::sync::oneshot::error::TryRecvError;

use breakpoint::Breakpoint;
use editor::{RuleEditor, action_name, new_rule};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum RuleCategory {
    #[default]
    All,
    Rewrite,
    Script,
    Breakpoint,
}

impl RuleCategory {
    const CATEGORIES: [Self; 4] = [Self::All, Self::Rewrite, Self::Script, Self::Breakpoint];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Rewrite => "重写",
            Self::Script => "脚本",
            Self::Breakpoint => "断点",
        }
    }

    fn setting(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Rewrite => Some("rewriteEnabled"),
            Self::Script => Some("scriptEnabled"),
            Self::Breakpoint => Some("breakpointEnabled"),
        }
    }

    fn folder_key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Rewrite => "rewrite",
            Self::Script => "script",
            Self::Breakpoint => "breakpoint",
        }
    }

    fn matches(self, rule: &Value) -> bool {
        let phases = [&rule["request"], &rule["response"]];
        let breakpoint = phases.iter().any(|action| action["breakpoint"] == true);
        let script = phases.iter().any(|action| {
            ["python", "script", "pythonValue", "scriptValue"]
                .iter()
                .any(|key| action.get(key).is_some())
        });
        match self {
            Self::All => true,
            Self::Breakpoint => breakpoint,
            Self::Script => script,
            Self::Rewrite => {
                rule.get("mock").is_some()
                    || phases.iter().any(|action| {
                        action.as_object().is_some_and(|object| {
                            object.keys().any(|key| {
                                !matches!(
                                    key.as_str(),
                                    "breakpoint"
                                        | "python"
                                        | "script"
                                        | "pythonValue"
                                        | "scriptValue"
                                )
                            })
                        })
                    })
            }
        }
    }
}

#[derive(Default)]
pub(super) struct RulesUi {
    workspace: workspace::Workspace,
    open: bool,
    category: RuleCategory,
    rules: Option<Vec<Value>>,
    rules_reply: Option<(RuleReply, RequestHandle)>,
    settings: Option<Value>,
    settings_reply: Option<RequestHandle>,
    pending: Vec<Value>,
    deferred: BTreeSet<String>,
    breakpoint: Option<Breakpoint>,
    editor: Option<RuleEditor>,
    delete: Option<Value>,
    new_folder: Option<String>,
    folder: String,
    search: String,
    selected: Option<String>,
    error: Option<String>,
    source_error: Option<String>,
    settings_error: Option<String>,
    logs: Option<Vec<Value>>,
    log_reply: Option<RequestHandle>,
    file: Option<(bool, crate::files::FilePick)>,
    sharing: ShareUi<Value>,
}

enum RuleReply {
    Load,
    Save {
        reopen: bool,
        selected: Option<String>,
    },
    SaveWorkspace(String),
    Toggle,
}

impl RulesUi {
    pub(super) fn open(&mut self, category: RuleCategory) {
        if !self.child_open() {
            self.select_category(category);
            self.open = true;
        }
    }

    fn select_category(&mut self, category: RuleCategory) {
        if self.category != category {
            self.category = category;
            self.folder.clear();
            self.selected = None;
        }
    }

    fn child_open(&self) -> bool {
        self.editor.is_some()
            || self.delete.is_some()
            || self.new_folder.is_some()
            || self.logs.is_some()
            || self.file.is_some()
            || self.sharing.is_open()
            || self.breakpoint.is_some()
    }

    pub(super) fn new_for_capture(&mut self, category: RuleCategory, method: &str, url: &str) {
        if self.child_open() {
            return;
        }
        self.select_category(category);
        let mut value = new_rule(category, &self.folder);
        value["match"]["url"] = json!(url);
        value["match"]["method"] = json!(method);
        if let Ok(url) = url::Url::parse(url) {
            if let Some(host) = url.host_str() {
                value["name"] = json!(format!("{} {host}", category.label()));
            }
        }
        self.editor = Some(RuleEditor::new(value, None));
    }

    pub(super) fn update_pending(&mut self, mut value: Value) {
        self.pending = items(&mut value, "pending");
        self.deferred
            .retain(|id| self.pending.iter().any(|p| p["id"] == id.as_str()));
    }

    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn poll(&mut self) {
        if let Some((kind, result)) = self.rules_reply.as_mut().and_then(|(kind, receiver)| {
            finished(receiver).map(|result| (std::mem::replace(kind, RuleReply::Load), result))
        }) {
            self.rules_reply = None;
            match result {
                Ok(mut value) => {
                    self.error = None;
                    match kind {
                        RuleReply::Toggle => {
                            if let Some(list) = &mut self.rules {
                                if let Some(rule) =
                                    list.iter_mut().find(|rule| rule["id"] == value["id"])
                                {
                                    self.workspace.apply_rule_toggle(rule, &value);
                                    rule["enabled"] = value["enabled"].take();
                                }
                            }
                        }
                        RuleReply::Load | RuleReply::Save { .. } | RuleReply::SaveWorkspace(_) => {
                            self.source_error = value["error"].as_str().map(str::to_owned);
                            self.rules = Some(items(&mut value, "rules"));
                            match kind {
                                RuleReply::Save { reopen, selected } => {
                                    self.selected = selected;
                                    if reopen {
                                        self.editor = None;
                                    }
                                }
                                RuleReply::SaveWorkspace(id) => {
                                    self.workspace.finish_rule_save(&id, Ok(()))
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Err(error) => match kind {
                    RuleReply::Save { reopen: true, .. } if self.editor.is_some() => {
                        self.editor.as_mut().unwrap().error = Some(error);
                    }
                    RuleReply::SaveWorkspace(id) => {
                        self.workspace.finish_rule_save(&id, Err(error))
                    }
                    _ => self.error = Some(error),
                },
            }
        }
        if let Some(result) = self.settings_reply.as_mut().and_then(finished) {
            self.settings_reply = None;
            match result {
                Ok(value) => {
                    self.settings_error = value["error"].as_str().map(str::to_owned);
                    self.settings = Some(value);
                }
                Err(error) => self.settings_error = Some(error),
            }
        }
        if let Some(result) = self.log_reply.as_mut().and_then(finished) {
            self.log_reply = None;
            match result {
                Ok(mut value) => self.logs = Some(items(&mut value, "logs")),
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(editor) = &mut self.editor {
            editor.poll();
        }
        self.workspace.poll();
        if self.breakpoint.as_mut().is_some_and(Breakpoint::poll) {
            if let Some(existing) = self.breakpoint.take() {
                self.deferred.insert(existing.id);
            }
        }
    }

    fn refresh(&mut self, worker: &Worker) {
        self.workspace.refresh_values(worker);
        if self.rules_reply.is_none() {
            self.send_rules(worker, "GET", "/rules", Value::Null, RuleReply::Load);
        }
        if self.settings_reply.is_none() {
            self.send_settings(worker, "GET", Value::Null);
        }
    }

    fn send_rules(
        &mut self,
        worker: &Worker,
        method: &str,
        path: &str,
        body: Value,
        reply: RuleReply,
    ) {
        match worker.request(method, path, body) {
            Ok(request) => self.rules_reply = Some((reply, request)),
            Err(error) => self.error = Some(error),
        }
    }

    fn send_settings(&mut self, worker: &Worker, method: &str, body: Value) {
        match worker.request(method, "/settings", body) {
            Ok(request) => self.settings_reply = Some(request),
            Err(error) => self.settings_error = Some(error),
        }
    }

    fn save_list(
        &mut self,
        worker: &Worker,
        next: Vec<Value>,
        selected: Option<String>,
        editor: bool,
    ) {
        if let Some(expected) = &self.rules {
            self.send_rules(
                worker,
                "PUT",
                "/rules",
                json!({"rules": next, "expected": expected}),
                RuleReply::Save {
                    reopen: editor,
                    selected,
                },
            );
        }
    }

    fn rules_dialog(&mut self, ctx: &egui::Context, worker: &Worker) {
        if self.rules.is_none() && self.rules_reply.is_none() && self.error.is_none() {
            self.refresh(worker);
        }
        let width = (ctx.content_rect().width() - 64.0).min(820.0);
        let height = (ctx.content_rect().height() - 130.0).min(400.0);
        let modal = Theme::modal(ctx, egui::Id::new("local_rules")).show(ctx, |ui| {
            ui.set_width(width);
            let close = dialog_header(ui, "本地规则", !self.child_open());
            let current = self.category;
            dialog_columns(
                ui,
                egui::vec2(width, height),
                |ui| {
                    let mut selected = current;
                    for category in RuleCategory::CATEGORIES {
                        if crate::workbench::sidebar_row(
                            ui,
                            category.label(),
                            Some(category == current),
                        )
                        .clicked()
                        {
                            selected = category;
                        }
                    }
                    selected
                },
                |ui, category| {
                    if !self.child_open() {
                        self.select_category(category);
                    }
                    self.show_list(ui, worker);
                },
            );
            close
        });
        if !self.child_open() && (modal.inner || modal.should_close()) {
            self.open = false;
        }
    }

    fn show_list(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(self.category.label());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("脚本控制台").clicked() {
                    self.load_logs(worker);
                }
                if ui.button("刷新规则").clicked() {
                    self.refresh(worker);
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            for category in RuleCategory::CATEGORIES {
                if self.category != RuleCategory::All && self.category != category {
                    continue;
                }
                let Some(key) = category.setting() else {
                    continue;
                };
                let old = self.settings.as_ref().and_then(|s| s[key].as_bool());
                let mut enabled = old.unwrap_or(false);
                let available =
                    old.is_some() && self.settings_reply.is_none() && self.settings_error.is_none();
                if ui
                    .add_enabled(
                        available,
                        egui::Checkbox::new(&mut enabled, format!("启用{}", category.label())),
                    )
                    .changed()
                {
                    self.send_settings(
                        worker,
                        "PATCH",
                        json!({(key): enabled, "_expected": {(key): old}}),
                    );
                }
            }
            if !self.pending.is_empty() {
                ui.menu_button(format!("待处理断点 ({})", self.pending.len()), |ui| {
                    let mut chosen = None;
                    for item in &self.pending {
                        let title = format!(
                            "{} · {}",
                            breakpoint::title(item),
                            label(&item["request"]["url"])
                        );
                        if ui.button(title).clicked() {
                            chosen = Some(item.clone());
                            ui.close();
                        }
                    }
                    if let Some(item) = chosen {
                        self.breakpoint = Some(Breakpoint::new(item, worker));
                    }
                });
            }
        });
        for error in [&self.source_error, &self.settings_error, &self.error]
            .into_iter()
            .flatten()
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(notice) = self.sharing.notice() {
            match notice {
                Ok(message) => {
                    ui.weak(message);
                }
                Err(error) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            }
        }
        ui.separator();
        if self.rules.is_none() {
            if self.rules_reply.is_some() {
                ui.spinner();
            }
            return;
        }

        let ready = self.rules_reply.is_none()
            && self.source_error.is_none()
            && self.file.is_none()
            && !self.sharing.is_open();
        let folders = self.folders();
        let mut edit_index = None;
        let mut toggle = None;
        let mut action = None;
        egui::Panel::bottom("rule_actions").show(ui, |ui| {
            ui.add_enabled_ui(ready, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("新建规则").clicked() {
                        self.editor =
                            Some(RuleEditor::new(new_rule(self.category, &self.folder), None));
                    }
                    if ui.button("新建文件夹").clicked() {
                        self.new_folder = Some(String::new());
                    }
                    let selected = self.selected.as_deref().and_then(|id| {
                        self.rules
                            .as_ref()?
                            .iter()
                            .position(|item| item["id"] == id)
                    });
                    ui.add_enabled_ui(selected.is_some(), |ui| {
                        if ui.button("编辑").clicked() {
                            edit_index = selected;
                        }
                        if ui.button("复制").clicked() {
                            action = selected.map(|index| (index, "copy"));
                        }
                        if ui.button("上移").clicked() {
                            action = selected.map(|index| (index, "up"));
                        }
                        if ui.button("下移").clicked() {
                            action = selected.map(|index| (index, "down"));
                        }
                        if ui.button("删除").clicked() {
                            action = selected.map(|index| (index, "delete"));
                        }
                    });
                    ui.separator();
                    ui.menu_button("分享", |ui| {
                        if let Some(rule) =
                            selected.and_then(|index| self.rules.as_ref()?.get(index))
                            && self.sharing.export_menu(ui, "recho-rule.json", || {
                                share::encode_rule(
                                    rule,
                                    self.workspace.values.as_deref().unwrap_or_default(),
                                )
                            })
                        {
                            ui.close();
                        }
                        if ui.button("导入分享规则…").clicked() {
                            self.sharing.open("导入分享规则");
                            ui.close();
                        }
                    });
                    if ui.button("导入").clicked() {
                        self.file = Some((false, crate::files::pick("json", false, ui.ctx())));
                    }
                    if ui.button("导出").clicked() {
                        self.file = Some((true, crate::files::pick("json", true, ui.ctx())));
                    }
                });
            });
        });
        if !folders.is_empty() {
            egui::Panel::left("rule_folders")
                .default_size(145.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for folder in std::iter::once("").chain(folders.iter().map(String::as_str))
                        {
                            let caption = if folder.is_empty() {
                                "全部规则"
                            } else {
                                folder
                            };
                            if ui
                                .selectable_label(self.folder == folder, caption)
                                .clicked()
                            {
                                self.folder = folder.to_owned();
                                self.selected = None;
                            }
                        }
                    });
                });
        }
        let colors = Theme::current_palette(ui.ctx());
        let mut frame = colors.input_frame().begin(ui);
        let width = frame.content_ui.available_width();
        let hint = "搜索规则名称、文件夹或 URL";
        let search = frame
            .content_ui
            .add(input_text(&mut self.search, hint, width));
        search
            .ctx
            .accesskit_node_builder(search.id, |node| node.set_label(hint));
        frame.frame.stroke = colors.input_stroke(&search);
        frame.end(ui);
        let rules = self.rules.as_ref().unwrap();
        let search = self.search.to_lowercase();
        let visible = rules
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let folder = label(&item["folder"]);
                let under_folder = self.folder.is_empty()
                    || folder == self.folder
                    || folder
                        .strip_prefix(&self.folder)
                        .is_some_and(|suffix| suffix.starts_with('/'));
                self.category.matches(item)
                    && under_folder
                    && ["name", "folder"]
                        .iter()
                        .map(|key| label(&item[*key]))
                        .chain(std::iter::once(label(&item["match"]["url"])))
                        .any(|text| text.to_lowercase().contains(&search))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        TableBuilder::new(ui)
            .id_salt("rule_table")
            .sense(egui::Sense::click())
            .striped(true)
            .column(Column::exact(45.0))
            .column(Column::initial(180.0).clip(true))
            .column(Column::remainder().clip(true))
            .column(Column::initial(180.0).clip(true))
            .header(24.0, |mut header| {
                for title in ["启用", "名称", "匹配条件", "行为"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(25.0, visible.len(), |mut row| {
                    let index = visible[row.index()];
                    let item = &rules[index];
                    let id = label(&item["id"]);
                    let old = item["enabled"] != false;
                    let mut enabled = old;
                    row.set_selected(self.selected.as_deref() == Some(id));
                    row.col(|ui| {
                        if ui
                            .add_enabled(ready, egui::Checkbox::without_text(&mut enabled))
                            .changed()
                        {
                            toggle = Some((id.to_owned(), old, enabled));
                        }
                    });
                    for value in [
                        label(&item["name"]).to_owned(),
                        format!(
                            "{}  {}",
                            label(&item["match"]["method"]),
                            label(&item["match"]["url"])
                        ),
                        behavior(item),
                    ] {
                        row.col(|ui| {
                            ui.add(egui::Label::new(value).truncate().selectable(false));
                        });
                    }
                    let response = row.response();
                    if response.clicked() {
                        self.selected = Some(id.to_owned());
                    }
                    if ready && response.double_clicked() {
                        edit_index = Some(index);
                    }
                    if ready {
                        response.context_menu(|ui| {
                            for (caption, option) in [
                                ("编辑", "edit"),
                                ("复制", "copy"),
                                ("上移", "up"),
                                ("下移", "down"),
                                ("删除", "delete"),
                            ] {
                                if ui.button(caption).clicked() {
                                    action = Some((index, option));
                                    ui.close();
                                }
                            }
                        });
                    }
                });
            });
        if let Some((id, old, enabled)) = toggle {
            self.toggle_rule(worker, id, old, enabled);
        } else if let Some(index) = edit_index {
            self.edit(index);
        } else if let Some((index, action)) = action {
            self.act(worker, index, action);
        }
    }

    fn toggle_rule(&mut self, worker: &Worker, id: String, old: bool, enabled: bool) {
        self.send_rules(
            worker,
            "PATCH",
            "/rules/enabled",
            json!({"id": id, "expected": old, "enabled": enabled}),
            RuleReply::Toggle,
        );
    }

    fn edit(&mut self, index: usize) {
        if let Some(item) = self.rules.as_ref().and_then(|rules| rules.get(index)) {
            self.editor = Some(RuleEditor::new(item.clone(), Some(item.clone())));
        }
    }

    fn act(&mut self, worker: &Worker, index: usize, action: &str) {
        let Some(mut rules) = self.rules.clone() else {
            return;
        };
        let id = rules[index]["id"].as_str().map(str::to_owned);
        match action {
            "edit" => self.edit(index),
            "delete" => self.delete = Some(rules.swap_remove(index)),
            "copy" => {
                let mut copy = rules[index].clone();
                let id = uuid::Uuid::new_v4().to_string();
                copy["id"] = json!(id);
                copy["name"] = json!(format!("{} 副本", label(&copy["name"])));
                rules.insert(index + 1, copy);
                self.save_list(worker, rules, Some(id), false);
            }
            "up" if index > 0 => {
                rules.swap(index, index - 1);
                self.save_list(worker, rules, id, false);
            }
            "down" if index + 1 < rules.len() => {
                rules.swap(index, index + 1);
                self.save_list(worker, rules, id, false);
            }
            _ => {}
        }
    }

    fn folders(&self) -> BTreeSet<String> {
        let key = self.category.folder_key();
        let configured = self
            .settings
            .as_ref()
            .and_then(|s| s["ruleFolders"][key].as_array());
        configured
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(
                self.rules
                    .iter()
                    .flatten()
                    .filter(|r| self.category.matches(r))
                    .filter_map(|r| r["folder"].as_str()),
            )
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn load_logs(&mut self, worker: &Worker) {
        self.logs.get_or_insert_default();
        if self.log_reply.is_none() {
            match worker.request("GET", "/script-logs", Value::Null) {
                Ok(reply) => self.log_reply = Some(reply),
                Err(error) => self.error = Some(error),
            }
        }
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context, worker: &Worker) {
        self.finish_file(worker);
        if self.open {
            self.rules_dialog(ctx, worker);
        }
        if let Some(rule) = self.sharing.poll(ctx, share::preview_rule) {
            if self.rules_reply.is_some() || self.source_error.is_some() {
                self.error = Some("规则列表正在更新，请重试。".into());
            } else if share::requires_local_file(&rule) {
                self.editor = Some(RuleEditor::relink_import(rule));
            } else if let Some(mut rules) = self.rules.clone() {
                let id = rule["id"].as_str().map(str::to_owned);
                rules.push(rule);
                self.save_list(worker, rules, id, false);
            }
        }
        if self.editor.is_some()
            && self.rules.is_none()
            && self.rules_reply.is_none()
            && self.error.is_none()
        {
            self.send_rules(worker, "GET", "/rules", Value::Null, RuleReply::Load);
        }
        self.workspace
            .load_editor_values(worker, self.editor.is_some());
        let mut close_editor = false;
        let mut refresh = false;
        let mut save = None;
        let blocked_by_breakpoint = self.breakpoint.is_some();
        if let Some(editor) = &mut self.editor {
            let saving = matches!(
                self.rules_reply,
                Some((RuleReply::Save { reopen: true, .. }, _))
            );
            editor.show(
                ctx,
                worker,
                editor::DialogState {
                    can_save: self.rules.is_some()
                        && self.rules_reply.is_none()
                        && self.source_error.is_none(),
                    saving,
                    keyboard: !blocked_by_breakpoint,
                    source_error: self.source_error.as_deref().or(self.error.as_deref()),
                    values: self.workspace.values.as_deref().unwrap_or_default(),
                },
                &mut close_editor,
                &mut refresh,
                &mut save,
            );
        }
        if close_editor {
            self.editor = None;
        }
        if refresh {
            self.refresh(worker);
        }
        if let Some(value) = save {
            self.save_editor(worker, value);
        }
        if let Some(item) = &self.delete {
            let colors = Theme::current_palette(ctx);
            let reply = Theme::modal(ctx, egui::Id::new("delete_rule")).show(ctx, |ui| {
                ui.set_width(340.0);
                let close = dialog_header(ui, "删除规则", true);
                ui.label(format!("确定删除「{}」？", label(&item["name"])));
                dialog_footer(ui, |ui| {
                    let delete = ui
                        .add(colors.dialog_action("删除规则", colors.red))
                        .clicked();
                    let cancel = ui.button("取消").clicked();
                    (close || cancel, delete)
                })
            });
            if reply.inner.1 {
                if let Some(mut next) = self.rules.clone() {
                    if let Some(index) = next.iter().position(|rule| rule == item) {
                        next.remove(index);
                        self.save_list(worker, next, None, false);
                    } else {
                        self.error = Some("规则内容已更改，请重新选择。".into());
                    }
                }
                self.delete = None;
            } else if reply.inner.0 || reply.should_close() {
                self.delete = None;
            }
        }
        self.folder_dialog(ctx, worker);
        self.log_dialog(ctx, worker);
        if self.breakpoint.is_none() {
            if let Some(item) = self
                .pending
                .iter()
                .filter(|item| !self.deferred.contains(label(&item["id"])))
                .min_by_key(|item| label(&item["expiresAt"]))
            {
                self.breakpoint = Some(Breakpoint::new(item.clone(), worker));
            }
        }
        if let Some(dialog) = &mut self.breakpoint {
            let live = self
                .pending
                .iter()
                .any(|item| item["id"] == dialog.id.as_str());
            if dialog.show(ctx, worker, live) {
                self.deferred.insert(dialog.id.clone());
                self.breakpoint = None;
                if self
                    .pending
                    .iter()
                    .any(|item| !self.deferred.contains(label(&item["id"])))
                {
                    ctx.request_repaint();
                }
            }
        }
    }

    fn save_editor(&mut self, worker: &Worker, value: Value) {
        let (Some(editor), Some(mut rules)) = (&mut self.editor, self.rules.clone()) else {
            return;
        };
        if let Some(baseline) = &editor.baseline {
            let Some(index) = rules.iter().position(|item| item == baseline) else {
                editor.error =
                    Some("该规则已在其他窗口修改或删除，请在保留草稿后重新打开规则。".into());
                return;
            };
            rules[index] = value.clone();
        } else {
            rules.push(value.clone());
        }
        let id = value["id"].as_str().map(str::to_owned);
        self.save_list(worker, rules, id, true);
    }

    fn folder_dialog(&mut self, ctx: &egui::Context, worker: &Worker) {
        let Some(name) = &mut self.new_folder else {
            return;
        };
        let colors = Theme::current_palette(ctx);
        let reply = Theme::modal(ctx, egui::Id::new("add_rule_folder")).show(ctx, |ui| {
            ui.set_width(340.0);
            let close = dialog_header(ui, "新建文件夹", true);
            let mut frame = colors.input_frame().begin(ui);
            let width = frame.content_ui.available_width();
            let hint = "文件夹名称或子路径";
            let field = frame.content_ui.add(input_text(name, hint, width));
            field
                .ctx
                .accesskit_node_builder(field.id, |node| node.set_label(hint));
            frame.frame.stroke = colors.input_stroke(&field);
            frame.end(ui);
            dialog_footer(ui, |ui| {
                let create = ui
                    .add_enabled(
                        !name.trim_matches([' ', '/']).is_empty(),
                        colors.dialog_action("创建", colors.accent),
                    )
                    .clicked();
                let cancel = ui.button("取消").clicked();
                (close || cancel, create)
            })
        });
        if reply.inner.1 {
            if let Some(settings) = &self.settings {
                let name = name.trim().trim_matches('/').trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let old = settings["ruleFolders"].clone();
                let mut folders = old.clone();
                let key = self.category.folder_key();
                let mut names = self.folders();
                names.insert(name);
                folders[key] = json!(names);
                self.send_settings(
                    worker,
                    "PATCH",
                    json!({"ruleFolders": folders, "_expected": {"ruleFolders": old}}),
                );
            }
            self.new_folder = None;
        } else if reply.inner.0 || reply.should_close() {
            self.new_folder = None;
        }
    }

    fn log_dialog(&mut self, ctx: &egui::Context, worker: &Worker) {
        let Some(logs) = &self.logs else { return };
        let reply = Theme::modal(ctx, egui::Id::new("rule_logs")).show(ctx, |ui| {
            ui.set_width(ctx.content_rect().width().min(740.0) - 48.0);
            let close = dialog_header(ui, "脚本控制台", true);
            egui::ScrollArea::vertical()
                .max_height(430.0)
                .show(ui, |ui| {
                    for log in logs.iter().rev() {
                        ui.weak(format!(
                            "{}  {}  {}",
                            label(&log["timestamp"]),
                            label(&log["phase"]),
                            label(&log["url"])
                        ));
                        ui.label(egui::RichText::new(label(&log["text"])).monospace());
                        ui.separator();
                    }
                    if logs.is_empty() {
                        ui.weak("暂无脚本输出");
                    }
                });
            let refresh = dialog_footer(ui, |ui| ui.button("刷新").clicked());
            (close, refresh)
        });
        if reply.inner.0 || reply.should_close() {
            self.logs = None;
            self.log_reply = None;
        } else if reply.inner.1 {
            self.load_logs(worker);
        }
    }

    fn finish_file(&mut self, worker: &Worker) {
        let Some((save, result)) = self
            .file
            .as_mut()
            .and_then(|(save, reply)| finished(reply).map(|result| (*save, result)))
        else {
            return;
        };
        self.file = None;
        let result = (|| -> Result<(), String> {
            let Some(path) = result? else { return Ok(()) };
            let Some(rules) = &self.rules else {
                return Err("请先加载规则列表。".into());
            };
            if save {
                let values = self.workspace.values.as_deref().unwrap_or_default();
                let exported = rules
                    .iter()
                    .map(|rule| share::inline_values(rule, values))
                    .collect::<Result<Vec<_>, _>>()?;
                let bytes = serde_json::to_vec_pretty(&json!({"rules": exported}))
                    .map_err(|e| e.to_string())?;
                recho_core::native::write_local_file(&path, &bytes)?;
            } else {
                use std::io::Read;
                let file = std::fs::File::open(path).map_err(|err| err.to_string())?;
                let mut bytes = Vec::new();
                const LIMIT: u64 = 16 * 1024 * 1024;
                file.take(LIMIT + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|err| err.to_string())?;
                if bytes.len() as u64 > LIMIT {
                    return Err("规则文件超过 16 MiB。".into());
                }
                let mut value: Value =
                    serde_json::from_slice(&bytes).map_err(|err| err.to_string())?;
                if value.is_object() {
                    value = value["rules"].take();
                }
                let Value::Array(imported) = value else {
                    return Err("规则文件需要 JSON 数组。".into());
                };
                let mut next = rules.clone();
                for mut item in imported {
                    let object = item.as_object_mut().ok_or("规则必须为 JSON 对象。")?;
                    object.insert("id".into(), json!(uuid::Uuid::new_v4().to_string()));
                    // Imported scripts and broad matches require an explicit enable action.
                    object.insert("enabled".into(), json!(false));
                    next.push(item);
                }
                self.save_list(worker, next, None, false);
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
}

fn items(value: &mut Value, key: &str) -> Vec<Value> {
    match value[key].take() {
        Value::Array(items) => items,
        _ => Vec::new(),
    }
}

pub(super) fn label(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

pub(super) fn finished<T>(
    reply: &mut tokio::sync::oneshot::Receiver<Result<T, String>>,
) -> Option<Result<T, String>> {
    match reply.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Closed) => Some(Err("引擎请求已结束，请重试。".into())),
    }
}

fn behavior(rule: &Value) -> String {
    let mut actions = Vec::new();
    if rule["mock"].is_object() {
        actions.push("本地响应".to_owned());
    }
    for (phase, label) in [("request", "请求"), ("response", "响应")] {
        if rule[phase].is_object() {
            actions.push(format!("{label} · {}", action_name(&rule[phase])));
        }
    }
    actions.join(" / ")
}
