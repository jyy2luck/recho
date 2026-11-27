use super::{
    RuleCategory, RuleEditor, RuleReply, RulesUi, Value, Worker, egui, finished, items, json,
    label, new_rule,
};
use crate::theme::{Theme, control_row, input_text};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Workspace {
    showing_values: bool,
    rule_search: String,
    value_search: String,
    rule_id: Option<String>,
    value_id: Option<String>,
    return_rule: Option<String>,
    rule_drafts: BTreeMap<String, RuleEditor>,
    value_drafts: BTreeMap<String, ValueDraft>,
    pub(super) values: Option<Vec<Value>>,
    values_reply: Option<(ValueReply, super::RequestHandle)>,
    values_error: Option<String>,
    values_source_error: Option<String>,
}

struct ValueDraft {
    baseline: Option<Value>,
    value: Value,
    dirty: bool,
    delete: bool,
}

impl ValueDraft {
    fn new(value: Value, baseline: Option<Value>) -> Self {
        Self {
            value,
            baseline,
            dirty: false,
            delete: false,
        }
    }
}

enum ValueReply {
    Load,
    Save(String),
    Delete(String),
}

impl RulesUi {
    pub(crate) fn refresh_workspace(&mut self, worker: &Worker) {
        self.refresh(worker);
    }

    pub(crate) fn workspace_can_save(&self) -> bool {
        if self.open
            || self.child_open()
            || self
                .pending
                .iter()
                .any(|item| !self.deferred.contains(label(&item["id"])))
        {
            return false;
        }
        let workspace = &self.workspace;
        if workspace.showing_values {
            workspace.values.is_some()
                && workspace.values_reply.is_none()
                && workspace.values_source_error.is_none()
                && workspace
                    .value_id
                    .as_ref()
                    .and_then(|id| workspace.value_drafts.get(id))
                    .is_some_and(|draft| !draft.delete && (draft.dirty || draft.baseline.is_none()))
        } else {
            self.rules.is_some()
                && self.rules_reply.is_none()
                && self.source_error.is_none()
                && workspace
                    .rule_id
                    .as_ref()
                    .and_then(|id| workspace.rule_drafts.get(id))
                    .is_some_and(|draft| draft.is_dirty() || draft.baseline.is_none())
        }
    }

    pub(crate) fn save_workspace(&mut self, worker: &Worker) {
        if !self.workspace_can_save() {
            return;
        }
        if self.workspace.showing_values {
            self.workspace.write_value(worker, false);
            return;
        }
        let Some(id) = self.workspace.rule_id.clone() else {
            return;
        };
        let Some(draft) = self.workspace.rule_drafts.get_mut(&id) else {
            return;
        };
        match draft.workspace_value() {
            Ok(value) => {
                let Some(current) = &self.rules else { return };
                if let Some(next) =
                    replace_record(current, &id, draft.baseline.as_ref(), Some(value))
                {
                    self.send_rules(
                        worker,
                        "PUT",
                        "/rules",
                        json!({"rules": next, "expected": current}),
                        RuleReply::SaveWorkspace(id),
                    );
                } else {
                    draft.error =
                        Some("规则已更新或删除。草稿仍在编辑器中，可复制内容后重新载入。".into());
                }
            }
            Err(error) => draft.error = Some(error),
        }
    }

