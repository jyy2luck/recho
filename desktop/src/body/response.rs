use super::{Content, FormattedJson, TextRows, formatted_json, hex_row, show_raw_headers};
use crate::{BodyFormat, files, theme::Theme};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use serde_json::Value;
use std::{borrow::Cow, collections::HashSet, time::Duration};
use tokio::sync::oneshot;

pub type SaveHandle = oneshot::Receiver<Result<bool, String>>;

const COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct View {
    search: Search,
    tree: Option<Result<Tree, String>>,
    pretty: Option<Result<FormattedJson, String>>,
    copied_until: f64,
}

impl View {
    fn body_format(
        &mut self,
        content: &Content,
        selected_format: Option<BodyFormat>,
        has_raw_headers: bool,
    ) -> BodyFormat {
        if matches!(content, Content::Binary(_)) {
            return BodyFormat::Hex;
        }
        selected_format.unwrap_or_else(|| {
            if !has_raw_headers
                && self
                    .pretty
                    .get_or_insert_with(|| prepare_json(content.bytes()))
                    .is_ok()
            {
                BodyFormat::Json
            } else {
                BodyFormat::Raw
            }
        })
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        selected_format: &mut Option<BodyFormat>,
        content: &Content,
        notice: Option<&str>,
        raw_headers: Option<&str>,
        mime: &str,
        saving: &mut Option<SaveHandle>,
    ) {
        let bytes = content.bytes();
        let is_text = matches!(content, Content::Text { .. });
        let mut format = self.body_format(content, *selected_format, raw_headers.is_some());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for (option, label) in [
                (BodyFormat::Json, "JSON"),
                (BodyFormat::Tree, "Tree"),
                (BodyFormat::Raw, "Raw"),
                (BodyFormat::Hex, "Hex"),
            ] {
                if ui
                    .add_enabled(
                        is_text || option == BodyFormat::Hex,
                        egui::Button::selectable(format == option, label)
                            .min_size(egui::vec2(35.0, 25.0)),
                    )
                    .clicked()
                {
                    *selected_format = Some(option);
                    format = option;
                }
            }
            let tree = (format == BodyFormat::Tree).then(|| {
                self.tree
                    .get_or_insert_with(|| parse_json(bytes).map(Tree::new))
            });
            let (can_expand, can_collapse) = tree
                .as_ref()
                .and_then(|tree| tree.as_ref().ok())
                .map_or((false, false), |tree| {
                    let expanded = if tree.expanded_by_default {
                        tree.branch_count - tree.toggled.len()
                    } else {
                        tree.toggled.len()
                    };
                    (expanded < tree.branch_count, expanded > 0)
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_title = if notice.is_some() {
                    "保存正文预览"
                } else {
                    "保存正文"
                };
                if action(
                    ui,
                    "\u{e2c4}",
                    save_title,
                    saving.is_none() && !bytes.is_empty(),
                )
                .clicked()
                {
                    *saving = Some(save(ui.ctx(), content, mime, notice.is_some()));
                }
                let json = if format == BodyFormat::Json {
                    self.pretty
                        .get_or_insert_with(|| prepare_json(bytes))
                        .as_ref()
                        .ok()
                } else {
                    None
                };
                let copy_title = match (format, is_text, notice.is_some()) {
                    (BodyFormat::Json, _, _) => "复制格式化 JSON",
                    (_, false, _) => "复制十六进制正文",
                    (_, true, true) => "复制正文预览",
                    (_, true, false) => "复制正文",
                };
                let copy = action_button(
                    ui,
                    "\u{e14d}",
                    copy_title,
                    !bytes.is_empty() && (format != BodyFormat::Json || json.is_some()),
                );
                let now = ui.input(|input| input.time);
                if copy.clicked() {
                    ui.ctx().copy_text(clipboard_text(content, json));
                    self.copied_until = now + COPY_FEEDBACK_DURATION.as_secs_f64();
                }
                if now < self.copied_until {
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs_f64(self.copied_until - now));
                    egui::Tooltip::for_widget(&copy).show(|ui| ui.label("已复制"));
                } else {
                    copy.on_hover_text(copy_title);
                }
                let collapse = action(ui, "\u{e5d6}", "收起全部节点", can_collapse).clicked();
                let expand = action(ui, "\u{e5d7}", "展开全部节点", can_expand).clicked();
                if let Some(Ok(tree)) = tree {
                    if collapse || expand {
                        tree.expanded_by_default = expand;
                        tree.toggled.clear();
                        tree.rebuild_visible();
                    }
                }
                if action(ui, "\u{e8b6}", "搜索正文", !bytes.is_empty()).clicked() {
                    self.search.open = !self.search.open;
                    self.search.focus = self.search.open;
                    if !self.search.open {
                        self.search.query.clear();
                    }
                }
            });
        });
        if self.search.open {
            self.search.controls(ui);
        }
        if let Some(notice) = notice {
            ui.colored_label(ui.visuals().warn_fg_color, notice);
        }
        if let Some(headers) = raw_headers {
            show_raw_headers(ui, headers, !bytes.is_empty());
            if bytes.is_empty() {
                return;
            }
        }
        match format {
            BodyFormat::Tree => match self.tree.as_mut().expect("tree format was prepared") {
                Ok(tree) => tree.show(ui, &mut self.search),
                Err(error) => {
                    self.search.refresh(format, |_| Vec::new());
                    ui.weak(error.as_str());
                }
            },
            BodyFormat::Json => match self.pretty.get_or_insert_with(|| prepare_json(bytes)) {
                Ok(json) => {
                    self.search.refresh_text(format, &json.text, &json.index);
                    let (scroll, _) = self.search.scroll_area(ui, format);
                    json.show(ui, scroll, self.search.active());
                }
                Err(error) => {
                    self.search.refresh(format, |_| Vec::new());
                    ui.weak(error.as_str());
                }
            },
            BodyFormat::Raw => {
                if let Content::Text { text, index } = content {
                    show_text(ui, &mut self.search, format, text, index);
                }
            }
            BodyFormat::Hex => {
                let count = bytes.len().div_ceil(16);
                self.search.refresh(format, |query| {
                    (0..count)
                        .filter(|&row| hex_row(bytes, row).to_ascii_lowercase().contains(query))
                        .collect()
                });
                let (scroll, height) = self.search.scroll_area(ui, format);
                scroll.show_rows(ui, height, count, |ui, range| {
                    for row in range {
                        self.search.label(ui, row, hex_row(bytes, row));
                    }
                });
            }
        }
    }
}

