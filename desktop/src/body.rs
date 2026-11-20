use std::ops::Range;

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
        let page = range.start / PAGE_ROWS;
        let mut offset = self.checkpoints.get(page).copied().unwrap_or(text.len());
        let skip = range.start % PAGE_ROWS;
        std::iter::from_fn(move || {
            if offset >= text.len() {
                return None;
            }
            let start = offset;
            offset = end(text, start);
            Some(&text[start..offset])
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
}
