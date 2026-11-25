use crate::theme::Theme;
use eframe::egui::{
    self,
    text::{LayoutJob, TextFormat},
};
use std::ops::Range;

pub mod response;

const PAGE_ROWS: usize = 128;
const ROW_BYTES: usize = 512;

pub enum Content {
    Text { text: String, index: TextRows },
    Binary(Vec<u8>),
}
impl Content {
    pub fn new(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => {
                let index = TextRows::new(&text);
                Self::Text { text, index }
            }
            Ok(text) => Self::Binary(text.into_bytes()),
            Err(error) => Self::Binary(error.into_bytes()),
        }
    }
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Text { text, .. } => text.as_bytes(),
            Self::Binary(bytes) => bytes,
        }
    }
}

pub struct TextRows {
    checkpoints: Vec<usize>,
    pub count: usize,
}
fn end(text: &str, start: usize) -> usize {
    let mut limit = (start + ROW_BYTES).min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    text.as_bytes()[start..limit]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(limit, |offset| start + offset + 1)
}
impl TextRows {
    pub fn new(text: &str) -> Self {
        let mut index = Self {
            checkpoints: Vec::new(),
            count: 0,
        };
        let mut offset = 0;
        while offset < text.len() {
            if index.count.is_multiple_of(PAGE_ROWS) {
                index.checkpoints.push(offset);
            }
            offset = end(text, offset);
            index.count += 1;
        }
        index
    }

    pub fn rows<'a>(&self, text: &'a str, range: Range<usize>) -> impl Iterator<Item = &'a str> {
        self.rows_with_offsets(text, range).map(|(_, row)| row)
    }

    fn rows_with_offsets<'a>(
        &self,
        text: &'a str,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, &'a str)> {
        let page = range.start / PAGE_ROWS;
        let mut offset = self.checkpoints.get(page).copied().unwrap_or(text.len());
        let skip = range.start % PAGE_ROWS;
        std::iter::from_fn(move || {
            if offset >= text.len() {
                return None;
            }
            let start = offset;
            offset = end(text, start);
            Some((start, &text[start..offset]))
        })
        .skip(skip)
        .take(range.len())
    }
}

pub fn hex_row(bytes: &[u8], row: usize) -> String {
    use std::fmt::Write;
    let offset = row * 16;
    let slice = &bytes[offset..(offset + 16).min(bytes.len())];
    let mut text = format!("{offset:08x}  ");
    for i in 0..16 {
        if let Some(byte) = slice.get(i) {
            write!(text, "{byte:02x} ").unwrap();
        } else {
            text.push_str("   ");
        }
    }
    text.push(' ');
    for byte in slice {
        text.push(if byte.is_ascii_graphic() || *byte == b' ' {
            *byte as char
        } else {
            '.'
        });
    }
    text
}

