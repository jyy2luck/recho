use crate::{
    capture_filter_ui::{MAX_CONDITIONS, MAX_VALUE_CHARS},
    sharing::{self, ShareUi},
    theme::{Theme, dialog_footer, dialog_header, input_text},
    workbench::icon,
};
use eframe::egui;
use recho_core::native::ConditionSet;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

const KIND: &str = "recho.search";
const MAX_NAME_CHARS: usize = 80;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preset {
    pub name: String,
    pub conditions: ConditionSet,
}

pub(super) enum PresetAction {
    Save,
    Apply(Preset),
    Clear,
}

enum Edit {
    Save(Preset),
    Remove(String),
}

pub(super) struct SearchPresets {
    path: PathBuf,
    entries: BTreeMap<String, ConditionSet>,
    load_error: Option<String>,
    error: Option<String>,
    edit: Option<Edit>,
    share: ShareUi<Preset>,
}

impl SearchPresets {
    pub(super) fn new(path: PathBuf) -> Self {
        let load = || -> Result<BTreeMap<String, ConditionSet>, String> {
            if !path.try_exists().map_err(|error| error.to_string())? {
                return Ok(BTreeMap::new());
            }
            serde_json::from_str(&sharing::read(&path)?).map_err(|error| error.to_string())
        };
        let (entries, load_error) = match load() {
            Ok(entries) => (entries, None),
            Err(error) => (
                BTreeMap::new(),
                Some(format!("读取已保存搜索失败（{error}）")),
            ),
        };
        Self {
            path,
            entries,
            load_error,
            error: None,
            edit: None,
            share: ShareUi::default(),
        }
    }

    pub(super) fn is_applied(&self, name: &str, conditions: &ConditionSet) -> bool {
        self.entries.get(name) == Some(conditions)
    }

    pub(super) fn menu(
        &mut self,
        ui: &mut egui::Ui,
        active: Option<&str>,
        can_save: bool,
        can_clear: bool,
    ) -> Option<PresetAction> {
        let bookmark = if active.is_some() {
            "\u{e866}"
        } else {
            "\u{e867}"
        };
        let mut button = egui::Atoms::new(icon(bookmark));
        if let Some(name) = active {
            let short: String = name.chars().take(14).collect();
            let ellipsis = if short.len() < name.len() { "…" } else { "" };
            button.push_right(format!("{short}{ellipsis}"));
        }
        let title = active.map_or_else(
            || "已保存搜索".to_owned(),
            |name| format!("已保存搜索 · {name}"),
        );
        let mut action = None;
        let response = ui.menu_button(button, |ui| {
            ui.set_min_width(176.0);
            if let Some(error) = self.load_error.as_ref().or(self.error.as_ref()) {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if ui
                .add_enabled(
                    can_save && self.load_error.is_none(),
                    egui::Button::new("保存当前搜索…"),
                )
                .clicked()
            {
                action = Some(PresetAction::Save);
                ui.close();
            }
            if ui
                .add_enabled(
                    self.load_error.is_none(),
                    egui::Button::new("导入搜索条件…"),
                )
                .clicked()
            {
                self.error = None;
                self.share.open("导入搜索条件");
                ui.close();
            }
            if can_clear && ui.button("清除搜索条件").clicked() {
                action = Some(PresetAction::Clear);
                ui.close();
            }
            if !self.entries.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (name, conditions) in &self.entries {
                            ui.push_id(name, |ui| {
                                ui.horizontal(|ui| {
                                    let label =
                                        egui::Button::selectable(active == Some(name), name)
                                            .truncate()
                                            .min_size(egui::vec2(140.0, 0.0));
                                    if ui.add(label).on_hover_text(name).clicked() {
                                        action = Some(PresetAction::Apply(Preset {
                                            name: name.clone(),
                                            conditions: conditions.clone(),
                                        }));
                                        ui.close();
                                    }
                                    ui.menu_button(icon("\u{e5d3}").size(15.0), |ui| {
                                        self.share.export_menu(ui, "recho-search", || {
                                            sharing::encode(
                                                KIND,
                                                &Preset {
                                                    name: name.clone(),
                                                    conditions: conditions.clone(),
                                                },
                                            )
                                        });
                                        if ui.button("删除已保存搜索…").clicked() {
                                            self.error = None;
                                            self.edit = Some(Edit::Remove(name.clone()));
                                            ui.close();
                                        }
                                    })
                                    .response
                                    .on_hover_text("分享或管理搜索");
                                });
                            });
                        }
                    });
            }
            if let Some(notice) = self.share.notice() {
                match notice {
                    Ok(message) => {
                        ui.weak(message);
                    }
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                }
            }
        });
        ui.ctx()
            .accesskit_node_builder(response.response.id, |node| node.set_label(title.as_str()));
        response.response.on_hover_text(title);
        action
    }

    pub(super) fn save_current(&mut self, conditions: ConditionSet) {
        self.error = None;
        self.edit = Some(Edit::Save(Preset {
            name: String::new(),
            conditions,
        }));
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(preset) = self.share.poll(ctx, |text| {
            let mut preset: Preset = sharing::decode(KIND, text)?;
            preset.name = preset.name.trim().to_owned();
            validate(&preset)?;
            if self.entries.contains_key(&preset.name) {
                let original = preset.name.clone();
                for number in 2.. {
                    let suffix = format!(" ({number})");
                    let base: String = original
                        .chars()
                        .take(MAX_NAME_CHARS - suffix.len())
                        .collect();
                    preset.name = format!("{base}{suffix}");
                    if !self.entries.contains_key(&preset.name) {
                        break;
                    }
                }
            }
            let preview = describe(&preset);
            Ok((preset, preview))
        }) {
            self.error = self.insert(preset).err();
        }
        let Some(mut edit) = self.edit.take() else {
            return;
        };
        let title = match edit {
            Edit::Save(_) => "保存搜索条件",
            Edit::Remove(_) => "删除已保存搜索",
        };
        let dialog = Theme::modal(ctx, egui::Id::new("edit_saved_search")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 360.0));
            let colors = Theme::current_palette(ctx);
            let mut cancel = dialog_header(ui, title, true);
            match &mut edit {
                Edit::Save(preset) => {
                    ui.label("名称");
                    let width = ui.available_width() - 2.0;
                    let mut frame = colors.input_frame().begin(ui);
                    let field = frame.content_ui.add(
                        input_text(&mut preset.name, "例如「接口错误」", width)
                            .char_limit(MAX_NAME_CHARS),
                    );
                    frame.frame.stroke = colors.input_stroke(&field);
                    frame.end(ui);
                    ctx.accesskit_node_builder(field.id, |node| node.set_label("名称"));
                    if field.changed() {
                        self.error = None;
                    }
                }
                Edit::Remove(name) => {
                    ui.label(format!("删除「{name}」？"));
                }
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let confirmed = dialog_footer(ui, |ui| {
                let (label, enabled) = match &edit {
                    Edit::Save(preset) => ("保存", !preset.name.trim().is_empty()),
                    Edit::Remove(_) => ("删除", true),
                };
                let color = match &edit {
                    Edit::Save(_) => colors.accent,
                    Edit::Remove(_) => colors.red,
                };
                let confirmed = ui
                    .add_enabled(enabled, colors.dialog_action(label, color))
                    .clicked();
                cancel |= ui.button("取消").clicked();
                confirmed
            });
            (confirmed, cancel)
        });
        let (confirmed, cancel) = dialog.inner;
        if cancel || dialog.should_close() {
            self.error = None;
            return;
        }
        if confirmed {
            let result = match &edit {
                Edit::Save(preset) => self.insert(preset.clone()),
                Edit::Remove(name) => {
                    let mut next = self.entries.clone();
                    next.remove(name);
                    self.write(next)
                }
            };
            self.error = result.err();
            if self.error.is_none() {
                return;
            }
        }
        self.edit = Some(edit);
    }

    fn insert(&mut self, mut preset: Preset) -> Result<(), String> {
        preset.name = preset.name.trim().into();
        validate(&preset)?;
        if self.entries.contains_key(&preset.name) {
            return Err("已保存搜索中已有这个名称。".into());
        }
        let mut next = self.entries.clone();
        next.insert(preset.name, preset.conditions);
        self.write(next)
    }

    fn write(&mut self, next: BTreeMap<String, ConditionSet>) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(error.clone());
        }
        let text = serde_json::to_vec(&next).map_err(|error| error.to_string())?;
        sharing::check_size(text.len())?;
        recho_core::native::write_local_file(&self.path, &text)?;
        self.entries = next;
        Ok(())
    }
}

