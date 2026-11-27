use crate::{
    Recho,
    search_presets::PresetAction,
    theme::{CONTROL_HEIGHT, Theme, control_row, input_text},
    workbench::icon,
};
use eframe::egui;
use egui_extras::{Size, StripBuilder};
use recho_core::native::{Condition, ConditionSet, Field};

pub(super) const MAX_CONDITIONS: usize = 32;
pub(super) const MAX_VALUE_CHARS: usize = 4096;
const MODIFIER_WIDTH: f32 = 28.0;

pub(super) fn presets(app: &mut Recho, ui: &mut egui::Ui) {
    let (draft, error, selected) = match app.active_history() {
        Some(history) => (
            &history.filter_draft,
            &history.filter_error,
            history
                .preset
                .as_ref()
                .filter(|name| app.search_presets.is_applied(name, &history.conditions)),
        ),
        None => (
            &app.spaces.filter_draft,
            &app.spaces.filter_error,
            app.spaces.current().and_then(|space| {
                space
                    .preset
                    .as_ref()
                    .filter(|name| app.search_presets.is_applied(name, &space.conditions))
            }),
        ),
    };
    let selected = selected.cloned();
    let can_save = error.is_none() && draft.active();
    let can_clear =
        draft.items.iter().any(|item| item != &Condition::default()) || draft.items.len() > 1;
    match app
        .search_presets
        .menu(ui, selected.as_deref(), can_save, can_clear)
    {
        Some(PresetAction::Save) => {
            let draft = app
                .active_history()
                .map_or(&app.spaces.filter_draft, |history| &history.filter_draft)
                .clone();
            app.search_presets.save_current(draft);
        }
        Some(PresetAction::Apply(preset)) => apply(app, preset.conditions, Some(preset.name)),
        Some(PresetAction::Clear) => apply(app, ConditionSet::default(), None),
        None => {}
    }
}

fn apply(app: &mut Recho, draft: ConditionSet, preset: Option<String>) {
    if let Some(history) = app.active_history_mut() {
        history.filter_draft = draft;
        history.filter_error = history.filter_draft.validate().err();
        if history.filter_error.is_none() {
            history.conditions.clone_from(&history.filter_draft);
            history.preset = preset;
            app.load_history(0);
        }
    } else if let Some(space) = app.spaces.items.get_mut(&app.spaces.group) {
        app.spaces.filter_draft = draft;
        app.spaces.filter_error = app.spaces.filter_draft.validate().err();
        if app.spaces.filter_error.is_none() {
            space.conditions.clone_from(&app.spaces.filter_draft);
            space.preset = preset;
            app.evaluate_conditions();
        }
    }
}

pub(super) fn show(app: &mut Recho, ui: &mut egui::Ui) {
    let draft = if app.active_history().is_some() {
        &mut app.active_history_mut().unwrap().filter_draft
    } else if app.spaces.items.contains_key(&app.spaces.group) {
        &mut app.spaces.filter_draft
    } else {
        return;
    };
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
                    control_row(ui);
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
        let draft = std::mem::take(draft);
        apply(app, draft, None);
    }
    let error = app
        .active_history()
        .map_or(&app.spaces.filter_error, |history| &history.filter_error);
    if let Some(error) = error {
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
    Theme::compact_window(ctx, "自动高亮")
        .open(&mut opened)
        .default_width(640.0)
        .show(ctx, |ui| {
            control_row(ui);
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
    let colors = Theme::current_palette(ui.ctx());
    colors
        .input_frame()
        .inner_margin(2)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().interact_size.y = CONTROL_HEIGHT - 6.0;
            let all = ui.selectable_value(&mut conditions.any, false, "全部条件 (AND)");
            let any = ui.selectable_value(&mut conditions.any, true, "任一条件 (OR)");
            all.changed() || any.changed()
        })
        .inner
}

