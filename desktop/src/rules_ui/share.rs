use super::label;
use crate::sharing;
use serde_json::{Value, json};

const KIND: &str = "recho.rule";

pub(super) fn encode_rule(rule: &Value, values: &[Value]) -> Result<String, String> {
    sharing::encode(KIND, &portable(inline_values(rule, values)?)?)
}

pub(super) fn inline_values(rule: &Value, values: &[Value]) -> Result<Value, String> {
    let mut rule = rule.clone();
    for phase in ["request", "response", "mock"] {
        let Some(action) = rule.get_mut(phase).and_then(Value::as_object_mut) else {
            continue;
        };
        for (reference, inline) in [
            ("bodyValue", "body"),
            ("scriptValue", "script"),
            ("pythonValue", "python"),
        ] {
            if let Some(id) = action.remove(reference) {
                let text = values
                    .iter()
                    .find(|value| value["id"] == id)
                    .and_then(|value| value["text"].as_str())
                    .ok_or("找不到规则引用的 Value，请刷新后重试。")?;
                action.insert(inline.into(), json!(text));
            }
        }
    }
    Ok(rule)
}

pub(super) fn preview_rule(text: &str) -> Result<(Value, String), String> {
    let mut rule = portable(sharing::decode(KIND, text)?)?;
    let mut summary = format!(
        "{}\n{} {}\n导入后默认停用。",
        label(&rule["name"]),
        label(&rule["match"]["method"]),
        label(&rule["match"]["url"]),
    );
    if requires_local_file(&rule) {
        summary.push_str("\n保存前请为正文文件选择本机路径。");
    }
    let details = serde_json::to_string_pretty(&rule).map_err(|error| error.to_string())?;
    summary.push_str("\n\n");
    summary.extend(details.chars().take(6000));
    if details.chars().nth(6000).is_some() {
        summary.push_str("\n…\n其余内容可在导入后查看。");
    }
    rule["id"] = json!(uuid::Uuid::new_v4().to_string());
    rule["enabled"] = json!(false);
    Ok((rule, summary))
}

pub(super) fn requires_local_file(rule: &Value) -> bool {
    ["request", "response"]
        .into_iter()
        .any(|phase| rule[phase].get("bodyFile").is_some())
}

fn portable(mut rule: Value) -> Result<Value, String> {
    let Some(object) = rule.as_object_mut() else {
        return Err("分享内容需要包含一个规则对象。".into());
    };
    if object
        .get("match")
        .and_then(|value| value["url"].as_str())
        .is_none_or(|url| url.is_empty() || url.chars().any(char::is_whitespace))
        || ["request", "response", "mock"]
            .into_iter()
            .any(|phase| object.get(phase).is_some_and(|action| !action.is_object()))
        || !["request", "response", "mock"]
            .into_iter()
            .any(|phase| object.contains_key(phase))
    {
        return Err("规则需要有效的 URL 匹配条件和至少一个行为。".into());
    }
    object.remove("id");
    object.remove("enabled");
    for phase in ["request", "response"] {
        if let Some(action) = object.get_mut(phase).and_then(Value::as_object_mut)
            && action.contains_key("bodyFile")
        {
            action.insert("bodyFile".into(), json!(""));
        }
    }
    Ok(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_retains_script_and_replaces_local_paths_and_identity() {
        let rule = json!({
            "id": "existing",
            "enabled": true,
            "name": "检查响应",
            "match": {"url":"https://example.com/*", "method": "*"},
            "response": {"bodyFile":"/Users/alice/private.json", "python":"# 中文\npass\n"}
        });
        let shared = encode_rule(&rule, &[]).unwrap();
        assert!(!shared.contains("/Users/alice"));
        assert!(!shared.contains("existing"));
        let (imported, preview) = preview_rule(&shared).unwrap();
        assert_eq!(imported["enabled"], false);
        assert_ne!(imported["id"], rule["id"]);
        assert_eq!(imported["response"]["bodyFile"], "");
        assert_eq!(imported["response"]["python"], rule["response"]["python"]);
        assert!(preview.contains("本机路径"));
    }

    #[test]
    fn exported_rules_carry_the_referenced_text() {
        let rule = json!({
            "name": "分享风控",
            "match": {"url": "https://example.test/share"},
            "response": {"bodyValue": "body", "scriptValue": "code"}
        });
        let values = [
            json!({"id": "body", "text": "{\"error\":\"denied\"}"}),
            json!({"id": "code", "text": "response.status = 403;"}),
        ];
        let encoded = encode_rule(&rule, &values).unwrap();
        let (imported, _) = preview_rule(&encoded).unwrap();
        assert_eq!(
            imported["response"],
            json!({"body": values[0]["text"], "script": values[1]["text"]})
        );
        assert!(encode_rule(&rule, &values[..1]).is_err());
    }
}