fn validate(preset: &Preset) -> Result<(), String> {
    if preset.name.is_empty()
        || preset.name.chars().count() > MAX_NAME_CHARS
        || preset.name.chars().any(char::is_control)
    {
        return Err("搜索名称须为 1 到 80 个字符，且不能包含控制字符。".into());
    }
    let conditions = &preset.conditions;
    if !conditions.active() || conditions.items.len() > MAX_CONDITIONS {
        return Err(format!("搜索须有有效条件，最多支持 {MAX_CONDITIONS} 行。"));
    }
    for item in &conditions.items {
        let choices = item.field.choices();
        if !item.field.operators().contains(&item.operator)
            || item.value.chars().count() > MAX_VALUE_CHARS
            || item.values.as_ref().is_some_and(|values| {
                choices.is_empty()
                    || values.len() > choices.len()
                    || values
                        .iter()
                        .any(|value| !choices.contains(&value.as_str()))
            })
        {
            return Err(format!("「{}」搜索条件无效。", item.field.label()));
        }
    }
    conditions.validate()
}

fn describe(preset: &Preset) -> String {
    let mut lines = vec![
        preset.name.clone(),
        if preset.conditions.any {
            "任一条件（OR）"
        } else {
            "全部条件（AND）"
        }
        .into(),
    ];
    for item in &preset.conditions.items {
        let value = item
            .values
            .as_ref()
            .map_or_else(|| item.value.clone(), |values| values.join("、"));
        let modifiers = [
            (!item.enabled).then_some("［停用］"),
            item.sensitive.then_some("［区分大小写］"),
            item.deep.then_some("［深度搜索］"),
        ]
        .into_iter()
        .flatten()
        .collect::<String>();
        lines.push(format!(
            "{} {} {value} {modifiers}",
            item.field.label(),
            item.operator.label()
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use recho_core::native::{Condition, Field, Operator};

    #[test]
    fn validates_shared_search_before_saving() {
        let mut preset = Preset {
            name: "接口状态".into(),
            conditions: ConditionSet {
                any: true,
                items: vec![Condition {
                    field: Field::Status,
                    values: Some(vec!["403".into(), "500".into()]),
                    ..Condition::default()
                }],
            },
        };
        let text = sharing::encode(KIND, &preset).unwrap();
        let imported: Preset = sharing::decode(KIND, &text).unwrap();
        assert_eq!(imported.conditions, preset.conditions);
        assert!(validate(&imported).is_ok());
        preset.conditions.items[0].operator = Operator::Regex;
        assert!(validate(&preset).is_err());
    }
}
