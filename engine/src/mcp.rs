//! Local MCP transports share the same tools and authenticated control API.
use crate::wire::{Error, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::TcpStream;

#[cfg(all(test, unix))]
use std::os::unix::fs::PermissionsExt;

pub const MAX_MESSAGE: u64 = 20 * 1024 * 1024;
pub const HTTP_PROTOCOLS: &[&str] = &["2025-03-26", "2025-06-18", "2025-11-25"];

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessToken {
    pub id: String,
    pub name: String,
    pub token: String,
    #[serde(
        default,
        rename = "lastUsedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct AccessTokens {
    path: PathBuf,
    pub items: Vec<AccessToken>,
}

fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn token_name(input: &Value) -> Result<String> {
    let name = input["name"]
        .as_str()
        .ok_or_else(|| Error::bad("MCP token name required"))?
        .trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(Error::bad(
            "MCP token name must contain 1–100 characters without control characters",
        ));
    }
    Ok(name.into())
}

impl AccessTokens {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("mcp-token.json");
        let mut store = Self {
            path,
            items: Vec::new(),
        };
        let bytes = match fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                store.save(&[])?;
                return Ok(store);
            }
            Err(error) => return Err(error.into()),
        };
        let value: Value = serde_json::from_slice(&bytes)?;
        if value.as_object().is_some_and(|v| v.len() == 1) && value.get("token").is_some() {
            let token = value["token"]
                .as_str()
                .filter(|token| valid_token(token))
                .ok_or_else(|| Error::bad("Invalid saved MCP token; file preserved"))?;
            store.items.push(AccessToken {
                id: uuid::Uuid::new_v4().to_string(),
                name: "默认令牌".into(),
                token: token.into(),
                last_used_at: None,
            });
            store.save(&store.items)?;
        } else {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Saved {
                version: u32,
                tokens: Vec<AccessToken>,
            }
            let saved: Saved = serde_json::from_value(value)?;
            if saved.version != 1 || saved.tokens.len() > 256 {
                return Err(Error::bad("Invalid MCP token list; file preserved"));
            }
            let mut ids = std::collections::HashSet::new();
            let mut tokens = std::collections::HashSet::new();
            for item in &saved.tokens {
                if uuid::Uuid::parse_str(&item.id).is_err()
                    || !valid_token(&item.token)
                    || token_name(&json!({"name":item.name}))? != item.name
                    || !ids.insert(&item.id)
                    || !tokens.insert(&item.token)
                {
                    return Err(Error::bad("Invalid MCP token list; file preserved"));
                }
            }
            store.items = saved.tokens;
            crate::storage::private_file(&store.path)?;
        }
        Ok(store)
    }

    fn save(&self, items: &[AccessToken]) -> Result<()> {
        crate::storage::atomic_write(
            &self.path,
            &serde_json::to_vec(&json!({"version":1,"tokens":items}))?,
        )
    }

    pub fn authorized_token(&self, supplied: &[u8]) -> Option<&str> {
        use subtle::{ConditionallySelectable, ConstantTimeEq};
        let mut matched = 0;
        for (index, item) in self.items.iter().enumerate() {
            let equal = supplied.ct_eq(format!("Bearer {}", item.token).as_bytes());
            matched = u32::conditional_select(&matched, &(index as u32 + 1), equal);
        }
        self.items
            .get((matched as usize).wrapping_sub(1))
            .map(|item| item.id.as_str())
    }

    pub fn record_access(&mut self, id: &str) -> Result<()> {
        let now = chrono::Utc::now();
        let Some(index) = self.items.iter().position(|item| item.id == id) else {
            return Ok(());
        };
        if self.items[index]
            .last_used_at
            .is_none_or(|time| now.signed_duration_since(time).num_seconds().abs() >= 60)
        {
            // shortcut: one recorded access per minute per token, revisit if exact session history is needed.
            let previous = self.items[index].last_used_at.replace(now);
            if let Err(error) = self.save(&self.items) {
                self.items[index].last_used_at = previous;
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn update(&mut self, method: &str, id: Option<&str>, input: Value) -> Result<()> {
        let fields = input
            .as_object()
            .ok_or_else(|| Error::bad("MCP token object required"))?;
        let allowed: &[&str] = if method == "DELETE" { &[] } else { &["name"] };
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::bad("Unexpected MCP token field"));
        }
        let mut next = self.items.clone();
        match (method, id) {
            ("POST", None) => {
                if next.len() >= 256 {
                    return Err(Error::bad("MCP token limit reached (256)"));
                }
                next.push(AccessToken {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: token_name(&input)?,
                    token: uuid::Uuid::new_v4().simple().to_string(),
                    last_used_at: None,
                });
            }
            ("PATCH" | "DELETE", Some(id)) => {
                let index = next
                    .iter()
                    .position(|item| item.id == id)
                    .ok_or_else(|| Error::new(404, "MCP token not found"))?;
                if method == "DELETE" {
                    next.remove(index);
                } else {
                    next[index].name = token_name(&input)?;
                }
            }
            _ => return Err(Error::bad("Invalid MCP token operation")),
        }
        self.save(&next)?;
        self.items = next;
        Ok(())
    }
}

