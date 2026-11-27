use super::{name, text};
use crate::sharing;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const KIND: &str = "recho.script";
const MAX_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScriptShare {
    name: String,
    code: Code,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "language", rename_all = "lowercase", deny_unknown_fields)]
enum Code {
    Python { source: String },
    JavaScript { before: String, after: String },
}

pub(super) fn encode(request: &Value) -> Result<String, String> {
    let code = if text(request, "scriptLanguage") == "javascript" {
        Code::JavaScript {
            before: text(request, "beforeScript").into(),
            after: text(request, "afterScript").into(),
        }
    } else {
        Code::Python {
            source: text(request, "pythonScript").into(),
        }
    };
    let script = ScriptShare {
        name: name(request).into(),
        code,
    };
    script.validate()?;
    sharing::encode(KIND, &script)
}

pub(super) fn preview(text: &str, target: &str) -> Result<(ScriptShare, String), String> {
    let script: ScriptShare = sharing::decode(KIND, text)?;
    script.validate()?;
    let mut summary = format!(
        "{}\n导入到「{target}」会替换该请求的同语言脚本，导入后停用。",
        script.name
    );
    match &script.code {
        Code::Python { source } => {
            append_source(&mut summary, "Python · onRequest / onResponse", source)
        }
        Code::JavaScript { before, after } => {
            append_source(&mut summary, "JavaScript · 请求前", before);
            append_source(&mut summary, "JavaScript · 响应后", after);
        }
    }
    Ok((script, summary))
}

fn append_source(summary: &mut String, hook: &str, source: &str) {
    if source.is_empty() {
        return;
    }
    summary.push_str("\n\n");
    summary.push_str(hook);
    summary.push('\n');
    summary.extend(source.chars().take(3000));
    if source.chars().nth(3000).is_some() {
        summary.push_str("\n…\n其余源码可在导入后查看。");
    }
}

impl ScriptShare {
    fn validate(&self) -> Result<(), String> {
        let parts: &[&str] = match &self.code {
            Code::Python { source } => &[source],
            Code::JavaScript { before, after } => &[before, after],
        };
        if parts.iter().all(|part| part.trim().is_empty()) {
            return Err("脚本内容为空。".into());
        }
        if parts.iter().any(|part| part.len() > MAX_SOURCE_BYTES) {
            return Err("单段脚本超过 1 MiB。".into());
        }
        Ok(())
    }

    pub(super) fn apply(self, request: &mut Value) {
        request["scriptEnabled"] = json!(false);
        match self.code {
            Code::Python { source } => {
                request["scriptLanguage"] = json!("python");
                request["pythonScript"] = json!(source);
            }
            Code::JavaScript { before, after } => {
                request["scriptLanguage"] = json!("javascript");
                request["beforeScript"] = json!(before);
                request["afterScript"] = json!(after);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharing_carries_only_selected_script_and_import_leaves_it_disabled() {
        let source = json!({
            "name": "测试",
            "scriptEnabled": true,
            "scriptLanguage": "javascript",
            "beforeScript": "console.log('你好');\n",
            "afterScript": "console.log('done');\n",
            "authorizationSecret": "private",
            "pythonScript": "ignored"
        });
        let text = encode(&source).unwrap();
        assert!(!text.contains("private"));
        assert!(!text.contains("ignored"));
        let (script, preview) = preview(&text, "当前请求").unwrap();
        assert!(preview.contains("当前请求"));
        let mut target = json!({"scriptEnabled": true, "url": "https://example.com"});
        script.apply(&mut target);
        assert_eq!(target["scriptEnabled"], false);
        assert_eq!(target["beforeScript"], source["beforeScript"]);
        assert_eq!(target["afterScript"], source["afterScript"]);
        assert_eq!(target["url"], "https://example.com");
    }
}
