use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chrono::Utc;
use rquickjs::{Context, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

#[path = "rule_edits.rs"]
mod edits;
use edits::Modification;
pub use edits::preview as preview_edit;

use crate::{
    storage::atomic_write,
    wire::{
        Error, Headers, RequestData, ResponseData, Result, method, normalize_headers, parse_url,
    },
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replacement {
    search: String,
    replacement: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    regex: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryEdit {
    name: String,
    #[serde(default)]
    remove: bool,
    #[serde(default)]
    regex: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    #[serde(skip_serializing_if = "Option::is_none")]
    edits: Option<Vec<Modification>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replace_headers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preserve_host: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<Vec<QueryEdit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    header_edits: Option<Vec<QueryEdit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_replacements: Option<Vec<Replacement>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    python: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<Headers>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replace: Option<Replacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    breakpoint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    redirect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
}
impl Action {
    fn validate(&mut self, phase: &str) -> Result<()> {
        if let Some(edits) = &self.edits {
            for edit in edits {
                edit.validate(phase)?;
            }
        }
        if self
            .mode
            .as_ref()
            .is_some_and(|m| !["replace", "modify"].contains(&m.as_str()))
        {
            return Err(Error::bad("Action mode must be replace or modify"));
        }
        if phase != "request"
            && (self.method.is_some()
                || self.path.is_some()
                || self.query.is_some()
                || self.preserve_host.is_some())
        {
            return Err(Error::bad(
                "Only request actions support method/path/query/preserveHost",
            ));
        }
        if self.mode.as_deref() == Some("modify")
            && (self.method.is_some() || self.path.is_some() || self.status.is_some())
        {
            return Err(Error::bad(
                "Modify actions cannot change method/path/status; use replace",
            ));
        }
        if let Some(value) = &self.method {
            self.method = Some(method(value)?);
        }
        if self.path.as_ref().is_some_and(|p| {
            !p.starts_with('/')
                || p.starts_with("//")
                || p.contains('#')
                || p.chars().any(char::is_whitespace)
        }) {
            return Err(Error::bad(
                "Replacement path must start with / and contain no whitespace or fragment",
            ));
        }
        if self.body.is_some() && self.body_file.is_some() {
            return Err(Error::bad("Choose body or bodyFile"));
        }
        if let Some(path) = &self.body_file {
            if !Path::new(path).is_absolute() {
                return Err(Error::bad("bodyFile must be an absolute path"));
            }
        }
        if let Some(source) = &self.python {
            crate::python::validate(source)?;
        }
        if let Some(edits) = &self.query {
            for edit in edits {
                if edit.name.is_empty() {
                    return Err(Error::bad("Query edit name must not be empty"));
                }
                if edit.regex {
                    if let Some(search) = &edit.search {
                        regex::Regex::new(search).map_err(Error::bad)?;
                    }
                }
            }
        }
        if let Some(edits) = &self.header_edits {
            for edit in edits {
                hyper::header::HeaderName::from_bytes(edit.name.as_bytes()).map_err(Error::bad)?;
                if let Some(value) = &edit.value {
                    hyper::header::HeaderValue::from_str(value).map_err(Error::bad)?;
                }
                if edit.regex {
                    if let Some(search) = &edit.search {
                        regex::Regex::new(search).map_err(Error::bad)?;
                    }
                }
            }
        }
        if let Some(replacements) = &self.body_replacements {
            for replacement in replacements {
                if replacement.search.is_empty() {
                    return Err(Error::bad("Body replacement search must not be empty"));
                }
                if replacement.regex {
                    regex::Regex::new(&replacement.search).map_err(Error::bad)?;
                }
            }
        }
        if let Some(replace) = &self.replace {
            if replace.regex {
                regex::Regex::new(&replace.search).map_err(Error::bad)?;
            }
        }

        if let Some(headers) = &self.headers {
            self.headers = Some(normalize_headers(headers, true)?);
        }
        if self
            .replace
            .as_ref()
            .is_some_and(|value| value.search.is_empty())
        {
            return Err(Error::bad("replace.search must not be empty"));
        }
        if let Some(value) = &self.redirect {
            if phase != "request" {
                return Err(Error::bad("response: unknown field redirect"));
            }
            self.redirect = Some(parse_url(value)?.to_string());
        }
        if let Some(status) = self.status {
            if phase != "response" {
                return Err(Error::bad("request: unknown field status"));
            }
            if !(200..=599).contains(&status) {
                return Err(Error::bad("status must be from 200 to 599"));
            }
        }
        if let Some(source) = &self.script {
            javascript(source, None, None, true, 100)?;
        }
        Ok(())
    }
    pub fn needs_body(&self) -> bool {
        self.edits.as_ref().is_some_and(|edits| {
            edits
                .iter()
                .any(|edit| edit.enabled && edit.r#type == "body")
        }) || self.body.is_some()
            || self.body_file.is_some()
            || self.python.is_some()
            || self.replace.is_some()
            || self
                .body_replacements
                .as_ref()
                .is_some_and(|v| !v.is_empty())
            || self.script.is_some()
            || self.breakpoint == Some(true)
    }
    fn rewrite(
        &self,
        headers: &mut Headers,
        body: &mut Option<String>,
        binary: &mut Option<Vec<u8>>,
    ) -> Result<()> {
        if let Some(patch) = &self.headers {
            if self.replace_headers == Some(true) || self.mode.as_deref() == Some("replace") {
                headers.clear();
            }
            for (name, value) in patch {
                if value.is_null() {
                    headers.remove(name);
                } else {
                    headers.insert(name.clone(), value.clone());
                }
            }
        }
        if let Some(edits) = &self.header_edits {
            for edit in edits {
                let name = edit.name.to_ascii_lowercase();
                if edit.remove {
                    headers.remove(&name);
                    continue;
                }
                if let Some(value) = &edit.value {
                    headers.insert(name, json!(value));
                    continue;
                }
                if let Some(search) = &edit.search {
                    if let Some(existing) = headers.get_mut(&name) {
                        let change = |value: &str| -> Result<String> {
                            Ok(if edit.regex {
                                regex::Regex::new(search)
                                    .map_err(Error::bad)?
                                    .replace_all(value, edit.replacement.as_deref().unwrap_or(""))
                                    .into_owned()
                            } else {
                                value.replace(search, edit.replacement.as_deref().unwrap_or(""))
                            })
                        };
                        *existing = match existing {
                            Value::String(value) => json!(change(value)?),
                            Value::Array(values) => Value::Array(
                                values
                                    .iter()
                                    .map(|v| change(v.as_str().unwrap_or("")).map(Value::String))
                                    .collect::<Result<Vec<_>>>()?,
                            ),
                            _ => return Err(Error::bad("Invalid header value")),
                        };
                    }
                }
            }
            *headers = normalize_headers(headers, false)?;
        }
        if let Some(value) = &self.body {
            *body = Some(value.clone());
            *binary = None;
        }
        if let Some(path) = &self.body_file {
            use std::io::Read;
            let mut data = Vec::new();
            fs::File::open(path)
                .and_then(|file| file.take(16 * 1024 * 1024 + 1).read_to_end(&mut data))
                .map_err(|error| Error::bad(format!("Cannot read bodyFile {path}: {error}")))?;
            if data.len() > 16 * 1024 * 1024 {
                return Err(Error::new(413, "bodyFile exceeds 16 MiB"));
            }
            match String::from_utf8(data) {
                Ok(text) => {
                    *body = Some(text);
                    *binary = None;
                }
                Err(error) => {
                    *body = None;
                    *binary = Some(error.into_bytes());
                }
            }
        }
        if let Some(value) = &self.replace {
            let original = body
                .as_ref()
                .ok_or_else(|| Error::bad("Body rewrite requires a buffered text body"))?;
            *body = Some(if value.regex {
                regex::Regex::new(&value.search)
                    .map_err(Error::bad)?
                    .replace_all(original, value.replacement.as_str())
                    .into_owned()
            } else {
                original.replace(&value.search, &value.replacement)
            });
        }
        if let Some(replacements) = &self.body_replacements {
            for replacement in replacements {
                let original = body.as_ref().ok_or_else(|| {
                    Error::bad(
                        "Text body replacement requires UTF-8; use Python to edit binary bodies",
                    )
                })?;
                *body = Some(if replacement.regex {
                    regex::Regex::new(&replacement.search)
                        .map_err(Error::bad)?
                        .replace_all(original, replacement.replacement.as_str())
                        .into_owned()
                } else {
                    original.replace(&replacement.search, &replacement.replacement)
                });
            }
        }
        Ok(())
    }
    fn query_edit(&self, req: &mut RequestData) -> Result<()> {
        let Some(edits) = &self.query else {
            return Ok(());
        };
        let mut url = parse_url(&req.url)?;
        // Keep each untouched raw component byte-for-byte, including duplicate keys and bare flags.
        let mut parts: Vec<String> = url
            .query()
            .map(|q| q.split('&').map(str::to_owned).collect())
            .unwrap_or_default();
        for edit in edits {
            let mut found = false;
            parts = parts
                .into_iter()
                .filter_map(|raw| {
                    let (name, value) = url::form_urlencoded::parse(raw.as_bytes())
                        .next()
                        .unwrap_or_default();
                    if name != edit.name {
                        return Some(raw);
                    };
                    found = true;
                    if edit.remove {
                        return None;
                    };
                    let next = if let Some(value) = &edit.value {
                        value.clone()
                    } else if let Some(search) = &edit.search {
                        if edit.regex {
                            regex::Regex::new(search)
                                .unwrap()
                                .replace_all(&value, edit.replacement.as_deref().unwrap_or(""))
                                .into_owned()
                        } else {
                            value.replace(search, edit.replacement.as_deref().unwrap_or(""))
                        }
                    } else {
                        return Some(raw);
                    };
                    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                    serializer.append_pair(&name, &next);
                    Some(serializer.finish())
                })
                .collect();
            if !found && !edit.remove {
                if let Some(value) = &edit.value {
                    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                    serializer.append_pair(&edit.name, value);
                    parts.push(serializer.finish());
                }
            }
        }
        let query = parts.join("&");
        url.set_query(if parts.is_empty() { None } else { Some(&query) });
        req.url = url.to_string();
        Ok(())
    }
}

fn default_method() -> String {
    "*".into()
}
fn enabled() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Match {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclude: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
    r#match: Match,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mock: Option<Value>,
}
impl Rule {
    pub fn interceptors(&self) -> Vec<&'static str> {
        let actions: Vec<_> = self.request.iter().chain(self.response.iter()).collect();
        let mut kinds = vec![];
        if self.mock.is_some()
            || actions.iter().any(|a| {
                a.mode.is_some()
                    || a.method.is_some()
                    || a.path.is_some()
                    || a.headers.is_some()
                    || a.header_edits.is_some()
                    || a.query.is_some()
                    || a.body.is_some()
                    || a.body_file.is_some()
                    || a.replace.is_some()
                    || a.body_replacements.is_some()
                    || a.edits.is_some()
                    || a.status.is_some()
            })
        {
            kinds.push("重写");
        }
        if actions.iter().any(|a| a.breakpoint == Some(true)) {
            kinds.push("断点");
        }
        if actions
            .iter()
            .any(|a| a.python.is_some() || a.script.is_some())
        {
            kinds.push("脚本");
        }
        kinds
    }
    pub fn request_body(&self) -> bool {
        self.request.as_ref().is_some_and(Action::needs_body)
            || self
                .response
                .as_ref()
                .is_some_and(|a| a.script.is_some() || a.python.is_some())
    }
    pub fn replaces_request_body(&self) -> bool {
        self.request
            .as_ref()
            .is_some_and(|a| a.body.is_some() || a.body_file.is_some())
    }
    pub fn replaces_response_body(&self) -> bool {
        self.response
            .as_ref()
            .is_some_and(|a| a.body.is_some() || a.body_file.is_some())
    }
    pub fn response_body(&self) -> bool {
        self.response.as_ref().is_some_and(Action::needs_body)
    }
    fn matches(&self, req: &RequestData) -> bool {
        self.enabled
            && (self.r#match.method == "*" || self.r#match.method == req.method)
            && wildcard(&self.r#match.url, &req.url)
            && !self
                .r#match
                .exclude
                .iter()
                .any(|pattern| wildcard(pattern, &req.url))
    }
}

pub fn wildcard(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi, mut star, mut mark) = (0, 0, None, 0);
    while vi < v.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = vi;
        } else if let Some(position) = star {
            mark += 1;
            vi = mark;
            pi = position + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn validate(values: Value) -> Result<Vec<Rule>> {
    let array = values
        .as_array()
        .ok_or_else(|| Error::bad("rules must be an array"))?;
    let mut ids = HashSet::new();
    let mut result = Vec::new();
    for (index, value) in array.iter().enumerate() {
        for phase in ["request", "response", "mock"] {
            if let Some(action) = value.get(phase) {
                let object = action
                    .as_object()
                    .ok_or_else(|| Error::bad(format!("{phase} must be an object")))?;
                if object.values().any(Value::is_null) {
                    return Err(Error::bad(format!("{phase} fields must not be null")));
                }
            }
        }
        let mut rule: Rule = serde_json::from_value(value.clone())?;
        if value.get("id").is_none() {
            rule.id = Uuid::new_v4().to_string();
        }
        if value.get("name").is_none() {
            rule.name = format!("Rule {}", index + 1);
        }
        if rule.id.is_empty() || !ids.insert(rule.id.clone()) {
            return Err(Error::bad("Rule IDs must be nonempty and unique"));
        }
        if rule.r#match.url.is_empty() || rule.r#match.url.chars().any(char::is_whitespace) {
            return Err(Error::bad(
                "match.url must be a nonempty URL wildcard without whitespace",
            ));
        }
        if rule
            .r#match
            .exclude
            .iter()
            .any(|v| v.is_empty() || v.chars().any(char::is_whitespace))
        {
            return Err(Error::bad(
                "Excluded URL patterns must be nonempty without whitespace",
            ));
        }
        if rule.r#match.method == "ANY" {
            rule.r#match.method = "*".into();
        }
        if rule.r#match.method != "*" {
            rule.r#match.method = method(&rule.r#match.method)?;
        }
        if let Some(change) = &mut rule.request {
            change.validate("request")?;
        }
        if let Some(change) = &mut rule.response {
            change.validate("response")?;
        }
        if let Some(mock) = &rule.mock {
            let mut mock = mock.clone();
            let object = mock
                .as_object_mut()
                .ok_or_else(|| Error::bad("mock must be an object"))?;
            object.entry("status").or_insert(json!(200));
            if !object.contains_key("bodyBase64") {
                object.entry("body").or_insert(json!(""));
            }
            let response: ResponseData = serde_json::from_value(mock)?;
            rule.mock = Some(serde_json::to_value(response.validate()?)?);
        }
        if rule.request.is_none() && rule.response.is_none() && rule.mock.is_none() {
            return Err(Error::bad("Rule needs request, response, or mock"));
        }
        result.push(rule);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;

fn javascript(
    source: &str,
    request: Option<&RequestData>,
    response: Option<&ResponseData>,
    compile: bool,
    timeout: u64,
) -> Result<Value> {
    let runtime = Runtime::new().map_err(Error::bad)?;
    // The engine has no host bindings; limits contain accidents in trusted local scripts.
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime).map_err(Error::bad)?;
    context.with(|ctx| {
        let code = if compile {
            format!("new Function({}); 'null'", serde_json::to_string(&format!("\"use strict\";\n{source}"))?)
        } else {
            format!("\"use strict\";\nglobalThis.request = {}; globalThis.response = {};\nconst noCodegen = function() {{ throw new Error('Code generation from strings disallowed'); }}; Function.prototype.constructor = noCodegen; globalThis.Function = noCodegen; globalThis.eval = noCodegen;\n{source}\n;JSON.stringify({{request, response}})", serde_json::to_string(&request)?, serde_json::to_string(&response)?)
        };
        let result: rquickjs::Result<String> = ctx.eval(code);
        match result {
            Ok(encoded) => Ok(serde_json::from_str(&encoded)?),
            Err(error) => {
                if Instant::now() >= deadline { return Err(Error::bad(format!("Script execution timed out after {timeout} ms"))); }
                let exception = ctx.catch();
                let message = if let Some(exception) = exception.into_exception() { exception.message().unwrap_or_else(|| error.to_string()) } else { error.to_string() };
                Err(Error::bad(format!("JavaScript: {message}")))
            }
        }
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    id: String,
    rule_id: String,
    rule_name: String,
    phase: String,
    request: RequestData,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<ResponseData>,
    expires_at: String,
}
type PausedResult = (RequestData, Option<ResponseData>);
struct Entry {
    item: Pending,
    sender: oneshot::Sender<Result<PausedResult>>,
}
struct Saved {
    rules: Vec<Rule>,
    error: Option<String>,
}
pub struct Rules {
    path: PathBuf,
    saved: Mutex<Saved>,
    pending: Mutex<BTreeMap<String, Entry>>,
    breakpoint_ms: u64,
    script_ms: u64,
    settings: Mutex<crate::settings::Settings>,
}
struct PauseGuard {
    rules: Arc<Rules>,
    id: String,
}
impl Drop for PauseGuard {
    fn drop(&mut self) {
        self.rules.pending.lock().unwrap().remove(&self.id);
    }
}

impl Rules {
    pub fn open(directory: &Path, breakpoint_ms: u64, script_ms: u64) -> Arc<Self> {
        let path = directory.join("rules.json");
        let loaded = (|| -> Result<Vec<Rule>> {
            let data: Value = serde_json::from_slice(&fs::read(&path)?)?;
            let object = data
                .as_object()
                .ok_or_else(|| Error::bad("Saved rules must be an object"))?;
            if object.len() != 1 {
                return Err(Error::bad("Saved rules: unknown field"));
            }
            validate(data["rules"].clone())
        })();
        let (rules, error) = match loaded {
            Ok(rules) => (rules, None),
            Err(_) if !path.exists() => (vec![], None),
            Err(error) => (
                vec![],
                Some(format!(
                    "Cannot load {}: {error}. The file has been preserved.",
                    path.display()
                )),
            ),
        };
        Arc::new(Self {
            path,
            saved: Mutex::new(Saved { rules, error }),
            pending: Mutex::new(BTreeMap::new()),
            breakpoint_ms,
            script_ms,
            settings: Mutex::new(crate::settings::Settings::default()),
        })
    }
    pub fn get(&self) -> Value {
        let saved = self.saved.lock().unwrap();
        json!({"rules": saved.rules, "error": saved.error})
    }
    #[cfg(test)]
    pub fn set(&self, values: Value) -> Result<Value> {
        self.set_expected(values, None)
    }
    pub fn set_expected(&self, values: Value, expected: Option<&Value>) -> Result<Value> {
        let rules = validate(values)?;
        let mut saved = self.saved.lock().unwrap();
        if expected
            .is_some_and(|value| serde_json::to_value(&saved.rules).ok().as_ref() != Some(value))
        {
            return Err(Error::new(
                409,
                "Rules changed since they were loaded; reload before saving",
            ));
        }
        atomic_write(
            &self.path,
            &serde_json::to_vec_pretty(&json!({"rules": rules}))?,
        )?;
        *saved = Saved { rules, error: None };
        Ok(json!({"rules": saved.rules, "error": saved.error}))
    }
    pub fn set_enabled(&self, id: &str, enabled: bool, expected: bool) -> Result<Value> {
        let mut saved = self.saved.lock().unwrap();
        if saved.error.is_some() {
            return Err(Error::bad("Reload valid rules before changing their state"));
        }
        let index = saved
            .rules
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| Error::new(404, "Rule not found"))?;
        if saved.rules[index].enabled != expected {
            return Err(Error::new(
                409,
                "Rule state changed; reload before changing it",
            ));
        }
        // Actions are already validated. Toggling does not recompile every
        // script and regex in the table, nor send all sources over the API.
        let mut rules = saved.rules.clone();
        rules[index].enabled = enabled;
        atomic_write(
            &self.path,
            &serde_json::to_vec_pretty(&json!({"rules":rules}))?,
        )?;
        saved.rules = rules;
        Ok(json!({"id":id,"enabled":enabled}))
    }
    pub fn configure(&self, settings: &crate::settings::Settings) {
        *self.settings.lock().unwrap() = settings.clone();
    }
    fn snapshot(&self) -> Vec<Rule> {
        let settings = self.settings.lock().unwrap().clone();
        self.saved
            .lock()
            .unwrap()
            .rules
            .iter()
            .cloned()
            .map(|mut rule| {
                if !settings.rewrite_enabled {
                    rule.mock = None;
                }
                for action in [&mut rule.request, &mut rule.response]
                    .into_iter()
                    .flatten()
                {
                    if !settings.rewrite_enabled {
                        let script = action.script.take();
                        let python = action.python.take();
                        let breakpoint = action.breakpoint;
                        *action = Action::default();
                        action.script = script;
                        action.python = python;
                        action.breakpoint = breakpoint;
                    }
                    if !settings.script_enabled {
                        action.script = None;
                        action.python = None;
                    }
                    if !settings.breakpoint_enabled {
                        action.breakpoint = None;
                    }
                }
                rule
            })
            .collect()
    }
    #[cfg(test)]
    pub fn matching(&self, request: &RequestData) -> Vec<Rule> {
        self.snapshot()
            .into_iter()
            .filter(|r| r.matches(request))
            .collect()
    }
    pub fn prepare(&self, mut req: RequestData) -> Result<(RequestData, Vec<Rule>)> {
        let snapshot = self.snapshot();
        let redirect = snapshot.iter().find(|rule| {
            rule.matches(&req) && rule.request.as_ref().is_some_and(|a| a.redirect.is_some())
        });
        let legacy = redirect
            .filter(|rule| {
                rule.request.as_ref().is_some_and(|a| {
                    a.mode.is_none()
                        && (a.body.is_some()
                            || a.body_file.is_some()
                            || a.headers.is_some()
                            || a.header_edits.is_some()
                            || a.replace.is_some()
                            || a.body_replacements.is_some()
                            || a.edits.is_some()
                            || a.script.is_some()
                            || a.python.is_some()
                            || a.breakpoint.is_some())
                }) || rule.response.is_some()
                    || rule.mock.is_some()
            })
            .map(|r| r.id.clone());
        if let Some(rule) = redirect {
            Self::redirect(&mut req, rule.request.as_ref().unwrap())?;
        }
        let mut selected: Vec<Rule> = snapshot
            .into_iter()
            .filter(|rule| rule.matches(&req) || legacy.as_deref() == Some(&rule.id))
            .collect();
        for rule in &mut selected {
            if let Some(action) = &mut rule.request {
                action.redirect = None;
            }
        }
        Ok((req, selected))
    }
    fn redirect(req: &mut RequestData, change: &Action) -> Result<()> {
        if let Some(destination) = &change.redirect {
            let mut url = parse_url(destination)?;
            if destination.ends_with("/*") {
                let original = parse_url(&req.url)?;
                let path = format!(
                    "{}{}",
                    url.path().trim_end_matches('*'),
                    original.path().trim_start_matches('/')
                );
                url.set_path(&path);
                url.set_query(original.query());
            }
            if change.preserve_host != Some(true) {
                req.headers.insert(
                    "host".into(),
                    json!(&url[url::Position::BeforeHost..url::Position::AfterPort]),
                );
            } else if !req.headers.contains_key("host") {
                let original = parse_url(&req.url)?;
                req.headers.insert(
                    "host".into(),
                    json!(&original[url::Position::BeforeHost..url::Position::AfterPort]),
                );
            }
            req.url = url.to_string();
        }
        Ok(())
    }
    pub fn pending(&self) -> Value {
        json!({"pending": self.pending.lock().unwrap().values().map(|e| e.item.clone()).collect::<Vec<_>>()})
    }
    pub fn resume(&self, id: &str, edit: Value) -> Result<Value> {
        if ["request", "response"]
            .iter()
            .any(|key| edit.get(key).is_some_and(Value::is_null))
        {
            return Err(Error::bad("Breakpoint request/response must be an object"));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Edit {
            action: String,
            request: Option<RequestData>,
            response: Option<ResponseData>,
        }
        let mut pending = self.pending.lock().unwrap();
        let entry = pending
            .get(id)
            .ok_or_else(|| Error::new(404, "Breakpoint not found or already resumed"))?;
        let edit: Edit = serde_json::from_value(edit)?;
        if !["continue", "drop"].contains(&edit.action.as_str()) {
            return Err(Error::bad("Breakpoint action must be continue or drop"));
        }
        if entry.item.phase == "request" && edit.response.is_some() {
            return Err(Error::bad("A request breakpoint cannot edit response"));
        }
        if entry.item.phase == "response" && edit.request.is_some() {
            return Err(Error::bad("A response breakpoint cannot edit request"));
        }
        let req = edit
            .request
            .unwrap_or_else(|| entry.item.request.clone())
            .validate()?;
        let upgrading = entry
            .item
            .response
            .as_ref()
            .is_some_and(|r| r.status == 101);
        let res = edit
            .response
            .or_else(|| entry.item.response.clone())
            .map(|response| {
                if upgrading {
                    response.validate_for_upgrade()
                } else {
                    response.validate()
                }
            })
            .transpose()?;
        let entry = pending.remove(id).unwrap();
        let result = if edit.action == "drop" {
            Err(Error::upstream(format!(
                "Dropped at breakpoint {}",
                entry.item.rule_name
            )))
        } else {
            Ok((req, res))
        };
        entry
            .sender
            .send(result)
            .map_err(|_| Error::new(404, "Breakpoint request disconnected"))?;
        Ok(json!({"ok": true}))
    }
    async fn pause(
        self: &Arc<Self>,
        rule: &Rule,
        phase: &str,
        req: RequestData,
        res: Option<ResponseData>,
    ) -> Result<PausedResult> {
        let id = Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        let item = Pending {
            id: id.clone(),
            rule_id: rule.id.clone(),
            rule_name: rule.name.clone(),
            phase: phase.into(),
            request: req,
            response: res,
            expires_at: (Utc::now() + chrono::Duration::milliseconds(self.breakpoint_ms as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        self.pending
            .lock()
            .unwrap()
            .insert(id.clone(), Entry { item, sender });
        let _cleanup = PauseGuard {
            rules: self.clone(),
            id,
        };
        tokio::time::timeout(Duration::from_millis(self.breakpoint_ms), receiver)
            .await
            .map_err(|_| {
                Error::new(
                    504,
                    format!(
                        "Breakpoint {} timed out after {} ms",
                        rule.name, self.breakpoint_ms
                    ),
                )
            })?
            .map_err(|_| Error::new(503, "Rules engine closed while waiting at a breakpoint"))?
    }
    #[cfg(test)]
    pub async fn apply_request(
        self: &Arc<Self>,
        req: RequestData,
        selected: &[Rule],
    ) -> Result<(RequestData, Option<ResponseData>)> {
        self.apply_request_flow(req, selected, &mut json!({})).await
    }
    pub async fn apply_request_flow(
        self: &Arc<Self>,
        mut req: RequestData,
        selected: &[Rule],
        shared: &mut Value,
    ) -> Result<(RequestData, Option<ResponseData>)> {
        let mut ordered: Vec<&Rule> = selected.iter().collect();
        ordered.sort_by_key(|r| {
            if r.request
                .as_ref()
                .is_some_and(|a| a.mode.as_deref() == Some("replace"))
            {
                0
            } else {
                1
            }
        });
        let mut redirected = false;
        for rule in ordered {
            if let Some(change) = &rule.request {
                change.rewrite(&mut req.headers, &mut req.body, &mut req.binary_body)?;
                if req.binary_body.is_some() && change.script.is_some() {
                    return Err(Error::bad(
                        "JavaScript body editing requires UTF-8 text; Python supports binary data",
                    ));
                }
                if !redirected && change.redirect.is_some() {
                    Self::redirect(&mut req, change)?;
                    redirected = true;
                }
                if let Some(value) = &change.method {
                    req.method = value.clone();
                }
                if let Some(path) = &change.path {
                    let url = parse_url(&req.url)?;
                    req.url = url.join(path).map_err(Error::bad)?.to_string();
                }
                change.query_edit(&mut req)?;
                if let Some(edits) = &change.edits {
                    for edit in edits {
                        edit.apply(
                            Some(&mut req.url),
                            &mut req.headers,
                            &mut req.body,
                            &mut req.binary_body,
                        )?;
                    }
                }
                if let Some(source) = &change.python {
                    let (executable, python_timeout) = {
                        let settings = self.settings.lock().unwrap();
                        (
                            settings.python_executable.clone(),
                            settings.python_timeout_ms,
                        )
                    };
                    let (updated, _, context) = crate::python::execute_for_rule(
                        Some(&rule.id),
                        &executable,
                        source,
                        &req,
                        None,
                        &shared.get(&rule.id).cloned().unwrap_or(Value::Null),
                        python_timeout,
                    )
                    .await?;
                    req = updated.validate()?;
                    shared[&rule.id] = context;
                }
                if let Some(source) = &change.script {
                    req = serde_json::from_value::<RequestData>(
                        javascript(source, Some(&req), None, false, self.script_ms)?["request"]
                            .clone(),
                    )?
                    .validate()?;
                }
                if change.breakpoint == Some(true) {
                    req = self.pause(rule, "request", req, None).await?.0;
                }
            }
            if let Some(mock) = &rule.mock {
                return Ok((req, Some(serde_json::from_value(mock.clone())?)));
            }
        }
        Ok((req, None))
    }
    #[cfg(test)]
    pub async fn apply_response(
        self: &Arc<Self>,
        req: &RequestData,
        res: ResponseData,
        selected: &[Rule],
    ) -> Result<ResponseData> {
        self.apply_response_flow(req, res, selected, &mut json!({}))
            .await
    }
    pub async fn apply_response_flow(
        self: &Arc<Self>,
        req: &RequestData,
        mut res: ResponseData,
        selected: &[Rule],
        shared: &mut Value,
    ) -> Result<ResponseData> {
        let upgrading = res.status == 101;
        let mut ordered: Vec<&Rule> = selected.iter().collect();
        ordered.sort_by_key(|r| {
            if r.response
                .as_ref()
                .is_some_and(|a| a.mode.as_deref() == Some("replace"))
            {
                0
            } else {
                1
            }
        });
        for rule in ordered {
            let Some(change) = &rule.response else {
                continue;
            };
            change.rewrite(&mut res.headers, &mut res.body, &mut res.binary_body)?;
            if res.binary_body.is_some() && change.script.is_some() {
                return Err(Error::bad(
                    "JavaScript body editing requires UTF-8 text; Python supports binary data",
                ));
            }
            if let Some(status) = change.status {
                res.status = status;
            }
            if let Some(edits) = &change.edits {
                for edit in edits {
                    edit.apply(None, &mut res.headers, &mut res.body, &mut res.binary_body)?;
                }
            }
            if let Some(source) = &change.python {
                let (executable, python_timeout) = {
                    let settings = self.settings.lock().unwrap();
                    (
                        settings.python_executable.clone(),
                        settings.python_timeout_ms,
                    )
                };
                let (_, updated, context) = crate::python::execute_for_rule(
                    Some(&rule.id),
                    &executable,
                    source,
                    req,
                    Some(&res),
                    &shared.get(&rule.id).cloned().unwrap_or(Value::Null),
                    python_timeout,
                )
                .await?;
                let updated = updated
                    .ok_or_else(|| Error::bad("Python response hook returned no response"))?;
                res = if upgrading {
                    updated.validate_for_upgrade()?
                } else {
                    updated.validate()?
                };
                shared[&rule.id] = context;
            }
            if let Some(source) = &change.script {
                let updated = serde_json::from_value::<ResponseData>(
                    javascript(source, Some(req), Some(&res), false, self.script_ms)?["response"]
                        .clone(),
                )?;
                res = if upgrading {
                    updated.validate_for_upgrade()?
                } else {
                    updated.validate()?
                };
            }
            if change.breakpoint == Some(true) {
                res = self
                    .pause(rule, "response", req.clone(), Some(res))
                    .await?
                    .1
                    .unwrap();
            }
        }
        Ok(res)
    }
    pub fn close(&self) {
        self.pending.lock().unwrap().clear();
    }
}