fn clipboard_text(content: &Content, json: Option<&FormattedJson>) -> String {
    if let Some(json) = json {
        return json.text.clone();
    }
    match content {
        Content::Text { text, .. } => text.clone(),
        Content::Binary(bytes) => {
            use std::fmt::Write;
            let mut text = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                let _ = write!(text, "{byte:02x}");
            }
            text
        }
    }
}

fn prepare_json(bytes: &[u8]) -> Result<FormattedJson, String> {
    parse_json(bytes).and_then(|value| formatted_json(&value))
}

fn parse_json(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("结构化视图支持最多 8 MiB，完整正文可在 Raw / Hex 查看".into());
    }
    serde_json::from_slice(bytes).map_err(|error| format!("正文不是有效 JSON：{error}"))
}

pub fn action(ui: &mut egui::Ui, glyph: &str, title: &str, enabled: bool) -> egui::Response {
    action_button(ui, glyph, title, enabled).on_hover_text(title)
}

fn action_button(ui: &mut egui::Ui, glyph: &str, title: &str, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(crate::workbench::icon(glyph).size(16.0))
            .min_size(egui::vec2(23.0, 25.0))
            .frame_when_inactive(false),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, title));
    response
}

fn save(ctx: &egui::Context, content: &Content, mime: &str, preview: bool) -> SaveHandle {
    let extension = match mime {
        "application/json" => "json",
        "text/html" => "html",
        "text/css" => "css",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "application/pdf" => "pdf",
        _ if matches!(content, Content::Text { .. }) => "txt",
        _ => "bin",
    };
    let name = if preview {
        "response-preview"
    } else {
        "response"
    };
    let pick = files::pick_named(extension, name.into(), ctx);
    let bytes = content.bytes().to_vec();
    let ctx = ctx.clone();
    let (send, result) = oneshot::channel();
    std::thread::spawn(move || {
        let saved = pick
            .blocking_recv()
            .map_err(|error| error.to_string())
            .and_then(|result| result)
            .and_then(|path| {
                if let Some(path) = path {
                    recho_core::native::write_local_file(&path, &bytes)?;
                    return Ok(true);
                }
                Ok(false)
            });
        let _ = send.send(saved);
        ctx.request_repaint();
    });
    result
}

