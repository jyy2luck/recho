use crate::{Recho, theme::Theme, workbench::icon};
use eframe::egui;
use egui_extras::{Size, StripBuilder};
use recho_core::native::{Condition, ConditionSet, Field};

const MAX_CONDITIONS: usize = 32;
const ROW_HEIGHT: f32 = 32.0;
const MODIFIER_WIDTH: f32 = 28.0;

pub(super) fn show(app: &mut Recho, ui: &mut egui::Ui) {
    let Some(space) = app.spaces.items.get_mut(&app.spaces.group) else {
        return;
    };
    let draft = &mut app.spaces.filter_draft;
    if draft.items.is_empty() {
        draft.items.push(Condition::default());
    }
    let height = (ui.available_height() * 0.35).clamp(72.0, 240.0);
    let colors = Theme::current_palette(ui.ctx());
    let changed = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(8, 6))
        .fill(colors.side)
        .stroke(egui::Stroke::new(1.0, colors.line))
        .corner_radius(6)
        .show(ui, |ui| {
            let mut changed = rows(ui, draft, height, false);
            if draft.items.len() > 1 {
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    changed |= combination(ui, draft);
                    if ui
                        .button(egui::RichText::new("清除条件").color(colors.muted))
                        .clicked()
                    {
                        *draft = ConditionSet::default();
                        changed = true;
                    }
                });
            }
            changed
        })
        .inner;
    if changed {
        app.spaces.filter_error = draft.validate().err();
        if app.spaces.filter_error.is_none() {
            space.conditions.clone_from(draft);
            app.evaluate_conditions();
        }
    }
    if let Some(error) = &app.spaces.filter_error {
        let summary = error.lines().last().unwrap_or(error);
        ui.colored_label(colors.red, format!("筛选条件无效：{summary}"))
            .on_hover_text(error);
    }
}

impl Recho {
    pub(super) fn edit_highlights(&mut self) {
        self.highlight_editor = self.spaces.current().map(|space| {
            let mut rules = space.highlights.clone();
            if rules.items.is_empty() {
                rules.items.push(Condition::default());
            }
            (rules, space.highlight_color().into())
        });
    }
}

pub(super) fn highlight_dialog(app: &mut Recho, ctx: &egui::Context) {
    let Some((conditions, color)) = &mut app.highlight_editor else {
        return;
    };
    let mut opened = true;
    let mut save = false;
    egui::Window::new("自动高亮")
        .open(&mut opened)
        .default_width(640.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("高亮");
                egui::ComboBox::from_id_salt("highlight_color")
                    .selected_text(color.as_str())
                    .show_ui(ui, |ui| {
                        for &choice in &Field::Highlight.choices()[1..] {
                            ui.selectable_value(color, choice.to_owned(), choice);
                        }
                    });
                combination(ui, conditions);
            });
            rows(ui, conditions, 360.0, true);
            let validation = conditions.validate();
            save = ui
                .add_enabled(validation.is_ok(), egui::Button::new("应用"))
                .clicked();
            if let Err(error) = validation {
                ui.label(error);
            }
        });
    if save {
        let (conditions, color) = app.highlight_editor.take().unwrap();
        if let Some(space) = app.spaces.items.get_mut(&app.spaces.group) {
            space.highlights = conditions;
            space.highlight_color = Some(color);
            app.evaluate_conditions();
        }
    } else if !opened {
        app.highlight_editor = None;
    }
}

fn combination(ui: &mut egui::Ui, conditions: &mut ConditionSet) -> bool {
    let mut changed = false;
    ui.group(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        changed |= ui
            .selectable_value(&mut conditions.any, false, "全部条件 (AND)")
            .changed();
        changed |= ui
            .selectable_value(&mut conditions.any, true, "任一条件 (OR)")
            .changed();
    });
    changed
}

fn rows(ui: &mut egui::Ui, conditions: &mut ConditionSet, height: f32, highlight: bool) -> bool {
    let mut changed = false;
    let mut remove = None;
    let mut add = false;
    let can_add = conditions.items.len() < MAX_CONDITIONS;
    let width = ui.available_width().max(446.0);
    let colors = Theme::current_palette(ui.ctx());
    egui::ScrollArea::both()
        .id_salt("condition_rows")
        .auto_shrink([false, true])
        .max_height(height)
        .show(ui, |ui| {
            ui.set_min_width(width);
            for (index, item) in conditions.items.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, ROW_HEIGHT),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.spacing_mut().button_padding = egui::vec2(8.0, 5.0);
                            StripBuilder::new(ui)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .size(Size::exact(26.0))
                                .size(Size::exact(98.0))
                                .size(Size::exact(108.0))
                                .size(Size::remainder())
                                .sizes(Size::exact(28.0), 2)
                                .horizontal(|mut row| {
                                    row.cell(|ui| {
                                        ui.visuals_mut().widgets.inactive.bg_stroke =
                                            egui::Stroke::new(1.0, colors.muted);
                                        let enabled = ui
                                            .add(egui::Checkbox::without_text(&mut item.enabled))
                                            .on_hover_text("启用条件");
                                        ui.ctx().accesskit_node_builder(enabled.id, |node| {
                                            node.set_label("启用条件")
                                        });
                                        changed |= enabled.changed();
                                    });
                                    row.cell(|ui| changed |= field_picker(ui, item, highlight));
                                    row.cell(|ui| changed |= operator_picker(ui, item));
                                    row.cell(|ui| changed |= condition_value(ui, item));
                                    row.cell(|ui| {
                                        if row_button(ui, "−", "删除条件", true).clicked() {
                                            remove = Some(index);
                                        }
                                    });
                                    row.cell(|ui| {
                                        add |= row_button(ui, "+", "添加条件", can_add).clicked();
                                    });
                                });
                        },
                    );
                });
            }
        });
    if let Some(index) = remove {
        conditions.items.remove(index);
        if conditions.items.is_empty() {
            conditions.items.push(Condition::default());
        }
    }
    if add {
        conditions.items.push(Condition::default());
    }
    changed || add || remove.is_some()
}