    pub(crate) fn workspace(&mut self, ui: &mut egui::Ui, worker: &Worker, sidebar_visible: bool) {
        if self.rules.is_none() && self.rules_reply.is_none() && self.error.is_none() {
            self.refresh(worker);
        } else if self.workspace.values.is_none()
            && self.workspace.values_reply.is_none()
            && self.workspace.values_error.is_none()
        {
            self.workspace.refresh_values(worker);
        }
        control_row(ui);
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.workspace.showing_values, "规则")
                .clicked()
            {
                self.workspace.showing_values = false;
            }
            if ui
                .selectable_label(self.workspace.showing_values, "Values")
                .clicked()
            {
                self.workspace.showing_values = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("刷新").clicked() {
                    self.refresh(worker);
                }
            });
        });
        ui.separator();

        if sidebar_visible {
            let colors = Theme::current_palette(ui.ctx());
            egui::Panel::left(ui.make_persistent_id("workspace_list"))
                .default_size(240.0)
                .min_size(150.0)
                .max_size(360.0)
                .resizable(true)
                .frame(egui::Frame::new().fill(colors.side).inner_margin(10))
                .show(ui, |ui| {
                    if self.workspace.showing_values {
                        self.workspace.value_list(ui);
                    } else {
                        self.workspace_rule_list(ui, worker);
                    }
                });
        }
        ui.add_space(5.0);
        self.workspace
            .sync_selection(self.rules.as_deref().unwrap_or_default());
        let can_save = self.workspace_can_save();
        let keyboard_save = can_save
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        let save = if self.workspace.showing_values {
            self.workspace.value_content(
                ui,
                worker,
                self.rules.as_deref().unwrap_or_default(),
                can_save,
            )
        } else {
            self.workspace_rule_content(ui, worker, can_save)
        };
        if save || keyboard_save {
            self.save_workspace(worker);
        }
    }

    fn workspace_rule_list(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        let workspace = &mut self.workspace;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.rules.is_some() && self.source_error.is_none(),
                    egui::Button::new("＋ 新建"),
                )
                .clicked()
            {
                let mut value = new_rule(RuleCategory::All, "");
                value.as_object_mut().unwrap().remove("mock");
                let id = label(&value["id"]).to_owned();
                workspace.rule_search.clear();
                workspace.rule_id = Some(id.clone());
                workspace
                    .rule_drafts
                    .insert(id, RuleEditor::new(value, None));
            }
            search_box(ui, &mut workspace.rule_search, "搜索规则");
        });
        ui.separator();
        let Some(rules) = &self.rules else {
            if self.rules_reply.is_some() {
                ui.spinner();
            }
            return;
        };
        let search = workspace.rule_search.to_lowercase();
        let mut groups = Vec::<(&str, Vec<&Value>)>::new();
        for rule in rules {
            if ![&rule["name"], &rule["folder"], &rule["match"]["url"]]
                .into_iter()
                .any(|field| label(field).to_lowercase().contains(&search))
            {
                continue;
            }
            let folder = label(&rule["folder"]);
            if let Some((_, entries)) = groups.iter_mut().find(|(name, _)| *name == folder) {
                entries.push(rule);
            } else {
                groups.push((folder, vec![rule]));
            }
        }
        let mut toggle = None;
        let ready = self.rules_reply.is_none() && self.source_error.is_none();
        egui::ScrollArea::vertical()
            .id_salt("workspace_rules")
            .show(ui, |ui| {
                for (id, draft) in &workspace.rule_drafts {
                    if (draft.baseline.is_none() || !rules.iter().any(|rule| rule["id"] == *id))
                        && draft.workspace_name().to_lowercase().contains(&search)
                        && crate::workbench::sidebar_row(
                            ui,
                            format!("{} *", draft.workspace_name()),
                            Some(workspace.rule_id.as_ref() == Some(id)),
                        )
                        .clicked()
                    {
                        workspace.rule_id = Some(id.clone());
                    }
                }
                for (folder, entries) in groups {
                    let name = if folder.is_empty() {
                        "未分组"
                    } else {
                        folder
                    };
                    let selected = entries
                        .iter()
                        .any(|rule| rule["id"].as_str() == workspace.rule_id.as_deref());
                    let mut header = egui::CollapsingHeader::new(name)
                        .id_salt(("rule_group", folder))
                        .default_open(true);
                    if selected || !search.is_empty() {
                        header = header.open(Some(true));
                    }
                    header.show(ui, |ui| {
                        for rule in entries {
                            let id = label(&rule["id"]);
                            ui.push_id(id, |ui| {
                                ui.horizontal_top(|ui| {
                                    let old = rule["enabled"] != false;
                                    let mut enabled = old;
                                    let title = label(&rule["name"]);
                                    let checkbox = ui.add_enabled(
                                        ready,
                                        egui::Checkbox::without_text(&mut enabled),
                                    );
                                    checkbox.widget_info(|| {
                                        egui::WidgetInfo::selected(
                                            egui::WidgetType::Checkbox,
                                            ready,
                                            enabled,
                                            format!("启用 {title}"),
                                        )
                                    });
                                    if checkbox.changed() {
                                        toggle = Some((id.to_owned(), old, enabled));
                                    }
                                    ui.vertical(|ui| {
                                        let dirty = workspace
                                            .rule_drafts
                                            .get(id)
                                            .is_some_and(RuleEditor::is_dirty);
                                        let title = if dirty {
                                            format!("{title} *")
                                        } else {
                                            title.to_owned()
                                        };
                                        if crate::workbench::sidebar_row(
                                            ui,
                                            title,
                                            Some(workspace.rule_id.as_deref() == Some(id)),
                                        )
                                        .clicked()
                                        {
                                            workspace.rule_id = Some(id.to_owned());
                                        }
                                        let tags = rule_tags(rule);
                                        if !tags.is_empty() {
                                            ui.weak(tags);
                                        }
                                    });
                                });
                            });
                        }
                    });
                }
            });
        if let Some((id, old, enabled)) = toggle {
            self.toggle_rule(worker, id, old, enabled);
        }
    }

    fn workspace_rule_content(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        can_save: bool,
    ) -> bool {
        for error in [&self.source_error, &self.error].into_iter().flatten() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(error) = self
            .workspace
            .values_source_error
            .as_deref()
            .or(self.workspace.values_error.as_deref())
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let Some(id) = self.workspace.rule_id.clone() else {
            if self.rules.is_some() {
                ui.weak("选择规则或新建规则");
            }
            return false;
        };
        let Some(editor) = self.workspace.rule_drafts.get_mut(&id) else {
            return false;
        };
        let saving = matches!(&self.rules_reply, Some((RuleReply::SaveWorkspace(active), _)) if active == &id);
        let mut discard = false;
        let mut save = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save = save_button(ui, can_save, saving);
                if editor.is_dirty() || editor.baseline.is_none() || editor.error.is_some() {
                    discard = ui
                        .add_enabled(!saving, egui::Button::new("放弃草稿"))
                        .clicked();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(editor.workspace_name()).heading())
                            .truncate(),
                    );
                });
            });
        });
        if let Some(error) = &editor.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.separator();
        let jump = egui::ScrollArea::vertical()
            .id_salt(("workspace_rule", &id))
            .show(ui, |ui| {
                ui.add_enabled_ui(!saving, |ui| {
                    editor.workspace_form(
                        ui,
                        worker,
                        self.workspace.values.as_deref().unwrap_or_default(),
                    )
                })
                .inner
            })
            .inner;
        if discard {
            self.workspace.rule_drafts.remove(&id);
            self.workspace
                .sync_selection(self.rules.as_deref().unwrap_or_default());
        }
        if let Some(value) = jump {
            self.workspace.showing_values = true;
            self.workspace.return_rule = Some(id);
            self.workspace.value_search.clear();
            self.workspace.value_id = Some(value);
        }
        save
    }
}

