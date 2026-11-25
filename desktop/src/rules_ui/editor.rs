use super::breakpoint::{header_lines, text_area};
use super::{RequestHandle, RuleCategory, Value, Worker, egui, finished, json, label};
use crate::theme::Theme;
use recho_core::native::parse_header_lines;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    Redirect,
    Replace,
    Modify,
    Breakpoint,
    Python,
    JavaScript,
    Other,
}

impl Action {
    fn of(value: &Value) -> Self {
        if !value.is_object() {
            return Self::None;
        }
        if value["breakpoint"] == true {
            return Self::Breakpoint;
        }
        for (key, kind) in [
            ("redirect", Self::Redirect),
            ("python", Self::Python),
            ("script", Self::JavaScript),
        ] {
            if value.get(key).is_some() {
                return kind;
            }
        }
        match value["mode"].as_str() {
            Some("replace") => Self::Replace,
            Some("modify") => Self::Modify,
            _ if value.get("edits").is_some() => Self::Modify,
            _ => Self::Other,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "无",
            Self::Redirect => "重定向 URL",
            Self::Replace => "替换",
            Self::Modify => "修改",
            Self::Breakpoint => "断点",
            Self::Python => "Python 脚本",
            Self::JavaScript => "JavaScript 脚本",
            Self::Other => "高级修改",
        }
    }

    fn create(self, phase: &str) -> Value {
        match self {
            Self::None => Value::Null,
            Self::Redirect => json!({"redirect": ""}),
            Self::Replace => json!({"mode": "replace", "body": ""}),
            Self::Modify => json!({"mode": "modify", "edits": []}),
            Self::Breakpoint => json!({"breakpoint": true}),
            Self::Python if phase == "request" => {
                json!({"python": "def onRequest(context, request):\n    return request\n"})
            }
            Self::Python => {
                json!({"python": "def onResponse(context, response):\n    return response\n"})
            }
            Self::JavaScript => json!({"script": "// Edit request or response here.\n"}),
            Self::Other => json!({}),
        }
    }
}

pub(super) fn action_name(value: &Value) -> &'static str {
    Action::of(value).name()
}

fn default_mock() -> Value {
    json!({"status": 200, "headers": {"content-type": "text/plain"}, "body": ""})
}

pub(super) fn new_rule(category: RuleCategory, folder: &str) -> Value {
    let mut rule = json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "name": "新建规则",
        "enabled": true,
        "match": {"url": "", "method": "*"},
    });
    if !folder.is_empty() {
        rule["folder"] = json!(folder);
    }
    match category {
        RuleCategory::Script => rule["request"] = Action::Python.create("request"),
        RuleCategory::Breakpoint => rule["request"] = Action::Breakpoint.create("request"),
        _ => rule["mock"] = default_mock(),
    }
    rule
}

pub(super) struct RuleEditor {
    pub baseline: Option<Value>,
    pub error: Option<String>,
    draft: Value,
    excludes: String,
    actions: [ActionEditor; 3],
    tab: usize,
    advanced: Option<String>,
    dirty: bool,
    cancel: bool,
}

pub(super) struct DialogState<'a> {
    pub can_save: bool,
    pub saving: bool,
    pub keyboard: bool,
    pub source_error: Option<&'a str>,
}