pub(super) fn show_raw_headers(ui: &mut egui::Ui, headers: &str, has_body: bool) {
    if headers.is_empty() {
        return;
    }
    let height = ui.available_height();
    let body_space = if has_body {
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        (3.0 * (row_height + ui.spacing().item_spacing.y)).min(height / 2.0)
    } else {
        0.0
    };
    egui::ScrollArea::both()
        .id_salt("raw_message_headers")
        .max_height(height - body_space)
        .min_scrolled_height(0.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(headers).monospace()).extend());
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_rows_preserve_multibyte_text_long_lines_and_tail() {
        for text in [
            String::new(),
            "\n".repeat(5000),
            "你好🌍".repeat(1200),
            "abc\r\n".repeat(4000) + "last line",
        ] {
            let index = TextRows::new(&text);
            let mut rebuilt = String::new();
            for start in (0..index.count).step_by(17) {
                for row in index.rows(&text, start..(start + 17).min(index.count)) {
                    assert!(row.len() <= ROW_BYTES);
                    rebuilt.push_str(row);
                }
            }
            assert_eq!(rebuilt, text);
            assert_eq!(index.checkpoints.len(), index.count.div_ceil(PAGE_ROWS));
        }
    }

    #[test]
    fn binary_and_hex_are_lossless() {
        for bytes in [vec![0, 1, 255, b'A'], b"hello\n".to_vec()] {
            let content = Content::new(bytes.clone());
            assert_eq!(content.bytes(), bytes);
        }
        let bytes: Vec<u8> = (0..255).collect();
        assert!(hex_row(&bytes, 15).starts_with("000000f0  f0 f1"));
        assert_eq!(
            hex_row(b"test", 0),
            "00000000  74 65 73 74                                      test"
        );
    }

    #[test]
    fn json_highlights_escaped_keys_and_strings_across_display_rows() {
        let long = "你好\n\"".repeat(200);
        let value = serde_json::json!({"键\":": long, "number": -12.5e9, "items": [true, null]});
        let pretty = formatted_json(&value).unwrap();
        let escaped_key = serde_json::to_string("键\":").unwrap();
        let token = |kind| {
            pretty
                .tokens
                .iter()
                .find_map(|(span, found)| (*found == kind).then(|| &pretty.text[span.clone()]))
                .unwrap()
        };
        assert!(pretty.tokens.iter().any(|(span, kind)| {
            *kind == JsonToken::Key && pretty.text[span.clone()] == escaped_key
        }));
        assert_eq!(
            serde_json::from_str::<String>(token(JsonToken::String)).unwrap(),
            long
        );
        let (span, _) = pretty
            .tokens
            .iter()
            .find(|(_, kind)| *kind == JsonToken::String)
            .unwrap();
        assert!(
            pretty
                .index
                .rows_with_offsets(&pretty.text, 0..pretty.index.count)
                .filter(|(start, row)| *start >= span.start && *start + row.len() <= span.end)
                .count()
                >= 2
        );
        assert!(token(JsonToken::Number).starts_with('-'));
        assert_eq!(token(JsonToken::Literal), "true");
    }
}

pub fn show_structured(ui: &mut egui::Ui, value: &serde_json::Value, tree: bool) {
    if tree {
        let mut budget = 400;
        egui::ScrollArea::both()
            .id_salt("json_tree")
            .show(ui, |ui| {
                tree_node(ui, "$", value, &mut budget);
                if budget == 0 {
                    ui.weak("当前展开的节点已达上限，折叠分支后可查看其他内容。");
                }
            });
        return;
    }

    // Cache one bounded pretty print per inspector instead of serializing for every repaint.
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::sync::Arc;
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let digest = hasher.finish();
    let slot = ui.id().with("formatted_json");
    let cached = ui.data_mut(|data| data.get_temp::<(u64, Arc<FormattedJson>)>(slot));
    let content = match cached {
        Some((previous, content)) if previous == digest => content,
        _ => {
            let content = match formatted_json(value) {
                Ok(content) => Arc::new(content),
                Err(error) => {
                    ui.weak(error);
                    return;
                }
            };
            ui.data_mut(|data| data.insert_temp(slot, (digest, content.clone())));
            content
        }
    };
    let scroll = egui::ScrollArea::both()
        .id_salt("pretty_json")
        .auto_shrink([false, false]);
    content.show(ui, scroll, None);
}

struct FormattedJson {
    text: String,
    index: TextRows,
    tokens: Vec<(Range<usize>, JsonToken)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonToken {
    Key,
    String,
    Number,
    Literal,
}

impl FormattedJson {
    fn show(&self, ui: &mut egui::Ui, scroll: egui::ScrollArea, active_row: Option<usize>) {
        let colors = Theme::current_palette(ui.ctx());
        let literal = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(199, 180, 242)
        } else {
            egui::Color32::from_rgb(101, 66, 164)
        };
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let height = ui.text_style_height(&egui::TextStyle::Monospace);
        scroll.show_rows(ui, height, self.index.count, |ui, rows| {
            for (row, (start, text)) in rows
                .clone()
                .zip(self.index.rows_with_offsets(&self.text, rows))
            {
                let end = start + text.trim_end_matches(['\r', '\n']).len();
                let background = if Some(row) == active_row {
                    colors.selected
                } else {
                    egui::Color32::TRANSPARENT
                };
                let format = |color| TextFormat {
                    font_id: font_id.clone(),
                    color,
                    background,
                    ..Default::default()
                };
                let mut job = LayoutJob::default();
                let mut position = start;
                let first = self.tokens.partition_point(|(token, _)| token.end <= start);
                for (token, kind) in &self.tokens[first..] {
                    if token.start >= end {
                        break;
                    }
                    let token_start = token.start.max(start);
                    let token_end = token.end.min(end);
                    if position < token_start {
                        job.append(&self.text[position..token_start], 0.0, format(colors.text));
                    }
                    let color = match kind {
                        JsonToken::Key => colors.key,
                        JsonToken::String => colors.green,
                        JsonToken::Number => colors.warning,
                        JsonToken::Literal => literal,
                    };
                    job.append(&self.text[token_start..token_end], 0.0, format(color));
                    position = token_end;
                }
                if position < end {
                    job.append(&self.text[position..end], 0.0, format(colors.text));
                } else if start == end {
                    job.append(" ", 0.0, format(colors.text));
                }
                ui.add(egui::Label::new(job).extend());
            }
        });
    }
}