impl Workspace {
    pub(super) fn apply_rule_toggle(&mut self, previous: &Value, reply: &Value) {
        if let Some(draft) = self.rule_drafts.get_mut(label(&previous["id"]))
            && let Some(enabled) = reply["enabled"].as_bool()
        {
            draft.apply_enabled_change(previous, enabled);
        }
    }

    pub(super) fn load_editor_values(&mut self, worker: &Worker, editor_open: bool) {
        if editor_open && self.values.is_none() && self.values_error.is_none() {
            self.refresh_values(worker);
        }
    }

    pub(super) fn refresh_values(&mut self, worker: &Worker) {
        if self.values_reply.is_none() {
            self.send_values(worker, "GET", Value::Null, ValueReply::Load);
        }
    }

    fn send_values(&mut self, worker: &Worker, method: &str, body: Value, reply: ValueReply) {
        match worker.request(method, "/rules/values", body) {
            Ok(request) => self.values_reply = Some((reply, request)),
            Err(error) => self.values_error = Some(error),
        }
    }

    pub(super) fn poll(&mut self) {
        if let Some((kind, result)) = self.values_reply.as_mut().and_then(|(kind, reply)| {
            finished(reply).map(|result| (std::mem::replace(kind, ValueReply::Load), result))
        }) {
            self.values_reply = None;
            match result {
                Ok(mut reply) => {
                    self.values_error = None;
                    self.values_source_error = reply["error"].as_str().map(str::to_owned);
                    self.values = Some(items(&mut reply, "values"));
                    match kind {
                        ValueReply::Save(id) => {
                            self.value_drafts.remove(&id);
                        }
                        ValueReply::Delete(id) => {
                            self.value_drafts.remove(&id);
                            if self.value_id.as_ref() == Some(&id) {
                                self.value_id = None;
                            }
                        }
                        ValueReply::Load => {}
                    }
                }
                Err(error) => self.values_error = Some(error),
            }
        }
        for draft in self.rule_drafts.values_mut() {
            draft.poll();
        }
    }