fn row_button(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(icon).size(16.0))
            .min_size(egui::vec2(28.0, 28.0))
            .frame_when_inactive(false),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    response.on_hover_text(label)
}

fn field_picker(ui: &mut egui::Ui, item: &mut Condition, highlight: bool) -> bool {
    let old = item.field;
    let mut selected = old;
    egui::ComboBox::from_id_salt("field")
        .width(ui.available_width())
        .wrap_mode(egui::TextWrapMode::Truncate)
        .height(370.0)
        .selected_text(old.label())
        .show_ui(ui, |ui| {
            for field in Field::ALL.into_iter().chain(Field::LEGACY) {
                if (highlight && field == Field::Highlight)
                    || (!highlight && Field::LEGACY.contains(&field) && field != old)
                {
                    continue;
                }
                ui.selectable_value(&mut selected, field, field.label());
            }
        });
    if selected != old {
        item.set_field(selected);
    }
    selected != old
}

fn operator_picker(ui: &mut egui::Ui, item: &mut Condition) -> bool {
    let old = item.operator;
    egui::ComboBox::from_id_salt("operator")
        .width(ui.available_width())
        .wrap_mode(egui::TextWrapMode::Truncate)
        .height(260.0)
        .selected_text(old.label())
        .show_ui(ui, |ui| {
            for &operator in item.field.operators() {
                ui.selectable_value(&mut item.operator, operator, operator.label());
            }
        });
    item.operator != old
}

fn condition_value(parent: &mut egui::Ui, item: &mut Condition) -> bool {
    let colors = Theme::current_palette(parent.ctx());
    let width = parent.available_width() - 2.0;
    let mut frame = egui::Frame::new()
        .fill(colors.bg)
        .stroke(egui::Stroke::new(1.0, colors.line))
        .corner_radius(5)
        .begin(parent);
    let ui = &mut frame.content_ui;
    ui.set_min_size(egui::vec2(width, ROW_HEIGHT - 2.0));
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
    let changed = if item.field.choices().is_empty() {
        let supports_deep = item.field.supports_deep();
        let modifiers = if supports_deep { 2.0 } else { 1.0 };
        let text_width = width - modifiers * (MODIFIER_WIDTH + 4.0) - 4.0;
        let value = ui.add(
            egui::TextEdit::singleline(&mut item.value)
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0)))
                .vertical_align(egui::Align::Center)
                .hint_text("搜索内容")
                .char_limit(4096)
                .min_size(egui::vec2(text_width, ROW_HEIGHT - 2.0))
                .desired_width(text_width),
        );
        ui.ctx()
            .accesskit_node_builder(value.id, |node| node.set_label("搜索内容"));
        if value.has_focus() {
            frame.frame.stroke.color = colors.accent;
        }
        let mut changed = value.changed();
        if supports_deep {
            changed |= modifier(ui, icon("\u{e157}").size(16.0), &mut item.deep, "深度搜索");
        }
        changed |= modifier(
            ui,
            egui::RichText::new("Aa").size(12.0),
            &mut item.sensitive,
            "区分大小写",
        );
        changed
    } else {
        ui.add_space(4.0);
        value_picker(ui, item)
    };
    frame.end(parent);
    changed
}

fn modifier(ui: &mut egui::Ui, text: egui::RichText, selected: &mut bool, label: &str) -> bool {
    let response = ui.add(
        egui::Button::selectable(*selected, text)
            .min_size(egui::vec2(MODIFIER_WIDTH, 24.0))
            .frame_when_inactive(false),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *selected, label)
    });
    if response.on_hover_text(label).clicked() {
        *selected = !*selected;
        true
    } else {
        false
    }
}

fn value_picker(ui: &mut egui::Ui, item: &mut Condition) -> bool {
    let values = item.values.as_deref().unwrap_or(if item.value.is_empty() {
        &[]
    } else {
        std::slice::from_ref(&item.value)
    });
    let mut remove = None;
    let mut add = None;
    let label = if values.is_empty() {
        "点击添加值"
    } else {
        "添加值"
    };
    egui::ComboBox::from_id_salt("values")
        .selected_text(label)
        .width(68.0)
        .height(224.0)
        .show_ui(ui, |ui| {
            for &choice in item.field.choices() {
                if !values.iter().any(|value| value == choice)
                    && ui.selectable_label(false, choice).clicked()
                {
                    add = Some(choice);
                }
            }
        });
    egui::ScrollArea::horizontal()
        .id_salt("selected_values_scroll")
        .show(ui, |ui| {
            ui.spacing_mut().button_padding.x = 6.0;
            for (index, value) in values.iter().enumerate() {
                if ui
                    .add(egui::Button::new(format!("{value} ×")).min_size(egui::vec2(0.0, 23.0)))
                    .on_hover_text("删除值")
                    .clicked()
                {
                    remove = Some(index);
                }
            }
        });
    if remove.is_none() && add.is_none() {
        return false;
    }
    let mut values = values.to_vec();
    if let Some(index) = remove {
        values.remove(index);
    }
    if let Some(choice) = add {
        values.push(choice.into());
    }
    item.value.clear();
    item.values = Some(values);
    item.restrict_operator();
    true
}