impl RuleEditor {
    pub fn new(mut value: Value, baseline: Option<Value>) -> Self {
        let excludes = value["match"]["exclude"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        let tab = if value["request"].is_object() {
            0
        } else if value["response"].is_object() {
            1
        } else {
            2
        };
        Self {
            baseline,
            error: None,
            actions: ["request", "response", "mock"]
                .map(|phase| ActionEditor::new(value[phase].take())),
            draft: value,
            excludes,
            tab,
            advanced: None,
            dirty: false,
            cancel: false,
        }
    }

    pub fn poll(&mut self) {
        for action in &mut self.actions {
            if let Some(result) = action.preview_reply.as_mut().and_then(finished) {
                action.preview_reply = None;
                action.preview = Some(result.map(|value| label(&value["result"]).to_owned()));
            }
            if let Some(result) = action.file_pick.as_mut().and_then(finished) {
                action.file_pick = None;
                match result {
                    Ok(Some(path)) => {
                        if let Some(path) = path.to_str() {
                            action.value["bodyFile"] = json!(path);
                            self.dirty = true;
                        } else {
                            self.error = Some("文件路径包含无法保存的字符。".into());
                        }
                    }
                    Ok(None) => {}
                    Err(error) => self.error = Some(error),
                }
            }
        }
    }

    fn value(&self) -> Result<Value, String> {
        let mut value = self.draft.clone();
        let object = value.as_object_mut().expect("Rule draft is an object");
        for (phase, editor) in ["request", "response", "mock"]
            .into_iter()
            .zip(&self.actions)
        {
            if let Some(action) = editor.value()? {
                object.insert(phase.into(), action);
            } else {
                object.remove(phase);
            }
        }
        value["match"]["exclude"] = json!(
            self.excludes
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
        );
        Ok(value)
    }

    fn read_advanced(&self) -> Result<Value, String> {
        let Some(text) = &self.advanced else {
            return self.value();
        };
        let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        if !value.is_object()
            || !value["match"].is_object()
            || ["request", "response", "mock"]
                .into_iter()
                .any(|phase| value.get(phase).is_some_and(|item| !item.is_object()))
        {
            return Err("规则、匹配条件与已配置的行为必须是 JSON 对象。".into());
        }
        if value["id"] != self.draft["id"] {
            return Err("编辑规则时不能更改 ID。".into());
        }
        Ok(value)
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        worker: &Worker,
        state: DialogState<'_>,
        close: &mut bool,
        refresh: &mut bool,
        result: &mut Option<Value>,
    ) {
        let mut switch_mode = false;
        let mut save = false;
        let mut cancel = false;
        let reply = Theme::modal(ctx, egui::Id::new("rule_editor")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 52.0).min(800.0));
            ui.heading(if self.baseline.is_some() {
                "编辑规则"
            } else {
                "新建规则"
            });
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 190.0).max(220.0))
                .show(ui, |ui| {
                    ui.add_enabled_ui(!state.saving, |ui| {
                        if let Some(text) = &mut self.advanced {
                            self.dirty |= text_area(ui, text, 24);
                        } else {
                            self.form(ui, worker);
                        }
                    });
                });
            ui.separator();
            if let Some(error) = self.error.as_deref().or(state.source_error) {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if self.cancel {
                ui.label("放弃未保存的修改？");
                ui.horizontal(|ui| {
                    if ui.button("继续编辑").clicked() {
                        self.cancel = false;
                    }
                    if ui.button("放弃修改").clicked() {
                        *close = true;
                    }
                });
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(!state.saving, |ui| {
                    switch_mode = ui
                        .button(if self.advanced.is_some() {
                            "返回表单"
                        } else {
                            "高级 JSON"
                        })
                        .clicked();
                    *refresh = ui.button("刷新规则列表").clicked();
                    cancel = ui.button("取消").clicked();
                });
                save = ui
                    .add_enabled(
                        state.can_save,
                        egui::Button::new(if state.saving {
                            "正在保存…"
                        } else {
                            "保存规则"
                        }),
                    )
                    .clicked();
                if state.can_save && state.keyboard {
                    save |= ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::COMMAND, egui::Key::S)
                    });
                }
            });
        });
        if switch_mode {
            if self.advanced.is_some() {
                match self.read_advanced() {
                    Ok(value) => {
                        let dirty = self.dirty;
                        *self = Self::new(value, self.baseline.take());
                        self.dirty = dirty;
                    }
                    Err(error) => self.error = Some(error),
                }
            } else {
                match self.value().and_then(|value| {
                    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
                }) {
                    Ok(text) => self.advanced = Some(text),
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if save {
            match self.read_advanced() {
                Ok(value) => *result = Some(value),
                Err(error) => self.error = Some(error),
            }
        }
        if !state.saving && (cancel || reply.should_close()) {
            if self.dirty {
                self.cancel = true;
            } else {
                *close = true;
            }
        }
    }

    fn form(&mut self, ui: &mut egui::Ui, worker: &Worker) {
        self.dirty |= string_field(ui, &mut self.draft, "name", "名称", None);
        self.dirty |= string_field(ui, &mut self.draft, "folder", "文件夹", None);
        let mut enabled = self.draft["enabled"] != false;
        if ui.checkbox(&mut enabled, "启用规则").changed() {
            self.draft["enabled"] = json!(enabled);
            self.dirty = true;
        }
        ui.separator();
        ui.strong("匹配条件");
        self.dirty |= string_field(ui, &mut self.draft["match"], "method", "方法", Some("*"));
        self.dirty |= string_field(
            ui,
            &mut self.draft["match"],
            "url",
            "URL",
            Some("http://localhost:8080/*"),
        );
        ui.weak("* 匹配任意字符，? 匹配单字符");
        ui.label("排除 URL（每行一个）");
        self.dirty |= text_area(ui, &mut self.excludes, 2);
        ui.separator();
        ui.horizontal(|ui| {
            for (index, phase) in ["请求", "响应", "本地响应"].into_iter().enumerate() {
                if ui.selectable_label(self.tab == index, phase).clicked() {
                    self.tab = index;
                }
            }
        });
        ui.push_id(self.tab, |ui| {
            let phase = ["request", "response", "mock"][self.tab];
            let action = &mut self.actions[self.tab];
            self.dirty |= action.show(ui, worker, phase);
        });
    }
}

struct ActionEditor {
    value: Value,
    headers: Option<String>,
    edit_index: usize,
    sample: String,
    preview: Option<Result<String, String>>,
    preview_reply: Option<RequestHandle>,
    file_pick: Option<crate::files::FilePick>,
}

impl ActionEditor {
    fn new(value: Value) -> Self {
        Self {
            value,
            headers: None,
            edit_index: 0,
            sample: String::new(),
            preview: None,
            preview_reply: None,
            file_pick: None,
        }
    }

    fn value(&self) -> Result<Option<Value>, String> {
        if !self.value.is_object() {
            return Ok(None);
        }
        let mut value = self.value.clone();
        if let Some(headers) = &self.headers {
            value["headers"] = parse_header_lines(headers)?;
        }
        Ok(Some(value))
    }

    fn show(&mut self, ui: &mut egui::Ui, worker: &Worker, phase: &str) -> bool {
        if phase == "mock" {
            let old = self.value.is_object();
            let mut enabled = old;
            let changed = ui.checkbox(&mut enabled, "启用本地响应").changed();
            if changed {
                *self = Self::new(if enabled { default_mock() } else { Value::Null });
            }
            if enabled {
                return self.replacement(ui, phase) | changed;
            }
            return changed;
        }
        let selected = Action::of(&self.value);
        let mut kind = selected;
        egui::ComboBox::from_label("行为")
            .selected_text(kind.name())
            .show_ui(ui, |ui| {
                for choice in [
                    Action::None,
                    Action::Redirect,
                    Action::Replace,
                    Action::Modify,
                    Action::Breakpoint,
                    Action::Python,
                    Action::JavaScript,
                ] {
                    if choice != Action::Redirect || phase == "request" {
                        ui.selectable_value(&mut kind, choice, choice.name());
                    }
                }
            });
        let changed = selected != kind;
        if changed {
            *self = Self::new(kind.create(phase));
        }
        let fields_changed = match kind {
            Action::Redirect => string_field(
                ui,
                &mut self.value,
                "redirect",
                "目标 URL",
                Some("http://localhost:8080/"),
            ),
            Action::Replace => self.replacement(ui, phase),
            Action::Modify => self.modifier(ui, worker, phase),
            Action::Python => multiline_field(ui, &mut self.value, "python", "Python 源码", 8),
            Action::JavaScript => {
                multiline_field(ui, &mut self.value, "script", "JavaScript 源码", 8)
            }
            Action::Other => {
                ui.weak("通过高级 JSON 编辑此行为。");
                false
            }
            _ => false,
        };
        changed || fields_changed
    }

    fn replacement(&mut self, ui: &mut egui::Ui, phase: &str) -> bool {
        let mut changed = false;
        if phase == "request" {
            changed |= string_field(
                ui,
                &mut self.value,
                "method",
                "替换方法",
                Some("保留原方法"),
            );
            changed |= string_field(ui, &mut self.value, "path", "替换路径", Some("保留原路径"));
            changed |= bool_field(ui, &mut self.value, "preserveHost", "保留原 Host", false);
        } else if optional(
            ui,
            &mut self.value,
            "status",
            "设置状态码",
            json!(200),
            phase == "mock",
            &mut changed,
        ) {
            let mut status = self.value["status"].as_u64().unwrap_or(200) as u16;
            if ui
                .add(egui::DragValue::new(&mut status).range(200..=599))
                .changed()
            {
                self.value["status"] = json!(status);
                changed = true;
            }
        }
        let headers_enabled = optional(
            ui,
            &mut self.value,
            "headers",
            "设置消息头",
            json!({}),
            phase == "mock",
            &mut changed,
        );
        if headers_enabled {
            if let Some(draft) = &mut self.headers {
                changed |= text_area(ui, draft, 4);
            } else {
                let mut draft = header_lines(&self.value["headers"]);
                if text_area(ui, &mut draft, 4) {
                    self.headers = Some(draft);
                    changed = true;
                }
            }
        } else {
            self.headers = None;
        }
        let mut body_kind = if self.value.get("bodyFile").is_some() {
            "bodyFile"
        } else if self.value.get("bodyBase64").is_some() {
            "bodyBase64"
        } else if self.value.get("body").is_some() {
            "body"
        } else {
            ""
        };
        let old = body_kind;
        let kinds = [
            ("", "保留原正文"),
            ("body", "文本正文"),
            ("bodyBase64", "Base64 正文"),
            ("bodyFile", "正文文件"),
        ];
        egui::ComboBox::from_label("正文")
            .selected_text(kinds.iter().find(|(key, _)| *key == body_kind).unwrap().1)
            .show_ui(ui, |ui| {
                for (key, caption) in kinds {
                    if if phase == "mock" {
                        key == "body" || key == "bodyBase64"
                    } else {
                        key != "bodyBase64"
                    } {
                        ui.selectable_value(&mut body_kind, key, caption);
                    }
                }
            });
        if old != body_kind {
            for (key, _) in kinds {
                if !key.is_empty() {
                    self.value.as_object_mut().unwrap().remove(key);
                }
            }
            if !body_kind.is_empty() {
                self.value[body_kind] = json!("");
            }
            changed = true;
        }
        match body_kind {
            "body" | "bodyBase64" => {
                changed |= multiline_field(ui, &mut self.value, body_kind, "", 6);
            }
            "bodyFile" => {
                changed |= string_field(
                    ui,
                    &mut self.value,
                    "bodyFile",
                    "文件路径",
                    Some("绝对路径"),
                );
                if ui
                    .add_enabled(self.file_pick.is_none(), egui::Button::new("选择文件"))
                    .clicked()
                {
                    self.file_pick = Some(crate::files::pick("", false, ui.ctx()));
                }
            }
            _ => {}
        }
        changed
    }

    fn modifier(&mut self, ui: &mut egui::Ui, worker: &Worker, phase: &str) -> bool {
        const EDITS: [(&str, &str); 7] = [
            ("body", "修改正文"),
            ("queryAdd", "添加查询参数"),
            ("queryReplace", "替换查询参数"),
            ("queryRemove", "移除查询参数"),
            ("headerAdd", "添加消息头"),
            ("headerReplace", "替换消息头"),
            ("headerRemove", "移除消息头"),
        ];
        if self.value.get("edits").is_none() {
            self.value["edits"] = json!([]);
        }
        let edits = self.value["edits"].as_array_mut().unwrap();
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong("修改动作");
            if ui.button("添加修改").clicked() {
                edits.push(json!({"type": "body", "search": "", "replacement": ""}));
                self.edit_index = edits.len() - 1;
                changed = true;
            }
            if ui
                .add_enabled(self.edit_index > 0, egui::Button::new("上移"))
                .clicked()
            {
                edits.swap(self.edit_index, self.edit_index - 1);
                self.edit_index -= 1;
                changed = true;
            }
            if ui
                .add_enabled(self.edit_index + 1 < edits.len(), egui::Button::new("下移"))
                .clicked()
            {
                edits.swap(self.edit_index, self.edit_index + 1);
                self.edit_index += 1;
                changed = true;
            }
            if ui
                .add_enabled(!edits.is_empty(), egui::Button::new("删除修改"))
                .clicked()
            {
                edits.remove(self.edit_index);
                self.edit_index = self.edit_index.min(edits.len().saturating_sub(1));
                changed = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (index, edit) in edits.iter().enumerate() {
                let kind = label(&edit["type"]);
                let name = EDITS
                    .iter()
                    .find(|(key, _)| *key == kind)
                    .map_or(kind, |(_, name)| name);
                if ui
                    .selectable_label(self.edit_index == index, format!("{}. {name}", index + 1))
                    .clicked()
                {
                    self.edit_index = index;
                    self.preview = None;
                }
            }
        });
        let Some(edit) = edits.get_mut(self.edit_index) else {
            return changed;
        };
        ui.push_id(self.edit_index, |ui| {
            let current = label(&edit["type"]);
            let mut selected = EDITS
                .iter()
                .find(|(key, _)| *key == current)
                .copied()
                .unwrap_or(EDITS[0]);
            let old = selected;
            egui::ComboBox::from_label("修改类型")
                .selected_text(selected.1)
                .show_ui(ui, |ui| {
                    for kind in EDITS {
                        if phase == "request" || !kind.0.starts_with("query") {
                            ui.selectable_value(&mut selected, kind, kind.1);
                        }
                    }
                });
            if selected != old {
                edit["type"] = json!(selected.0);
                changed = true;
            }
            let add = selected.0.ends_with("Add");
            let remove = selected.0.ends_with("Remove");
            changed |= bool_field(ui, edit, "enabled", "启用此修改", true);
            if !add {
                changed |= string_field(ui, edit, "search", "查找", None);
                changed |= bool_field(ui, edit, "regex", "正则表达式", false);
                changed |= bool_field(ui, edit, "caseSensitive", "区分大小写", true);
            }
            if !remove {
                changed |= multiline_field(ui, edit, "replacement", "替换内容", 3);
            }
            ui.collapsing("修改预览", |ui| {
                text_area(ui, &mut self.sample, 3);
                if ui
                    .add_enabled(
                        self.preview_reply.is_none(),
                        egui::Button::new("使用引擎预览"),
                    )
                    .clicked()
                {
                    match worker.request(
                        "POST",
                        "/rewrite/preview",
                        json!({"edit": edit, "data": self.sample}),
                    ) {
                        Ok(reply) => self.preview_reply = Some(reply),
                        Err(error) => self.preview = Some(Err(error)),
                    }
                }
                if let Some(result) = &self.preview {
                    match result {
                        Ok(text) => {
                            ui.monospace(text);
                        }
                        Err(error) => {
                            ui.colored_label(ui.visuals().error_fg_color, error);
                        }
                    }
                }
            });
        });
        changed
    }
}