fn rows(ui: &mut egui::Ui, conditions: &mut ConditionSet, height: f32, highlight: bool) -> bool {
    let mut changed = false;
    let mut remove = None;
    let mut add = false;
    let can_add = conditions.items.len() < MAX_CONDITIONS;
    let colors = Theme::current_palette(ui.ctx());
    egui::ScrollArea::both()
        .id_salt("condition_rows")
        .auto_shrink([false, true])
        .max_height(height)
        .show(ui, |ui| {
            let width = ui.available_width().max(446.0);
            ui.set_min_width(width);
            for (index, item) in conditions.items.iter_mut().enumerate() {
                ui.push_id(index, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, CONTROL_HEIGHT),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            control_row(ui);
                            let combo_height = ui
                                .text_style_height(&egui::TextStyle::Button)
                                .max(ui.spacing().icon_width);
                            ui.spacing_mut().button_padding =
                                egui::vec2(8.0, ((CONTROL_HEIGHT - combo_height) * 0.5).max(0.0));
                            StripBuilder::new(ui)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .size(Size::exact(CONTROL_HEIGHT))
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
                                    row.cell(|ui| {
                                        changed |= field_picker(ui, item, highlight);
                                    });
                                    row.cell(|ui| {
                                        changed |= operator_picker(ui, item);
                                    });
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
    ui.spacing_mut().button_padding.y = 0.0;
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(icon).size(16.0))
            .min_size(egui::vec2(28.0, CONTROL_HEIGHT))
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
    let mut frame = colors.input_frame().begin(parent);
    let ui = &mut frame.content_ui;
    ui.set_min_size(egui::vec2(width, CONTROL_HEIGHT - 2.0));
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.spacing_mut().interact_size.y = CONTROL_HEIGHT - 8.0;
    ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
    let changed = if item.field.choices().is_empty() {
        let supports_deep = item.field.supports_deep();
        let modifiers = if supports_deep { 2.0 } else { 1.0 };
        let text_width = width - modifiers * (MODIFIER_WIDTH + 4.0) - 4.0;
        let value =
            ui.add(input_text(&mut item.value, "搜索内容", text_width).char_limit(MAX_VALUE_CHARS));
        ui.ctx()
            .accesskit_node_builder(value.id, |node| node.set_label("搜索内容"));
        frame.frame.stroke = colors.input_stroke(&value);
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
            .min_size(egui::vec2(MODIFIER_WIDTH, CONTROL_HEIGHT - 8.0)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condition_rows_scroll_only_when_they_overflow() {
        for (width, count, scroll_x, scroll_y) in [
            (640.0, 1, false, false),
            (640.0, 4, false, true),
            (380.0, 1, true, false),
        ] {
            let ctx = egui::Context::default();
            Theme::default().apply(&ctx);
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "icons".into(),
                std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                    "../assets/fonts/MaterialIcons-Regular.ttf"
                ))),
            );
            fonts
                .families
                .insert(egui::FontFamily::Name("icons".into()), vec!["icons".into()]);
            ctx.set_fonts(fonts);
            let mut conditions = ConditionSet {
                items: vec![Condition::default(); count],
                ..Default::default()
            };
            let mut id = egui::Id::NULL;
            for delta in [egui::Vec2::ZERO, egui::vec2(-100.0, -100.0)] {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 240.0),
                        )),
                        events: vec![egui::Event::PointerMoved(egui::pos2(350.0, 16.0))],
                        ..Default::default()
                    },
                    |ui| {
                        ui.input_mut(|input| input.smooth_scroll_delta = delta);
                        id = ui.make_persistent_id(egui::IdSalt::new("condition_rows"));
                        rows(ui, &mut conditions, 90.0, false);
                    },
                );
                output.textures_delta.clear();
            }
            let offset = egui::scroll_area::State::load(&ctx, id).unwrap().offset;
            assert_eq!(offset.x > 0.0, scroll_x, "width {width}, offset {offset:?}");
            assert_eq!(offset.y > 0.0, scroll_y, "count {count}, offset {offset:?}");
        }
    }

    #[test]
    fn modifier_shows_selection_without_hover() {
        let ctx = egui::Context::default();
        Theme::default().apply(&ctx);
        for selected in [false, true] {
            let mut enabled = selected;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                modifier(ui, egui::RichText::new("Aa"), &mut enabled, "区分大小写");
            });
            output.textures_delta.clear();
            let selected_fill = Theme::current_palette(&ctx).selected;
            assert_eq!(
                output.shapes.iter().any(|shape| matches!(
                    &shape.shape,
                    egui::Shape::Rect(rect) if rect.fill == selected_fill
                )),
                selected,
            );
        }
    }
}