    pub(super) fn finish_rule_save(&mut self, id: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.rule_drafts.remove(id);
            }
            Err(error) => {
                if let Some(editor) = self.rule_drafts.get_mut(id) {
                    editor.error = Some(error);
                }
            }
        }
    }

    fn sync_selection(&mut self, rules: &[Value]) {
        self.rule_drafts.retain(|id, draft| {
            self.rule_id.as_ref() == Some(id) || draft.is_dirty() || draft.baseline.is_none()
        });
        self.value_drafts.retain(|id, draft| {
            self.value_id.as_ref() == Some(id) || draft.dirty || draft.baseline.is_none()
        });
        if self.showing_values {
            let Some(values) = &self.values else { return };
            let Some(id) = selection(&mut self.value_id, values, &self.value_drafts) else {
                return;
            };
            if let Some(value) = values.iter().find(|value| value["id"] == id)
                && self.value_drafts.get(id).is_none_or(|draft| {
                    !draft.dirty && !draft.delete && draft.baseline.as_ref() != Some(value)
                })
            {
                self.value_drafts.insert(
                    id.to_owned(),
                    ValueDraft::new(value.clone(), Some(value.clone())),
                );
            }
        } else {
            let Some(id) = selection(&mut self.rule_id, rules, &self.rule_drafts) else {
                return;
            };
            if let Some(rule) = rules.iter().find(|rule| rule["id"] == id)
                && self.rule_drafts.get(id).is_none_or(|draft| {
                    !draft.is_dirty()
                        && draft.error.is_none()
                        && draft.baseline.as_ref() != Some(rule)
                })
            {
                self.rule_drafts.insert(
                    id.to_owned(),
                    RuleEditor::new(rule.clone(), Some(rule.clone())),
                );
            }
        }
    }

    fn value_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.values.is_some() && self.values_source_error.is_none(),
                    egui::Button::new("＋ 新建"),
                )
                .clicked()
            {
                let id = uuid::Uuid::new_v4().to_string();
                let draft = ValueDraft::new(
                    json!({"id": id, "name": "新建 Value", "kind": "text", "text": ""}),
                    None,
                );
                self.value_search.clear();
                self.value_id = Some(id.clone());
                self.value_drafts.insert(id, draft);
            }
            search_box(ui, &mut self.value_search, "搜索 Values");
        });
        ui.separator();
        let Some(values) = &self.values else {
            if self.values_reply.is_some() {
                ui.spinner();
            }
            return;
        };
        let search = self.value_search.to_lowercase();
        let drafts = self
            .value_drafts
            .values()
            .filter(|draft| {
                draft.baseline.is_none()
                    || !values.iter().any(|value| value["id"] == draft.value["id"])
            })
            .map(|draft| &draft.value);
        egui::ScrollArea::vertical()
            .id_salt("workspace_values")
            .show(ui, |ui| {
                for value in drafts.chain(values) {
                    let id = label(&value["id"]);
                    let draft = self.value_drafts.get(id);
                    let name = label(&draft.map_or(value, |draft| &draft.value)["name"]);
                    if !name.to_lowercase().contains(&search) {
                        continue;
                    }
                    let text = if draft.is_some_and(|draft| draft.dirty || draft.baseline.is_none())
                    {
                        format!("{name} *")
                    } else {
                        name.to_owned()
                    };
                    if crate::workbench::sidebar_row(
                        ui,
                        text,
                        Some(self.value_id.as_deref() == Some(id)),
                    )
                    .clicked()
                    {
                        self.value_id = Some(id.to_owned());
                        self.values_error = None;
                    }
                }
            });
    }

    fn value_content(
        &mut self,
        ui: &mut egui::Ui,
        worker: &Worker,
        rules: &[Value],
        can_save: bool,
    ) -> bool {
        if let Some(rule) = &self.return_rule
            && ui.link("← 返回规则").clicked()
        {
            self.showing_values = false;
            self.rule_id = Some(rule.clone());
            self.return_rule = None;
            return false;
        }
        for error in [&self.values_source_error, &self.values_error]
            .into_iter()
            .flatten()
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let Some(id) = self.value_id.clone() else {
            if self.values.is_some() {
                ui.weak("选择 Value 或新建 Value");
            }
            return false;
        };
        let Some(draft) = self.value_drafts.get_mut(&id) else {
            return false;
        };
        let saving = matches!(&self.values_reply, Some((ValueReply::Save(active) | ValueReply::Delete(active), _)) if active == &id);
        let refs = rules
            .iter()
            .filter(|rule| references_value(rule, &id))
            .collect::<Vec<_>>();
        let mut save = false;
        let mut delete = false;
        let mut discard = false;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!saving, |ui| {
                ui.label("名称");
                if let Value::String(name) = &mut draft.value["name"] {
                    draft.dirty |= ui
                        .add(egui::TextEdit::singleline(name).desired_width(160.0))
                        .changed();
                }
                ui.label("类型");
                if let Value::String(kind) = &mut draft.value["kind"] {
                    egui::ComboBox::from_id_salt(("value_kind", &id))
                        .selected_text(kind_name(kind))
                        .show_ui(ui, |ui| {
                            for key in ["text", "json", "javascript", "python"] {
                                draft.dirty |= ui
                                    .selectable_value(kind, key.into(), kind_name(key))
                                    .changed();
                            }
                        });
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save = save_button(ui, can_save, saving);
                let can_delete = self.values_reply.is_none()
                    && self.values_source_error.is_none()
                    && refs.is_empty();
                if draft.baseline.is_some()
                    && ui
                        .add_enabled(can_delete && !draft.delete, egui::Button::new("删除"))
                        .on_disabled_hover_text("请先解除该 Value 的引用")
                        .clicked()
                {
                    draft.delete = true;
                }
                if draft.dirty || draft.baseline.is_none() {
                    discard = ui
                        .add_enabled(!saving, egui::Button::new("放弃草稿"))
                        .clicked();
                }
            });
        });
        if draft.delete {
            ui.horizontal(|ui| {
                ui.label(format!("删除「{}」？", label(&draft.value["name"])));
                delete = ui
                    .add_enabled(
                        self.values_reply.is_none() && refs.is_empty(),
                        egui::Button::new("确认删除"),
                    )
                    .clicked();
                if ui.button("取消").clicked() {
                    draft.delete = false;
                }
            });
        }
        ui.separator();
        egui::Panel::bottom(ui.make_persistent_id("workspace_value_refs")).show(ui, |ui| {
            ui.strong(format!("被 {} 条规则引用", refs.len()));
            egui::ScrollArea::vertical()
                .max_height(95.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for rule in refs {
                            if ui.link(label(&rule["name"])).clicked() {
                                self.showing_values = false;
                                self.rule_search.clear();
                                self.rule_id = rule["id"].as_str().map(str::to_owned);
                            }
                        }
                    });
                });
        });
        if let Value::String(text) = &mut draft.value["text"] {
            let height = ui.available_height();
            egui::ScrollArea::vertical()
                .id_salt(("workspace_value", &id))
                .show(ui, |ui| {
                    let editor = egui::TextEdit::multiline(text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, height));
                    let response = ui.add_enabled(!saving, editor);
                    response
                        .ctx
                        .accesskit_node_builder(response.id, |node| node.set_label("Value 内容"));
                    draft.dirty |= response.changed();
                });
        }
        if discard {
            self.value_drafts.remove(&id);
            self.values_error = None;
        }
        if delete {
            self.write_value(worker, true);
        }
        save
    }

    fn write_value(&mut self, worker: &Worker, delete: bool) {
        let (Some(id), Some(current)) = (self.value_id.clone(), &self.values) else {
            return;
        };
        let Some(draft) = self.value_drafts.get_mut(&id) else {
            return;
        };
        let value = (!delete).then(|| draft.value.clone());
        let Some(next) = replace_record(current, &id, draft.baseline.as_ref(), value) else {
            self.values_error =
                Some("Value 已更新或删除。草稿仍在编辑器中，可复制内容后重新载入。".into());
            return;
        };
        draft.delete = false;
        let kind = if delete {
            ValueReply::Delete(id)
        } else {
            ValueReply::Save(id)
        };
        self.send_values(
            worker,
            "PUT",
            json!({"values": next, "expected": current}),
            kind,
        );
    }
}