fn optional(
    ui: &mut egui::Ui,
    value: &mut Value,
    key: &str,
    name: &str,
    initial: Value,
    always: bool,
    changed: &mut bool,
) -> bool {
    if always {
        ui.label(name);
        return true;
    }
    let mut enabled = value.get(key).is_some();
    if ui.checkbox(&mut enabled, name).changed() {
        if enabled {
            value[key] = initial;
        } else {
            value.as_object_mut().unwrap().remove(key);
        }
        *changed = true;
    }
    enabled
}

fn bool_field(ui: &mut egui::Ui, object: &mut Value, key: &str, name: &str, default: bool) -> bool {
    let mut enabled = object[key].as_bool().unwrap_or(default);
    if ui.checkbox(&mut enabled, name).changed() {
        object[key] = json!(enabled);
        true
    } else {
        false
    }
}

fn string_field(
    ui: &mut egui::Ui,
    object: &mut Value,
    key: &str,
    name: &str,
    hint: Option<&str>,
) -> bool {
    let existed = object.get(key).is_some();
    let mut text = super::breakpoint::take_string(object, key).unwrap_or_default();
    let changed = ui
        .horizontal(|ui| {
            ui.label(name);
            ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text(hint.unwrap_or_default())
                    .desired_width(f32::INFINITY),
            )
            .changed()
        })
        .inner;
    let optional =
        matches!(key, "folder" | "path") || key == "method" && hint == Some("保留原方法");
    if optional && text.is_empty() {
        if let Some(obj) = object.as_object_mut() {
            obj.remove(key);
        }
    } else if existed || changed {
        object[key] = json!(text);
    } else if let Some(obj) = object.as_object_mut() {
        obj.remove(key);
    }
    changed
}

fn multiline_field(
    ui: &mut egui::Ui,
    object: &mut Value,
    key: &str,
    name: &str,
    rows: usize,
) -> bool {
    if !name.is_empty() {
        ui.label(name);
    }
    let mut text = super::breakpoint::take_string(object, key).unwrap_or_default();
    let changed = text_area(ui, &mut text, rows);
    object[key] = Value::String(text);
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_editor_keeps_both_phases_and_legacy_action_fields() {
        let initial = json!({
            "id": "both",
            "name": "test",
            "match": {"url": "http://localhost/*", "method": "GET", "exclude": ["http://localhost/private"]},
            "request": {"breakpoint": true, "script": "request.headers = {};"},
            "response": {"mode": "replace", "bodyFile": "/tmp/rule.body", "headers": {"x-app": ["a", "b"]}},
        });
        let mut editor = RuleEditor::new(initial.clone(), Some(initial.clone()));
        editor.draft["name"] = json!("renamed");
        let actual = editor.value().unwrap();
        assert_eq!(actual["request"], initial["request"]);
        assert_eq!(actual["response"], initial["response"]);
        assert_eq!(actual["match"], initial["match"]);
        assert_eq!(actual["name"], "renamed");
    }
}