#[derive(Default)]
struct Search {
    open: bool,
    focus: bool,
    query: String,
    last_query: String,
    format: Option<BodyFormat>,
    matches: Vec<usize>,
    current: usize,
    scroll: bool,
}

impl Search {
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("在正文中搜索")
                    .desired_width((ui.available_width() - 130.0).max(100.0)),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if edit.changed() {
                ui.ctx().request_repaint();
            }
            let enter = (edit.has_focus() || edit.lost_focus())
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if enter {
                edit.request_focus();
            }
            let has_matches = !self.matches.is_empty();
            ui.weak(format!(
                "{}/{}",
                if has_matches { self.current + 1 } else { 0 },
                self.matches.len()
            ));
            let prev = action(ui, "\u{e5ce}", "上一个匹配行", has_matches).clicked()
                || (enter && ui.input(|input| input.modifiers.shift));
            let next = action(ui, "\u{e5cf}", "下一个匹配行", has_matches).clicked() || enter;
            if has_matches && (prev || next) {
                let count = self.matches.len();
                self.current = (self.current + if prev { count - 1 } else { 1 }) % count;
                self.scroll = true;
            }
        });
    }

    fn refresh(&mut self, format: BodyFormat, find: impl FnOnce(&str) -> Vec<usize>) {
        if self.format == Some(format) && self.query == self.last_query {
            return;
        }
        self.format = Some(format);
        self.last_query.clone_from(&self.query);
        self.matches = if self.query.is_empty() {
            Vec::new()
        } else {
            find(&self.query.to_lowercase())
        };
        self.current = 0;
        self.scroll = !self.matches.is_empty();
    }

    fn active(&self) -> Option<usize> {
        self.matches.get(self.current).copied()
    }

    fn refresh_text(&mut self, format: BodyFormat, text: &str, index: &TextRows) {
        self.refresh(format, |query| {
            index
                .rows(text, 0..index.count)
                .enumerate()
                .filter_map(|(row, text)| text.to_lowercase().contains(query).then_some(row))
                .collect()
        });
    }

    fn scroll_target(&mut self) -> Option<usize> {
        std::mem::take(&mut self.scroll)
            .then(|| self.active())
            .flatten()
    }

    fn scroll_area(&mut self, ui: &egui::Ui, format: BodyFormat) -> (egui::ScrollArea, f32) {
        let height = ui.text_style_height(&egui::TextStyle::Monospace);
        let scroll = egui::ScrollArea::both()
            .id_salt(("response_body", format))
            .min_scrolled_height(0.0)
            .auto_shrink([false, false]);
        let scroll = if let Some(row) = self.scroll_target() {
            scroll.vertical_scroll_offset(row as f32 * (height + ui.spacing().item_spacing.y))
        } else {
            scroll
        };
        (scroll, height)
    }

    fn label(&self, ui: &mut egui::Ui, row: usize, text: impl Into<String>) {
        let mut text = RichText::new(text).monospace();
        if self.active() == Some(row) {
            text = text.background_color(Theme::current_palette(ui.ctx()).selected);
        }
        ui.add(egui::Label::new(text).extend());
    }
}

fn show_text(
    ui: &mut egui::Ui,
    search: &mut Search,
    format: BodyFormat,
    text: &str,
    index: &TextRows,
) {
    search.refresh_text(format, text, index);
    let (scroll, height) = search.scroll_area(ui, format);
    scroll.show_rows(ui, height, index.count, |ui, range| {
        for (row, text) in range.clone().zip(index.rows(text, range)) {
            let text = text.trim_end_matches(['\r', '\n']);
            search.label(ui, row, if text.is_empty() { " " } else { text });
        }
    });
}

enum Key {
    Root,
    Field(String),
    Index(usize),
}

impl Key {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Root => "$".into(),
            Self::Field(name) if name.is_empty() => r#""""#.into(),
            Self::Field(name) => name.as_str().into(),
            Self::Index(index) => index.to_string().into(),
        }
    }
}

struct Node {
    key: Key,
    value: Value,
    parent: Option<usize>,
    depth: u8,
    entries: usize,
    end: usize,
}

impl Node {
    fn label(&self) -> Cow<'_, str> {
        match &self.value {
            Value::Object(_) => format!("对象（{} 项）", self.entries).into(),
            Value::Array(_) => format!("数组（{} 项）", self.entries).into(),
            Value::String(text) if text.is_empty() => r#""""#.into(),
            Value::String(text) => text.as_str().into(),
            value => value.to_string().into(),
        }
    }
}