fn selection<'a, T>(
    selected: &'a mut Option<String>,
    rows: &[Value],
    drafts: &BTreeMap<String, T>,
) -> Option<&'a str> {
    if selected
        .as_ref()
        .is_none_or(|id| !drafts.contains_key(id) && !rows.iter().any(|row| row["id"] == *id))
    {
        *selected = rows
            .first()
            .and_then(|row| row["id"].as_str())
            .map(str::to_owned);
    }
    selected.as_deref()
}

fn replace_record(
    rows: &[Value],
    id: &str,
    baseline: Option<&Value>,
    value: Option<Value>,
) -> Option<Vec<Value>> {
    let position = rows.iter().position(|row| row["id"] == id);
    if position.map(|index| &rows[index]) != baseline {
        return None;
    }
    let mut next = rows.to_vec();
    match (position, value) {
        (Some(index), Some(value)) => next[index] = value,
        (Some(index), None) => {
            next.remove(index);
        }
        (None, Some(value)) => next.push(value),
        (None, None) => return None,
    }
    Some(next)
}

fn references_value(rule: &Value, id: &str) -> bool {
    ["request", "response", "mock"].iter().any(|phase| {
        ["bodyValue", "scriptValue", "pythonValue"]
            .iter()
            .any(|key| rule[*phase][*key] == id)
    })
}