pub fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn directory() -> Result<PathBuf> {
    if let Some(path) = env::var_os("RECHO_DATA_DIR") {
        return Ok(path.into());
    }
    Ok(PathBuf::from(
        env::var_os(if cfg!(windows) {
            "LOCALAPPDATA"
        } else {
            "HOME"
        })
        .ok_or_else(|| Error::bad("HOME or RECHO_DATA_DIR is required"))?,
    )
    .join(crate::identity::DATA_DIRECTORY))
}

async fn api(directory: &Path, verb: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let descriptor: Value =
        serde_json::from_slice(&fs::read(directory.join("control.json")).map_err(|_| {
            Error::new(
                503,
                format!("Open {} before using MCP", crate::identity::NAME),
            )
        })?)?;
    let url = url::Url::parse(descriptor["url"].as_str().unwrap_or("")).map_err(Error::bad)?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none() {
        return Err(Error::bad("Invalid local control descriptor"));
    }
    let token = descriptor["token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::bad("Missing local control token"))?;
    request_json(&url.join(path).map_err(Error::bad)?, token, verb, body)
        .await?
        .ok_or_else(|| Error::bad("Empty local API response"))
}

async fn request_json(
    url: &url::Url,
    token: &str,
    verb: &str,
    body: Option<Value>,
) -> Result<Option<Value>> {
    tokio::time::timeout(Duration::from_secs(120), async {
        let host = url
            .host_str()
            .ok_or_else(|| Error::bad("Missing MCP host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| Error::bad("Missing MCP port"))?;
        let socket = TcpStream::connect((host, port)).await?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .map_err(Error::upstream)?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        struct Guard(tokio::task::JoinHandle<()>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _driver = Guard(driver);
        let request = Request::builder()
            .method(verb)
            .uri(&url[url::Position::BeforePath..])
            .header(
                "host",
                &url[url::Position::BeforeHost..url::Position::AfterPort],
            )
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(
                body.map(|v| serde_json::to_vec(&v))
                    .transpose()?
                    .unwrap_or_default(),
            )))
            .map_err(Error::bad)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(Error::upstream)?;
        let status = response.status();
        let bytes = http_body_util::Limited::new(response.into_body(), MAX_MESSAGE as usize)
            .collect()
            .await
            .map_err(Error::upstream)?
            .to_bytes();
        if status == hyper::StatusCode::ACCEPTED && bytes.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if value["jsonrpc"] == "2.0" && value["error"].is_object() {
            return Ok(Some(value));
        }
        if !status.is_success() {
            return Err(Error::new(
                status.as_u16(),
                value["error"].as_str().unwrap_or("Local API failed"),
            ));
        }
        Ok(Some(value))
    })
    .await
    .map_err(|_| Error::new(504, "Local API timed out"))?
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tools() -> Value {
    let string = json!({"type":"string"});
    let object = json!({"type":"object"});
    let mut specs = vec![
        (
            "capture_live_status",
            "Get proxy status and current captured sessions",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "capture_live_filter",
            "Find captured requests by URL/method/body text",
            schema(json!({"query":string}), &["query"]),
            true,
            false,
        ),
        (
            "capture_list_filter_list",
            "List saved capture-list display filters and their enabled state; does not filter MCP search results",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "capture_list_filter_create",
            "Create and enable a display-only filter for future captures: exact full URL AND a top-level JSON request-body member equal to body.equals. body is {member: string, equals: any JSON value}. Multiple applicable enabled filters match ANY. Existing captures and traffic are unchanged",
            schema(
                json!({"url":string,"body":schema(json!({"member":string,"equals":{}}), &["member", "equals"])}),
                &["url", "body"],
            ),
            false,
            false,
        ),
        (
            "capture_list_filter_set_enabled",
            "Enable or disable a saved capture-list display filter. Disabling reveals hidden captures. Reactivation also reapplies its previous decisions; captures made while disabled stay unaffected",
            schema(
                json!({"id":string,"enabled":{"type":"boolean"}}),
                &["id", "enabled"],
            ),
            false,
            false,
        ),
        (
            "capture_list_filter_delete",
            "Delete a saved display filter and reveal its hidden captures; never deletes captured requests",
            schema(json!({"id":string}), &["id"]),
            false,
            false,
        ),
        (
            "capture_live_get_by_id",
            "Get a live or saved request by its stable session ID (UUID), not the UI row number",
            schema(json!({"id":string}), &["id"]),
            true,
            false,
        ),
        (
            "capture_live_set_enabled",
            "Start or stop the local capture proxy",
            schema(json!({"enabled":{"type":"boolean"}}), &["enabled"]),
            false,
            false,
        ),
        (
            "capture_live_clear",
            "Delete all locally saved request history",
            schema(json!({}), &[]),
            false,
            true,
        ),
        (
            "history_list",
            "Search saved request history, newest first. Optional query matches metadata and body previews; offset applies after filtering. Each page has at most 200 records",
            schema(
                json!({"query":string,"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}),
                &[],
            ),
            true,
            false,
        ),
        (
            "history_get_by_id",
            "Get a saved request by its date and numeric ID as displayed on the desktop History page",
            schema(
                json!({"date":{"type":"string","format":"date","description":"Local date shown in the History sidebar (YYYY-MM-DD)"},"id":{"type":"integer","minimum":1,"description":"Numeric ID shown for that date"}}),
                &["date", "id"],
            ),
            true,
            false,
        ),
        (
            "history_delete",
            "Delete one saved request by ID",
            schema(json!({"id":string}), &["id"]),
            false,
            true,
        ),
        (
            "http_request",
            "Send an HTTP API request through the local engine",
            schema(
                json!({"url":string,"method":string,"headers":string,"body":string,"bodyBase64":string,"protocol":{"type":"string","enum":["HTTP/1.1","HTTP/2","HTTP/3"]},"pythonScript":string,"beforeScript":string,"afterScript":string,"variables":object}),
                &["url"],
            ),
            false,
            false,
        ),
        (
            "rules_list",
            "List local rewrite, Python/JavaScript and breakpoint rules",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "rule_create",
            "Create a rule using the documented local rule JSON schema",
            schema(json!({"rule":object}), &["rule"]),
            false,
            false,
        ),
        (
            "rule_update",
            "Replace a rule, requiring the original rule for conflict detection",
            schema(
                json!({"id":string,"original":object,"rule":object}),
                &["id", "original", "rule"],
            ),
            false,
            false,
        ),
        (
            "rule_delete",
            "Delete a rule, requiring the original rule for conflict detection",
            schema(json!({"id":string,"original":object}), &["id", "original"]),
            false,
            true,
        ),
        (
            "settings_get",
            "Get local SSL/access/gateway/mirror/upstream/reverse and history settings",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "settings_update",
            "Update local settings; does not alter macOS system proxy or certificate trust",
            schema(json!({"settings":object}), &["settings"]),
            false,
            false,
        ),
        (
            "breakpoints_list",
            "List requests currently paused at breakpoints",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "breakpoint_resume",
            "Continue or drop a paused request; edit the active phase only",
            schema(
                json!({"id":string,"action":{"type":"string","enum":["continue","drop"]},"request":object,"response":object}),
                &["id", "action"],
            ),
            false,
            false,
        ),
        (
            "script_template",
            "Get the supported Python capture hook template",
            schema(json!({}), &[]),
            true,
            false,
        ),
    ];
    specs.extend([
        (
            "environment_list",
            "List local API environments and the active environment",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "environment_get_by_id",
            "Get one environment",
            schema(json!({"id":string}), &["id"]),
            true,
            false,
        ),
        (
            "environment_get_active",
            "Get the active API environment",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "environment_create",
            "Create a local API environment",
            schema(json!({"name":string,"variables":object}), &["name"]),
            false,
            false,
        ),
        (
            "environment_update",
            "Update a local API environment",
            schema(
                json!({"id":string,"name":string,"variables":object}),
                &["id"],
            ),
            false,
            false,
        ),
        (
            "environment_delete",
            "Delete a local API environment",
            schema(json!({"id":string}), &["id"]),
            false,
            true,
        ),
        (
            "environment_select",
            "Select the API environment, empty id selects none",
            schema(json!({"id":string}), &["id"]),
            false,
            false,
        ),
        (
            "collection_list",
            "List persistent API collection folders",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "collection_structure",
            "Get the local API collection hierarchy",
            schema(json!({}), &[]),
            true,
            false,
        ),
        (
            "collection_get",
            "Get a collection and its saved APIs",
            schema(json!({"id":string}), &["id"]),
            true,
            false,
        ),
        (
            "collection_create",
            "Create an API collection folder",
            schema(json!({"name":string,"parent":string}), &["name"]),
            false,
            false,
        ),
        (
            "collection_update",
            "Rename an API collection folder",
            schema(json!({"id":string,"name":string}), &["id", "name"]),
            false,
            false,
        ),
        (
            "collection_delete",
            "Delete a collection folder and its saved APIs",
            schema(json!({"id":string}), &["id"]),
            false,
            true,
        ),
        (
            "collection_api_get",
            "Get a saved API from a collection",
            schema(json!({"id":string,"collectionId":string}), &["id"]),
            true,
            false,
        ),
        (
            "collection_api_add",
            "Save a request into an API collection",
            schema(
                json!({"collectionId":string,"request":object}),
                &["collectionId", "request"],
            ),
            false,
            false,
        ),
        (
            "collection_api_update",
            "Update a saved API request",
            schema(
                json!({"id":string,"collectionId":string,"request":object}),
                &["id", "request"],
            ),
            false,
            false,
        ),
        (
            "collection_api_delete",
            "Delete a saved API request",
            schema(json!({"id":string,"collectionId":string}), &["id"]),
            false,
            true,
        ),
        (
            "rest_http_create_from_url",
            "Open an HTTP request tab in the running application",
            schema(
                json!({"url":string,"name":string,"method":string}),
                &["url"],
            ),
            false,
            false,
        ),
        (
            "rest_http_create_from_curl",
            "Import a cURL command into a request tab without executing a shell",
            schema(json!({"command":string}), &["command"]),
            false,
            false,
        ),
        (
            "rest_http_update",
            "Update an existing HTTP request tab",
            schema(json!({"id":string,"request":object}), &["id", "request"]),
            false,
            false,
        ),
    ]);
    let mut dynamic = Vec::new();
    for (prefix, _) in [
        ("rewrite", "rewriteEnabled"),
        ("script", "scriptEnabled"),
        ("breakpoint", "breakpointEnabled"),
    ] {
        for (suffix, description, input, read, destructive) in [
            (
                "get_config",
                "Read category settings and rules",
                schema(json!({}), &[]),
                true,
                false,
            ),
            (
                "list",
                "List rules in this category",
                schema(json!({}), &[]),
                true,
                false,
            ),
            (
                "get_by_id",
                "Get a rule by its ID",
                schema(json!({"id":string}), &["id"]),
                true,
                false,
            ),
            (
                "set_enabled",
                "Enable or disable this rule category",
                schema(json!({"enabled":{"type":"boolean"}}), &["enabled"]),
                false,
                false,
            ),
            (
                "create",
                "Create a category rule using the local rule JSON schema",
                schema(json!({"rule":object}), &["rule"]),
                false,
                false,
            ),
            (
                "update",
                "Replace a rule with optimistic conflict detection",
                schema(
                    json!({"id":string,"original":object,"rule":object}),
                    &["id", "original", "rule"],
                ),
                false,
                false,
            ),
            (
                "delete",
                "Delete a rule with optimistic conflict detection",
                schema(json!({"id":string,"original":object}), &["id", "original"]),
                false,
                true,
            ),
        ] {
            dynamic.push(json!({"name":format!("capture_{prefix}_{suffix}"),"description":description,"inputSchema":input,"annotations":{"readOnlyHint":read,"destructiveHint":destructive,"idempotentHint":read,"openWorldHint":false}}));
        }
    }
    let mut result:Vec<Value> = specs.into_iter().map(|(name, description, input, read, destructive)| json!({"name":name,"description":description,"inputSchema":input,"annotations":{"readOnlyHint":read,"destructiveHint":destructive,"idempotentHint":read,"openWorldHint":name == "http_request"}})).collect();
    result.extend(dynamic);
    json!(result)
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .ok_or_else(|| Error::bad(format!("{key} must be a string")))
}
fn identifier(args: &Value) -> Result<&str> {
    let id = text_arg(args, "id")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::bad("Invalid record ID"));
    }
    Ok(id)
}

async fn call_tool(directory: &Path, name: &str, args: &Value) -> Result<Value> {
    // Enforce property names and required fields before any side effect.
    let definitions = tools();
    let spec = definitions
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .ok_or_else(|| Error::bad("Unknown tool"))?;
    let values = args
        .as_object()
        .ok_or_else(|| Error::bad("arguments must be an object"))?;
    let properties = spec["inputSchema"]["properties"].as_object().unwrap();
    if values.keys().any(|k| !properties.contains_key(k)) {
        return Err(Error::bad("Unknown tool argument"));
    }
    for key in spec["inputSchema"]["required"].as_array().unwrap() {
        if !values.contains_key(key.as_str().unwrap()) {
            return Err(Error::bad(format!("Missing argument {key}")));
        }
    }
    for (key, value) in values {
        let property = &properties[key];
        let valid = match property["type"].as_str().unwrap_or("") {
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "integer" => value.is_u64(),
            _ => false,
        };
        if !valid
            || property
                .get("enum")
                .is_some_and(|items| !items.as_array().unwrap().contains(value))
        {
            return Err(Error::bad(format!("Invalid argument {key}")));
        }
    }
    if name.starts_with("environment_")
        || name.starts_with("collection_")
        || name.starts_with("rest_http_")
    {
        return api(
            directory,
            "POST",
            "/workspace",
            Some(json!({"name":name,"arguments":args})),
        )
        .await;
    }
    for (category, setting) in [
        ("rewrite", "rewriteEnabled"),
        ("script", "scriptEnabled"),
        ("breakpoint", "breakpointEnabled"),
    ] {
        if let Some(action) = name.strip_prefix(&format!("capture_{category}_")) {
            let belongs = |rule: &Value| match category {
                "script" => ["request", "response"]
                    .iter()
                    .any(|p| rule[p].get("script").is_some() || rule[p].get("python").is_some()),
                "breakpoint" => ["request", "response"]
                    .iter()
                    .any(|p| rule[p]["breakpoint"] == true),
                _ => {
                    rule.get("mock").is_some()
                        || ["request", "response"].iter().any(|p| {
                            rule[p].as_object().is_some_and(|v| {
                                v.keys().any(|k| {
                                    !["script", "python", "breakpoint"].contains(&k.as_str())
                                })
                            })
                        })
                }
            };
            return match action {
                "set_enabled" => {
                    api(
                        directory,
                        "PATCH",
                        "/settings",
                        Some(json!({setting:args["enabled"]})),
                    )
                    .await
                }
                "list" | "get_config" | "get_by_id" => {
                    let value = api(directory, "GET", "/rules", None).await?;
                    let rules = value["rules"]
                        .as_array()
                        .ok_or_else(|| Error::bad("Missing rules"))?
                        .iter()
                        .filter(|r| belongs(r))
                        .cloned()
                        .collect::<Vec<_>>();
                    if action == "get_by_id" {
                        rules
                            .into_iter()
                            .find(|r| r["id"] == args["id"])
                            .ok_or_else(|| Error::new(404, "Rule not found"))
                    } else if action == "get_config" {
                        let settings = api(directory, "GET", "/settings", None).await?;
                        Ok(json!({"enabled":settings[setting],"rules":rules}))
                    } else {
                        Ok(json!(rules))
                    }
                }
                "create" | "update" | "delete" => {
                    if action != "delete" && !belongs(&args["rule"]) {
                        return Err(Error::bad(
                            "Rule does not contain an action for this category",
                        ));
                    }
                    Box::pin(call_tool(directory, &format!("rule_{action}"), args)).await
                }
                _ => Err(Error::bad("Unknown category action")),
            };
        }
    }
    match name {
        "capture_list_filter_list" => api(directory, "GET", "/capture-list-filters", None).await,
        "capture_list_filter_create" => {
            api(
                directory,
                "POST",
                "/capture-list-filters",
                Some(args.clone()),
            )
            .await
        }
        "capture_list_filter_set_enabled" | "capture_list_filter_delete" => {
            let deleting = name.ends_with("_delete");
            api(
                directory,
                if deleting { "DELETE" } else { "PATCH" },
                &format!("/capture-list-filters/{}", identifier(args)?),
                Some(if deleting {
                    json!({})
                } else {
                    json!({"enabled":args["enabled"]})
                }),
            )
            .await
        }
        "capture_live_status" => api(directory, "GET", "/state", None).await,
        "capture_live_filter" => {
            let state = api(directory, "GET", "/state", None).await?;
            let sessions = state["sessions"]
                .as_array()
                .ok_or_else(|| Error::bad("Missing sessions"))?;
            let query = text_arg(args, "query")?.to_lowercase();
            Ok(json!(
                sessions
                    .iter()
                    .filter(|s| s.to_string().to_lowercase().contains(&query))
                    .collect::<Vec<_>>()
            ))
        }
        "capture_live_get_by_id" => {
            api(
                directory,
                "GET",
                &format!("/history/{}", identifier(args)?),
                None,
            )
            .await
        }
        "capture_live_set_enabled" => {
            api(
                directory,
                "POST",
                "/proxy",
                Some(json!({"running":args["enabled"]})),
            )
            .await
        }
        "capture_live_clear" => api(directory, "DELETE", "/sessions", None).await,
        "history_list" => {
            let offset = args["offset"].as_u64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(200);
            if limit == 0 || limit > 200 {
                return Err(Error::bad("limit must be 1–200"));
            }
            if offset > i64::MAX as u64 {
                return Err(Error::bad("Invalid history pagination"));
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("offset", &offset.to_string())
                .append_pair("limit", &limit.to_string())
                .append_pair("query", args["query"].as_str().unwrap_or(""))
                .finish();
            api(directory, "GET", &format!("/history?{query}"), None).await
        }
        "history_get_by_id" => {
            let date = text_arg(args, "date")?;
            let id = args["id"].as_u64().unwrap();
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("date", date)
                .append_pair("id", &id.to_string())
                .finish();
            api(directory, "GET", &format!("/history/by-id?{query}"), None).await
        }
        "history_delete" => {
            api(
                directory,
                "DELETE",
                &format!("/history/{}", identifier(args)?),
                None,
            )
            .await
        }
        "http_request" => api(directory, "POST", "/request", Some(args.clone())).await,
        "rules_list" => api(directory, "GET", "/rules", None).await,
        "rule_create" | "rule_update" | "rule_delete" => {
            let current = api(directory, "GET", "/rules", None).await?;
            if let Some(error) = current["error"].as_str() {
                return Err(Error::bad(error));
            }
            let mut rules = current["rules"]
                .as_array()
                .cloned()
                .ok_or_else(|| Error::bad("Missing rules"))?;
            if name == "rule_create" {
                let mut rule = args["rule"].clone();
                rule.as_object_mut()
                    .ok_or_else(|| Error::bad("rule must be an object"))?
                    .entry("id")
                    .or_insert(json!(uuid::Uuid::new_v4().to_string()));
                rules.push(rule);
            } else {
                let id = text_arg(args, "id")?;
                let index = rules
                    .iter()
                    .position(|r| r["id"] == id)
                    .ok_or_else(|| Error::new(404, "Rule not found"))?;
                if rules[index] != args["original"] {
                    return Err(Error::new(
                        409,
                        "Rule changed; read it again before editing",
                    ));
                }
                if name == "rule_delete" {
                    rules.remove(index);
                } else {
                    let mut rule = args["rule"].clone();
                    rule.as_object_mut().unwrap().insert("id".into(), json!(id));
                    rules[index] = rule;
                }
            }
            api(
                directory,
                "PUT",
                "/rules",
                Some(json!({"rules":rules,"expected":current["rules"]})),
            )
            .await
        }
        "settings_get" => api(directory, "GET", "/settings", None).await,
        "settings_update" => {
            if args["settings"].get("mcpRemoteEnabled").is_some()
                || args["settings"]["_expected"]
                    .get("mcpRemoteEnabled")
                    .is_some()
            {
                return Err(Error::new(
                    403,
                    "Manage remote MCP access in the application",
                ));
            }
            api(
                directory,
                "PATCH",
                "/settings",
                Some(args["settings"].clone()),
            )
            .await
        }
        "breakpoints_list" => api(directory, "GET", "/breakpoints", None).await,
        "breakpoint_resume" => {
            let id = identifier(args)?;
            let mut payload = args.clone();
            payload.as_object_mut().unwrap().remove("id");
            api(
                directory,
                "POST",
                &format!("/breakpoints/{id}"),
                Some(payload),
            )
            .await
        }
        "script_template" => Ok(
            json!({"language":"python3","source":"from recho import *\n\ndef onRequest(context, request):\n    context.shared = {'method': request.method}\n    return request\n\ndef onResponse(context, response):\n    print(context.shared)\n    return response\n","ruleExample":{"name":"Python hooks","match":{"url":"https://example.com/*"},"request":{"python":"<source>"},"response":{"python":"<same source>"}}}),
        ),
        _ => Err(Error::bad("Unknown tool")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Minimal,
    All,
}

impl Scope {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "minimal" => Ok(Self::Minimal),
            "all" => Ok(Self::All),
            _ => Err(Error::bad("MCP scope must be minimal or all")),
        }
    }
    fn contains(self, name: &str) -> bool {
        self == Self::All
            || matches!(
                name,
                "capture_live_status"
                    | "capture_list_filter_list"
                    | "capture_list_filter_create"
                    | "capture_list_filter_set_enabled"
                    | "capture_list_filter_delete"
                    | "capture_live_filter"
                    | "capture_live_get_by_id"
                    | "history_list"
                    | "history_get_by_id"
                    | "capture_live_set_enabled"
                    | "http_request"
                    | "rules_list"
                    | "rule_create"
                    | "rule_update"
                    | "settings_get"
                    | "breakpoints_list"
                    | "breakpoint_resume"
                    | "environment_list"
                    | "environment_create"
            )
    }
}

pub async fn dispatch(message: Value, scope: Scope, directory: &Path) -> Option<Value> {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let valid_id = id.is_null() || id.is_string() || id.is_number();
    if !message.is_object()
        || message["jsonrpc"] != "2.0"
        || !message["method"].is_string()
        || !valid_id
    {
        return Some(rpc_error(
            if valid_id { id } else { Value::Null },
            -32600,
            "Invalid JSON-RPC request",
        ));
    }
    if message.get("id").is_none() {
        return None;
    }
    let failure = |code, text| rpc_error(id.clone(), code, text);
    if message.get("params").is_some_and(|p| !p.is_object()) {
        return Some(failure(-32602, "Expected object params"));
    }
    let result = match message["method"].as_str().unwrap_or("") {
        "initialize" => {
            let requested = message["params"]["protocolVersion"].as_str().unwrap_or("");
            let protocol = if requested == "2024-11-05" || HTTP_PROTOCOLS.contains(&requested) {
                requested
            } else {
                HTTP_PROTOCOLS.last().unwrap()
            };
            json!({"protocolVersion":protocol,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":crate::identity::SLUG,"version":env!("CARGO_PKG_VERSION")},"instructions":"Debug the connected Recho instance. Keep the target application running. API requests can change remote services; use only user-authorized destinations. Use history_list with query/offset/limit to search saved requests; capture_live_filter only searches the live buffer. Use history_get_by_id with the date and numeric ID shown on the History page. Look up a session UUID with capture_live_get_by_id."})
        }
        "ping" => json!({}),
        "tools/list" => {
            json!({"tools":tools().as_array().unwrap().iter().filter(|tool| scope.contains(tool["name"].as_str().unwrap_or(""))).collect::<Vec<_>>()})
        }
        "tools/call" => {
            let Some(name) = message["params"]["name"].as_str() else {
                return Some(failure(-32602, "Tool name is required"));
            };
            if !scope.contains(name) {
                return Some(failure(
                    -32602,
                    "Tool is unavailable in minimal scope; use --scope all",
                ));
            }
            let arguments = message["params"]
                .get("arguments")
                .cloned()
                .unwrap_or(json!({}));
            if !arguments.is_object() {
                return Some(failure(-32602, "Expected object tool arguments"));
            }
            match call_tool(directory, name, &arguments).await {
                Ok(value) => {
                    json!({"content":[{"type":"text","text":serde_json::to_string(&value).unwrap()}],"isError":false})
                }
                Err(error) => {
                    json!({"content":[{"type":"text","text":error.to_string()}],"isError":true})
                }
            }
        }
        _ => return Some(failure(-32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

pub async fn run() -> Result<()> {
    let mut host = String::from("127.0.0.1");
    let mut port = None;
    let mut scope = "minimal".to_string();
    let mut arguments = env::args().skip(if env::args().nth(1).as_deref() == Some("--mcp") {
        2
    } else {
        1
    });
    let mut seen = std::collections::HashSet::new();
    while let Some(option) = arguments.next() {
        if option == "--help" {
            eprintln!(
                "Usage: mcp-server [--host HOST] [--port PORT] [--scope minimal|all]\nSet RECHO_TOKEN to the target application's MCP access token. Defaults: 127.0.0.1, local proxy port or 9988, minimal."
            );
            return Ok(());
        }
        if !["--host", "--port", "--scope"].contains(&option.as_str())
            || !seen.insert(option.clone())
        {
            return Err(Error::bad("Unknown or repeated MCP option; use --help"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| Error::bad("Missing MCP option value"))?;
        match option.as_str() {
            "--host" => host = value,
            "--port" => {
                port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or_else(|| Error::bad("MCP port must be 1 to 65535"))?,
                )
            }
            "--scope" => {
                Scope::parse(&value)?;
                scope = value;
            }
            _ => unreachable!(),
        }
    }
    let token = env::var("RECHO_TOKEN").ok().filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Error::bad("RECHO_TOKEN is required; copy an access token from Tools > MCP > Manage access tokens"))?;
    // Parse a host, not a URL: paths, userinfo and query input must never change the target.
    let parsed_host = url::Host::parse(&host).map_err(|_| Error::bad("Invalid MCP host"))?;
    if host.chars().any(char::is_whitespace) || host.contains(['/', '?', '#', '@']) {
        return Err(Error::bad("Invalid MCP host"));
    }
    let local = matches!(&parsed_host, url::Host::Domain(name) if name == "localhost")
        || matches!(&parsed_host, url::Host::Ipv4(ip) if ip.is_loopback())
        || matches!(&parsed_host, url::Host::Ipv6(ip) if ip.is_loopback());
    let port = match port {
        Some(port) => port,
        None if local => {
            let state = match directory() {
                Ok(directory) => api(&directory, "GET", "/state", None).await.ok(),
                Err(_) => None,
            };
            state
                .and_then(|v| v["proxyPort"].as_u64())
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(9988)
        }
        None => 9988,
    };
    let endpoint = url::Url::parse(&format!("http://{parsed_host}:{port}/mcp?scope={scope}"))
        .map_err(Error::bad)?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let read = input
            .by_ref()
            .take(MAX_MESSAGE + 1)
            .read_until(b'\n', &mut bytes)?;
        if read == 0 {
            break;
        }
        if bytes.len() as u64 > MAX_MESSAGE {
            return Err(Error::bad("MCP message exceeds 20 MiB"));
        }
        let response = match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => {
                let id = value.get("id").cloned();
                match request_json(&endpoint, &token, "POST", Some(value)).await {
                    Ok(value) => value,
                    Err(error) => {
                        if let Some(id) = id {
                            Some(rpc_error(id, -32000, &error.to_string()))
                        } else {
                            eprintln!("MCP target request failed: {error}");
                            None
                        }
                    }
                }
            }
            Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn dispatch(message: Value) -> Option<Value> {
        super::dispatch(message, Scope::All, Path::new("")).await
    }
    #[test]
    fn token_lists_migrate_persist_and_preserve_invalid_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mcp-token.json");
        let old = "1234567890abcdef1234567890abcdef";
        fs::write(&path, serde_json::to_vec(&json!({"token":old})).unwrap()).unwrap();
        let mut store = AccessTokens::load(directory.path()).unwrap();
        assert_eq!(store.items[0].name, "默认令牌");
        assert_eq!(store.items[0].token, old);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        store
            .update("POST", None, json!({"name":" Cursor "}))
            .unwrap();
        assert_eq!(store.items[1].name, "Cursor");
        let first = store.items[0].id.clone();
        store
            .update("PATCH", Some(&first), json!({"name":"Codex"}))
            .unwrap();
        assert_eq!(store.items[0].token, old);
        let saved = fs::read(&path).unwrap();
        for name in ["", "  ", "bad\nname", &"x".repeat(101)] {
            assert!(store.update("POST", None, json!({"name":name})).is_err());
        }
        assert!(
            store
                .update("PATCH", Some(&first), json!({"token":old}))
                .is_err()
        );
        assert!(store.update("DELETE", Some("missing"), json!({})).is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(AccessTokens::load(directory.path()).unwrap().items.len(), 2);
        for item in store.items.clone() {
            store.update("DELETE", Some(&item.id), json!({})).unwrap();
        }
        assert!(
            AccessTokens::load(directory.path())
                .unwrap()
                .items
                .is_empty()
        );
        assert!(
            store
                .authorized_token(format!("Bearer {old}").as_bytes())
                .is_none()
        );
        for invalid in [
            b"not json".as_slice(),
            br#"{"version":2,"tokens":[]}"#,
            br#"{"token":"bad"}"#,
            br#"{"version":1,"tokens":[{}]}"#,
        ] {
            fs::write(&path, invalid).unwrap();
            assert!(AccessTokens::load(directory.path()).is_err());
            assert_eq!(fs::read(&path).unwrap(), invalid);
        }
    }

    #[test]
    fn failed_token_save_preserves_live_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = AccessTokens::load(directory.path()).unwrap();
        store.update("POST", None, json!({"name":"Codex"})).unwrap();
        let item = store.items[0].clone();
        let backup = directory.path().join("backup.json");
        fs::rename(&store.path, &backup).unwrap();
        fs::create_dir(&store.path).unwrap();
        assert!(store.update("DELETE", Some(&item.id), json!({})).is_err());
        assert!(
            store
                .update("PATCH", Some(&item.id), json!({"name":"changed"}))
                .is_err()
        );
        assert!(store.update("POST", None, json!({"name":"new"})).is_err());
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.items[0].name, "Codex");
        assert_eq!(
            store.authorized_token(format!("Bearer {}", item.token).as_bytes()),
            Some(item.id.as_str())
        );
        fs::remove_dir(&store.path).unwrap();
        fs::rename(backup, &store.path).unwrap();
        assert_eq!(
            AccessTokens::load(directory.path()).unwrap().items[0].token,
            item.token
        );
    }

    #[tokio::test]
    async fn invalid_rpc_envelopes_and_params_are_rejected_without_execution() {
        for message in [
            json!(null),
            json!([]),
            json!({}),
            json!({"jsonrpc":"2.0","id":[],"method":"ping"}),
        ] {
            assert_eq!(dispatch(message).await.unwrap()["error"]["code"], -32600);
        }
        assert_eq!(
            dispatch(json!({"jsonrpc":"2.0","id":1,"method":"missing"}))
                .await
                .unwrap()["error"]["code"],
            -32601
        );
        assert_eq!(dispatch(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"settings_update","arguments":[]}})).await.unwrap()["error"]["code"], -32602);
        assert!(dispatch(json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"capture_live_clear"}})).await.is_none());
    }

    #[tokio::test]
    async fn embedded_mcp_reads_captures_from_its_data_directory() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let directory = tempfile::tempdir().unwrap();
        let mut tokens = AccessTokens::load(directory.path()).unwrap();
        tokens
            .update("POST", None, json!({"name":"MCP test"}))
            .unwrap();
        let running = crate::server::start_embedded(
            directory.path().into(),
            Some(0),
            "embedded-control-test".into(),
        )
        .await
        .unwrap();
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp-capture", origin.local_addr().unwrap());
        let origin_task = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.unwrap();
            socket.read(&mut [0; 4096]).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
        });
        let control = url::Url::parse(&format!(
            "http://127.0.0.1:{}/request",
            running.ready["controlPort"]
        ))
        .unwrap();
        let capture = request_json(
            &control,
            "embedded-control-test",
            "POST",
            Some(json!({"method":"GET","url":url})),
        )
        .await
        .unwrap()
        .unwrap();
        origin_task.await.unwrap();
        let date = chrono::DateTime::parse_from_rfc3339(capture["startedAt"].as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Local)
            .date_naive()
            .to_string();
        let endpoint = url::Url::parse(&format!(
            "http://127.0.0.1:{}/mcp?scope=minimal",
            running.ready["proxyPort"]
        ))
        .unwrap();
        let call = |name: &'static str, arguments: Value| {
            let endpoint = &endpoint;
            let token = &tokens.items[0].token;
            async move {
                request_json(
                    endpoint, token, "POST",
                    Some(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})),
                ).await.unwrap().unwrap()["result"].clone()
            }
        };
        for (name, arguments) in [
            ("capture_live_status", json!({})),
            ("capture_live_filter", json!({"query":url})),
            ("capture_live_get_by_id", json!({"id":capture["id"]})),
            ("history_get_by_id", json!({"date":date,"id":1})),
        ] {
            let result = call(name, arguments).await;
            assert_eq!(result["isError"], false, "{name}: {result}");
            let value: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            let record = match name {
                "capture_live_status" => &value["sessions"][0],
                "capture_live_filter" => &value[0],
                _ => &value,
            };
            assert_eq!(record["id"], capture["id"], "{name}");
            assert_eq!(record["url"], url, "{name}");
            assert_eq!(record["status"], 200, "{name}");
        }
        for (query, status) in [
            ("date=2026-02-30&id=1".to_owned(), 400),
            (format!("date={date}&id=0"), 400),
            (format!("date={date}&id=2"), 404),
        ] {
            let target = control.join(&format!("/history/by-id?{query}")).unwrap();
            let error = request_json(&target, "embedded-control-test", "GET", None)
                .await
                .unwrap_err();
            assert_eq!(error.status, status, "{query}: {error}");
        }
        running.stop().await;
    }

    #[tokio::test]
    async fn protocol_and_invalid_arguments_do_not_need_an_engine() {
        let response = dispatch(json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .await
            .unwrap();
        assert_eq!(response["result"]["serverInfo"]["name"], "recho");
        assert!(
            dispatch(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
                .is_none()
        );
        assert!(
            call_tool(
                Path::new(""),
                "capture_live_set_enabled",
                &json!({"enabled":"yes"})
            )
            .await
            .is_err()
        );
        assert!(
            call_tool(Path::new(""), "settings_update", &json!({"settings":null}))
                .await
                .is_err()
        );
        assert!(
            call_tool(
                Path::new(""),
                "history_delete",
                &json!({"id":"../sessions"})
            )
            .await
            .is_err()
        );
        assert!(tools().as_array().unwrap().len() >= 17);
    }
}