// Tokenize the pretty print before splitting it into display rows.
fn json_tokens(text: &str) -> Vec<(Range<usize>, JsonToken)> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        let start = offset;
        offset += 1;
        let kind = match byte {
            b'"' => {
                while let Some(&byte) = bytes.get(offset) {
                    offset += 1;
                    match byte {
                        b'"' => break,
                        b'\\' => offset += 1,
                        _ => {}
                    }
                }
                if bytes.get(offset) == Some(&b':') {
                    JsonToken::Key
                } else {
                    JsonToken::String
                }
            }
            b'-' | b'0'..=b'9' => {
                while bytes.get(offset).is_some_and(|&byte| {
                    matches!(byte, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                }) {
                    offset += 1;
                }
                JsonToken::Number
            }
            b't' | b'f' | b'n' => {
                while bytes.get(offset).is_some_and(u8::is_ascii_lowercase) {
                    offset += 1;
                }
                JsonToken::Literal
            }
            _ => continue,
        };
        tokens.push((start..offset, kind));
    }
    tokens
}

fn formatted_json(value: &serde_json::Value) -> Result<FormattedJson, String> {
    let mut buffer = CappedJson(Vec::new());
    serde_json::to_writer_pretty(&mut buffer, value)
        .map_err(|_| "格式化视图超过 1 MiB，请切换到树或原始正文。".to_owned())?;
    let text = String::from_utf8(buffer.0).expect("serialized JSON is UTF-8");
    let index = TextRows::new(&text);
    let tokens = json_tokens(&text);
    Ok(FormattedJson {
        text,
        index,
        tokens,
    })
}

struct CappedJson(Vec<u8>);
impl std::io::Write for CappedJson {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + buffer.len() > 1024 * 1024 {
            return Err(std::io::Error::other("Formatted JSON size limit"));
        }
        self.0.extend_from_slice(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tree_node(ui: &mut eframe::egui::Ui, name: &str, value: &serde_json::Value, budget: &mut usize) {
    use eframe::egui;
    use serde_json::Value;
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let bound = |text: &str| text.floor_char_boundary(240);
    let label = if name.len() > 240 {
        format!("{}…", &name[..bound(name)])
    } else {
        name.into()
    };
    let count = match value {
        Value::Object(entries) => Some((entries.len(), "{…}")),
        Value::Array(entries) => Some((entries.len(), "[…]")),
        _ => None,
    };
    ui.push_id(name, |ui| {
        if let Some((size, kind)) = count {
            egui::CollapsingHeader::new(format!("{label}  {kind}  ({size})"))
                .default_open(name == "$")
                .show(ui, |ui| match value {
                    Value::Object(entries) => {
                        for (key, child) in entries {
                            if *budget == 0 {
                                break;
                            }
                            tree_node(ui, key, child, budget);
                        }
                    }
                    Value::Array(entries) => {
                        for (index, child) in entries.iter().enumerate() {
                            if *budget == 0 {
                                break;
                            }
                            tree_node(ui, &index.to_string(), child, budget);
                        }
                    }
                    _ => {}
                });
        } else {
            let rendered = if let Value::String(text) = value {
                if text.len() > 240 {
                    format!("{:?}…", &text[..bound(text)])
                } else {
                    format!("{text:?}")
                }
            } else {
                value.to_string()
            };
            let colors = crate::theme::Theme::current_palette(ui.ctx());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{label}:"))
                        .monospace()
                        .color(colors.key),
                );
                let text = egui::RichText::new(rendered).monospace();
                ui.label(if value.is_string() {
                    text.color(colors.green)
                } else {
                    text
                });
            });
        }
    });
}