fn rule_tags(rule: &Value) -> String {
    let mut tags = Vec::new();
    if rule["request"]["redirect"].is_string() {
        tags.push("URL 重定向");
    }
    if rule.get("mock").is_some()
        || ["request", "response"].iter().any(|phase| {
            rule[*phase].as_object().is_some_and(|action| {
                action.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "redirect"
                            | "breakpoint"
                            | "script"
                            | "scriptValue"
                            | "python"
                            | "pythonValue"
                    )
                })
            })
        })
    {
        tags.push("重写");
    }
    if RuleCategory::Script.matches(rule) {
        tags.push("脚本");
    }
    if RuleCategory::Breakpoint.matches(rule) {
        tags.push("断点");
    }
    tags.join(" / ")
}

fn search_box(ui: &mut egui::Ui, text: &mut String, hint: &'static str) {
    let colors = Theme::current_palette(ui.ctx());
    let mut frame = colors.input_frame().begin(ui);
    let width = frame.content_ui.available_width();
    let response = frame.content_ui.add(input_text(text, hint, width));
    response
        .ctx
        .accesskit_node_builder(response.id, |node| node.set_label(hint));
    frame.frame.stroke = colors.input_stroke(&response);
    frame.end(ui);
}

fn save_button(ui: &mut egui::Ui, can_save: bool, saving: bool) -> bool {
    let colors = Theme::current_palette(ui.ctx());
    let label = if saving { "正在保存…" } else { "保存" };
    ui.add_enabled(can_save, colors.dialog_action(label, colors.accent))
        .clicked()
}

