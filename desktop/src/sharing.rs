mod ui;

pub(super) use ui::ShareUi;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{io::Read, path::Path};

const VERSION: u32 = 1;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    data: T,
}

pub(super) fn encode<T: Serialize>(kind: &str, data: &T) -> Result<String, String> {
    let text = serde_json::to_string(&Envelope {
        kind: kind.into(),
        version: VERSION,
        data,
    })
    .map_err(|error| error.to_string())?;
    check_size(text.len())?;
    Ok(text)
}

pub(super) fn decode<T: DeserializeOwned>(kind: &str, text: &str) -> Result<T, String> {
    check_size(text.len())?;
    let envelope: Envelope<serde_json::Value> =
        serde_json::from_str(text.trim()).map_err(|error| format!("无效的分享文本：{error}"))?;
    if envelope.kind != kind {
        return Err("分享内容与当前导入入口的类型不符。".into());
    }
    if envelope.version != VERSION {
        return Err(format!("当前版本无法读取分享格式 v{}。", envelope.version));
    }
    serde_json::from_value(envelope.data).map_err(|error| format!("无效的分享内容：{error}"))
}

pub(super) fn read(path: &Path) -> Result<String, String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    check_size(text.len())?;
    Ok(text)
}

pub(super) fn check_size(bytes: usize) -> Result<(), String> {
    if bytes > MAX_BYTES {
        return Err("分享内容超过 16 MiB。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_text_round_trips_and_checks_kind_and_version() {
        let source = "中文\n\"quoted\"\nregex = r'\\d+'";
        let text = encode("recho.script", &source).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert_eq!(decode::<String>("recho.script", &text).unwrap(), source);
        assert!(decode::<String>("recho.search", &text).is_err());
        let future = text.replace("\"version\":1", "\"version\":2");
        assert!(decode::<String>("recho.script", &future).is_err());
    }
}