struct Tree {
    nodes: Vec<Node>,
    visible: Vec<usize>,
    toggled: HashSet<usize>,
    expanded_by_default: bool,
    branch_count: usize,
}

impl Tree {
    fn new(value: Value) -> Self {
        let mut tree = Self {
            nodes: Vec::new(),
            visible: Vec::new(),
            toggled: HashSet::new(),
            expanded_by_default: false,
            branch_count: 0,
        };
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                for (key, value) in fields {
                    tree.add(Key::Field(key), value, None, 0);
                }
            }
            Value::Array(items) if !items.is_empty() => {
                for (index, value) in items.into_iter().enumerate() {
                    tree.add(Key::Index(index), value, None, 0);
                }
            }
            value => tree.add(Key::Root, value, None, 0),
        }
        tree.rebuild_visible();
        tree
    }

    // Nodes are in preorder so a collapsed branch can be skipped without visiting its children.
    fn add(&mut self, key: Key, mut value: Value, parent: Option<usize>, depth: u8) {
        let object = value.as_object_mut().map(std::mem::take);
        let array = value.as_array_mut().map(std::mem::take);
        let entries = object.as_ref().map_or(0, |fields| fields.len())
            + array.as_ref().map_or(0, |items| items.len());
        self.branch_count += usize::from(entries > 0);
        let index = self.nodes.len();
        self.nodes.push(Node {
            key,
            value,
            parent,
            depth,
            entries,
            end: index + 1,
        });
        if let Some(fields) = object {
            for (key, value) in fields {
                self.add(Key::Field(key), value, Some(index), depth + 1);
            }
        }
        if let Some(items) = array {
            for (offset, value) in items.into_iter().enumerate() {
                self.add(Key::Index(offset), value, Some(index), depth + 1);
            }
        }
        self.nodes[index].end = self.nodes.len();
    }

    fn expanded(&self, node: usize) -> bool {
        self.expanded_by_default != self.toggled.contains(&node)
    }

    fn toggle(&mut self, node: usize) {
        if !self.toggled.remove(&node) {
            self.toggled.insert(node);
        }
    }

    fn rebuild_visible(&mut self) {
        self.visible.clear();
        let mut index = 0;
        while let Some(node) = self.nodes.get(index) {
            self.visible.push(index);
            index = if node.entries > 0 && self.expanded(index) {
                index + 1
            } else {
                node.end
            };
        }
    }

    fn reveal(&mut self, target: usize) -> usize {
        let mut parent = self.nodes[target].parent;
        let mut changed = false;
        while let Some(node) = parent {
            if !self.expanded(node) {
                self.toggle(node);
                changed = true;
            }
            parent = self.nodes[node].parent;
        }
        if changed {
            self.rebuild_visible();
        }
        self.visible
            .binary_search(&target)
            .expect("target is visible")
    }

    fn show(&mut self, ui: &mut egui::Ui, search: &mut Search) {
        search.refresh(BodyFormat::Tree, |query| {
            self.nodes
                .iter()
                .enumerate()
                .filter_map(|(index, node)| {
                    (node.key.label().to_lowercase().contains(query)
                        || node.label().to_lowercase().contains(query))
                    .then_some(index)
                })
                .collect()
        });
        let scroll_to = search.scroll_target().map(|node| self.reveal(node));
        let colors = Theme::current_palette(ui.ctx());
        let key_width = (ui.available_width() * 0.34).clamp(96.0, 220.0);
        let mut toggled = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let painter = ui.painter().clone();
            let line = egui::Stroke::new(1.0, colors.line);
            let table = TableBuilder::new(ui)
                .id_salt("response_tree")
                .min_scrolled_height(0.0)
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(
                    Column::initial(key_width)
                        .at_least(82.0)
                        .at_most(260.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(Column::remainder().clip(true));
            let table = if let Some(row) = scroll_to {
                table.scroll_to_row(row, Some(egui::Align::Center))
            } else {
                table
            };
            table
                .header(25.0, |mut row| {
                    let (_, key) = row.col(|ui| {
                        ui.add_space(7.0);
                        ui.weak("键");
                    });
                    let (_, value) = row.col(|ui| {
                        ui.add_space(7.0);
                        ui.weak("值");
                    });
                    painter.line_segment([key.rect.left_bottom(), value.rect.right_bottom()], line);
                })
                .body(|body| {
                    body.rows(22.0, self.visible.len(), |mut row| {
                        let index = self.visible[row.index()];
                        let node = &self.nodes[index];
                        row.set_selected(search.active() == Some(index));
                        row.col(|ui| {
                            const DEPTH_STEP: f32 = 14.0;
                            const KEY_INSET: f32 = 7.0;
                            const ARROW_WIDTH: f32 = 14.0;
                            let rect = ui.max_rect();
                            let max_depth = ((rect.width() - 44.0) / DEPTH_STEP).max(0.0) as u8;
                            let depth = node.depth.min(max_depth);
                            for level in 0..depth {
                                let x = rect.left()
                                    + KEY_INSET
                                    + ARROW_WIDTH / 2.0
                                    + f32::from(level) * DEPTH_STEP;
                                ui.painter().vline(
                                    x,
                                    rect.y_range(),
                                    egui::Stroke::new(
                                        1.0,
                                        colors.bg.lerp_to_gamma(colors.line, 0.65),
                                    ),
                                );
                            }
                            ui.spacing_mut().item_spacing.x = 3.0;
                            ui.spacing_mut().button_padding = egui::Vec2::ZERO;
                            ui.add_space(KEY_INSET + f32::from(depth) * DEPTH_STEP);
                            if node.entries > 0 {
                                let icon = if self.expanded(index) {
                                    "\u{e5cf}"
                                } else {
                                    "\u{e5cc}"
                                };
                                let response = ui.add(
                                    egui::Button::new(crate::workbench::icon(icon).size(13.0))
                                        .frame_when_inactive(false)
                                        .min_size(egui::vec2(ARROW_WIDTH, 20.0)),
                                );
                                if response.clicked() {
                                    toggled = Some(index);
                                }
                            } else {
                                ui.allocate_space(egui::vec2(ARROW_WIDTH, 20.0));
                            }
                            let label = ui.add(
                                egui::Label::new(
                                    RichText::new(snippet(&node.key.label())).color(colors.key),
                                )
                                .truncate()
                                .sense(if node.entries > 0 {
                                    egui::Sense::click()
                                } else {
                                    egui::Sense::hover()
                                }),
                            );
                            if label.clicked() {
                                toggled = Some(index);
                            }
                        });
                        row.col(|ui| {
                            ui.add_space(7.0);
                            let color = match node.value {
                                Value::Number(_) => colors.warning,
                                Value::String(_) => colors.green,
                                Value::Bool(_) | Value::Null => colors.accent,
                                _ => colors.muted,
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(snippet(&node.label())).color(color),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
        });
        if let Some(node) = toggled {
            self.toggle(node);
            self.rebuild_visible();
            ui.ctx().request_repaint();
        }
    }
}

fn snippet(text: &str) -> &str {
    &text[..text.floor_char_boundary(4096)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn response_defaults_to_json_and_copies_the_displayed_format() {
        let original = r#"{"field":[1,2]}"#;
        let content = Content::new(original.as_bytes().to_vec());
        let mut view = View::default();
        assert_eq!(view.body_format(&content, None, false), BodyFormat::Json);
        assert_eq!(
            view.body_format(&content, Some(BodyFormat::Raw), false),
            BodyFormat::Raw
        );
        assert_eq!(view.body_format(&content, None, true), BodyFormat::Raw);

        let json = view.pretty.as_ref().unwrap().as_ref().unwrap();
        assert_eq!(
            clipboard_text(&content, Some(json)),
            "{\n  \"field\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(clipboard_text(&content, None), original);
        assert_eq!(
            View::default().body_format(&Content::new(b"{".to_vec()), None, false),
            BodyFormat::Raw
        );
        assert_eq!(
            View::default().body_format(&Content::Binary(vec![0xff]), None, false),
            BodyFormat::Hex
        );
    }

    #[test]
    fn tree_keeps_top_level_fields_visible_and_reveals_nested_search_matches() {
        let mut tree = Tree::new(json!({"$": {"items": [{"needle": "here"}]}, "other": []}));
        let labels = |tree: &Tree| {
            tree.visible
                .iter()
                .map(|&index| tree.nodes[index].key.label().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(&tree), ["$", "other"]);
        assert_eq!(tree.reveal(3), 3);
        assert_eq!(labels(&tree), ["$", "items", "0", "needle", "other"]);
        tree.toggled.clear();
        tree.rebuild_visible();
        assert_eq!(labels(&tree), ["$", "other"]);
        tree.expanded_by_default = true;
        tree.rebuild_visible();
        assert_eq!(labels(&tree), ["$", "items", "0", "needle", "other"]);
    }
}