fn kind_name(kind: &str) -> &str {
    match kind {
        "text" => "文本",
        "json" => "JSON",
        "javascript" => "JavaScript",
        "python" => "Python",
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_workspace_renders_and_follows_a_value_reference() {
        fn text_positions(output: &egui::FullOutput, expected: &str) -> Vec<egui::Pos2> {
            output
                .shapes
                .iter()
                .filter_map(|shape| {
                    let egui::Shape::Text(text) = &shape.shape else {
                        return None;
                    };
                    let rect = text.visual_bounding_rect();
                    (text.galley.text() == expected && shape.clip_rect.intersects(rect))
                        .then(|| rect.center())
                })
                .collect()
        }

        let ctx = egui::Context::default();
        Theme::default().apply(&ctx);
        let directory = tempfile::tempdir().unwrap();
        let worker = Worker::start(directory.path().into(), Some(0), ctx.clone());
        let mut rules_ui = RulesUi {
            rules: Some(vec![json!({
                "id": "rule", "name": "Example Rewrite", "enabled": true, "folder": "Fixtures",
                "match": { "url": "https://example.test/fixture", "method": "GET" },
                "response": { "mode": "replace", "status": 403, "bodyValue": "body", "script": "response.status = 403;" }
            })]),
            workspace: Workspace {
                values: Some(vec![
                    json!({"id": "body", "name": "Fixture Body", "kind": "text", "text": "shared fixture response"}),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 1000.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| rules_ui.workspace(ui, &worker, true),
            );
            output.textures_delta.clear();
            output
        };
        let click = |frame: &mut dyn FnMut(Vec<egui::Event>) -> egui::FullOutput, pos| {
            frame(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                frame(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
        };

        let rules = frame(vec![]);
        let folder = text_positions(&rules, "Fixtures")[0];
        let url = text_positions(&rules, "https://example.test/fixture")[0];
        assert!(folder.x < url.x);
        assert!(!text_positions(&rules, "response.status = 403;").is_empty());
        click(&mut frame, text_positions(&rules, "Values")[0]);

        let values = frame(vec![]);
        assert!(!text_positions(&values, "shared fixture response").is_empty());
        let rule_reference = text_positions(&values, "Example Rewrite")[0];
        click(&mut frame, rule_reference);
        assert!(!text_positions(&frame(vec![]), "https://example.test/fixture").is_empty());
    }

    #[test]
    fn concurrent_record_changes_do_not_replace_a_draft_baseline() {
        let original = json!({"id": "one", "name": "original"});
        let changed = json!({"id": "one", "name": "changed"});
        let unrelated = json!({"id": "two", "name": "updated"});
        let edited = json!({"id": "one", "name": "edited"});
        assert!(replace_record(&[changed], "one", Some(&original), Some(edited.clone())).is_none());
        let result = replace_record(
            &[original.clone(), unrelated.clone()],
            "one",
            Some(&original),
            Some(edited.clone()),
        )
        .unwrap();
        assert_eq!(result, [edited, unrelated]);
    }
}
