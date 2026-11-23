use super::{
    Composer, DEFAULT_FOLDER, VERSIONS, draft, folders, id, normalized, request_from_curl, rows,
    rows_mut, text,
};
use serde_json::{Value, json};

impl Composer {
    pub(in crate::composer) fn handle_operation(
        &mut self,
        operation_id: Option<&str>,
        operation: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let document = self.document.as_mut().ok_or("工作区仍在读取")?;
        if let Some(operation_id) = operation_id {
            if let Some(previous) = rows(document, "completedUIOperations")
                .iter()
                .find(|entry| text(entry, "id") == operation_id)
            {
                return serde_json::from_str(text(previous, "result"))
                    .map_err(|error| error.to_string());
            }
        }
        let identity = args["id"].as_str().unwrap_or_default();
        if self.volatile_tabs.contains(identity) && operation == "rest_http_update" {
            return Err("无痕草稿只能在窗口中编辑".into());
        }
        let (value, changed) = mutate(document, operation, args)?;
        if changed {
            if let Some(operation_id) = operation_id {
                let completed = rows_mut(document, "completedUIOperations");
                let serialized =
                    serde_json::to_string(&value).map_err(|error| error.to_string())?;
                completed.push(json!({"id":operation_id,"result":serialized}));
                let excess = completed.len().saturating_sub(128);
                completed.drain(..excess);
            }
            let next = text(document, "activeRequestID");
            if next != self.active {
                self.active = next.into();
                self.reply = None;
            }
            self.mark_dirty();
        }
        Ok(value)
    }
}

fn mutate(document: &mut Value, operation: &str, args: &Value) -> Result<(Value, bool), String> {
    let identity = text(args, "id");
    let name = || {
        let name = text(args, "name").trim();
        if name.is_empty() || name.chars().any(char::is_control) || name.len() > 512 {
            Err("名称必须为 1 到 512 字节的文本")
        } else {
            Ok(name)
        }
    };
    let existing = |key: &str, document: &Value| {
        rows(document, key)
            .iter()
            .position(|item| text(item, "id") == identity)
            .ok_or("指定的对象不存在")
    };
    let can_select = |folder: &str, document: &Value| {
        if folders(document).iter().any(|entry| entry == folder) {
            Ok(())
        } else {
            Err("集合不存在")
        }
    };
    match operation {
        "environment_list" => Ok((json!(rows(document, "environments")), false)),
        "environment_get_by_id" => Ok((
            rows(document, "environments")[existing("environments", document)?].clone(),
            false,
        )),
        "environment_get_active" => {
            let active = text(document, "activeEnvironmentID");
            let value = rows(document, "environments")
                .iter()
                .find(|entry| text(entry, "id") == active);
            Ok((value.cloned().unwrap_or_default(), false))
        }
        "environment_create" => {
            let value =
                json!({"id":id(),"name":name()?,"variables":variables(args.get("variables"))?});
            rows_mut(document, "environments").push(value.clone());
            Ok((value, true))
        }
        "environment_update" => {
            let index = existing("environments", document)?;
            let mut value = rows(document, "environments")[index].clone();
            if args.get("name").is_some() {
                value["name"] = json!(name()?);
            }
            if args.get("variables").is_some() {
                value["variables"] = json!(variables(args.get("variables"))?);
            }
            rows_mut(document, "environments")[index] = value.clone();
            Ok((value, true))
        }
        "environment_delete" => {
            let index = existing("environments", document)?;
            rows_mut(document, "environments").remove(index);
            if text(document, "activeEnvironmentID") == identity {
                document["activeEnvironmentID"] = Value::Null;
            }
            Ok((json!({"deleted":identity}), true))
        }
        "environment_select" => {
            let value = if identity.is_empty() {
                Value::Null
            } else {
                rows(document, "environments")[existing("environments", document)?].clone()
            };
            document["activeEnvironmentID"] = value["id"].clone();
            Ok((value, true))
        }
        "collection_list" | "collection_structure" | "collection_get" => {
            if operation == "collection_get" {
                can_select(identity, document)?;
                return Ok((collection(document, identity), false));
            }
            let groups = folders(document)
                .iter()
                .map(|folder| collection(document, folder))
                .collect::<Vec<_>>();
            let value = if operation == "collection_structure" {
                json!({"folders":groups,"requests":rows(document,"collections")})
            } else {
                json!(groups)
            };
            Ok((value, false))
        }
        "collection_create" => {
            let parent = text(args, "parent");
            if !parent.is_empty() {
                can_select(parent, document)?;
            }
            let name = name()?;
            if name.contains('/') {
                return Err("新集合名称不能包含 /".into());
            }
            let folder = if parent.is_empty() {
                name.to_string()
            } else {
                format!("{parent}/{name}")
            };
            if can_select(&folder, document).is_ok() {
                return Err("集合已存在".into());
            }
            // Materialize the default folder before adding another.
            let mut existing = folders(document);
            existing.push(folder.clone());
            document["collectionFolders"] = json!(existing);
            Ok((collection(document, &folder), true))
        }
        "collection_update" => {
            can_select(identity, document)?;
            let name = name()?;
            let target = if name.contains('/') {
                name.to_owned()
            } else if let Some((parent, _)) = identity.rsplit_once('/') {
                format!("{parent}/{name}")
            } else {
                name.to_owned()
            };
            if target == identity
                || target.starts_with(&format!("{identity}/"))
                || can_select(&target, document).is_ok()
                || target
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err("集合目标无效".into());
            }
            if let Some((parent, _)) = target.rsplit_once('/') {
                can_select(parent, document)?;
            }
            let rename = |folder: &str| {
                if folder == identity || folder.starts_with(&format!("{identity}/")) {
                    format!("{target}{}", &folder[identity.len()..])
                } else {
                    folder.into()
                }
            };
            let names = folders(document)
                .iter()
                .map(|folder| rename(folder))
                .collect::<Vec<_>>();
            document["collectionFolders"] = json!(names);
            for entry in rows_mut(document, "collections") {
                entry["folder"] = json!(rename(text(entry, "folder")));
            }
            Ok((collection(document, &target), true))
        }
        "collection_delete" => {
            can_select(identity, document)?;
            let descendant = format!("{identity}/");
            let keep = |folder: &str| folder != identity && !folder.starts_with(&descendant);
            let names = folders(document)
                .into_iter()
                .filter(|folder| keep(folder))
                .collect::<Vec<_>>();
            document["collectionFolders"] = json!(names);
            let mut removed = Vec::new();
            rows_mut(document, "collections").retain(|entry| {
                let keep = keep(text(entry, "folder"));
                if !keep {
                    removed.push(text(entry, "id").to_owned());
                }
                keep
            });
            detach(document, &removed);
            Ok((json!({"deleted":identity}), true))
        }
        "collection_api_get" | "collection_api_update" | "collection_api_delete" => {
            let index = existing("collections", document)?;
            let mut entry = rows(document, "collections")[index].clone();
            if operation == "collection_api_get" {
                return Ok((entry, false));
            }
            if operation == "collection_api_delete" {
                rows_mut(document, "collections").remove(index);
                detach(document, &[identity.into()]);
                return Ok((json!({"deleted":identity}), true));
            }
            if let Some(folder) = args["collectionId"].as_str() {
                can_select(folder, document)?;
                entry["folder"] = json!(folder);
            }
            entry["request"] = request(args, &entry["request"])?;
            rows_mut(document, "collections")[index] = entry.clone();
            Ok((entry, true))
        }
        "collection_api_add" => {
            let folder = args["collectionId"].as_str().unwrap_or(DEFAULT_FOLDER);
            can_select(folder, document)?;
            let value = json!({"id":id(),"folder":folder,"request":request(args,&Value::Null)?});
            rows_mut(document, "collections").push(value.clone());
            Ok((value, true))
        }
        "rest_http_create_from_url" | "rest_http_create_from_curl" | "rest_http_update" => {
            let existing = if operation == "rest_http_update" {
                Some(existing("requests", document)?)
            } else {
                None
            };
            let value = if operation.ends_with("curl") {
                request_from_curl(text(args, "command"))?
            } else {
                request(
                    args,
                    existing.map_or(&Value::Null, |index| &rows(document, "requests")[index]),
                )?
            };
            let entries = rows_mut(document, "requests");
            if let Some(index) = existing {
                entries[index] = value.clone();
            } else {
                entries.push(value.clone());
            }
            document["activeRequestID"] = value["id"].clone();
            Ok((value, true))
        }
        _ => Err("不支持的工作区操作".into()),
    }
}

fn variables(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let values = value.as_object().ok_or("variables 必须是字符串字典")?;
    values
        .iter()
        .map(|(name, value)| {
            let value = value.as_str().ok_or("变量值必须为字符串")?;
            if name.trim().is_empty() || name.contains(['{', '}']) {
                return Err("变量名无效".into());
            }
            Ok(json!({"id":id(),"name":name,"value":value,"enabled":true}))
        })
        .collect()
}
fn request(args: &Value, previous: &Value) -> Result<Value, String> {
    let edits = args
        .get("request")
        .unwrap_or(args)
        .as_object()
        .ok_or("请求必须是对象")?;
    let mut value = if previous.is_object() {
        previous.clone()
    } else {
        draft("http")
    };
    // Exposed operations edit a draft only. They never execute a request or hook.
    for key in [
        "name",
        "method",
        "url",
        "headers",
        "body",
        "bodyMode",
        "bodyFields",
        "bodyFile",
        "bodyMIME",
        "authorizationType",
        "authorizationUser",
        "authorizationSecret",
        "protocolVersion",
    ] {
        if let Some(edit) = edits.get(key) {
            value[key] = edit.clone();
        }
    }
    if let Some(protocol) = edits.get("protocol") {
        value["protocolVersion"] = protocol.clone();
    }
    if !VERSIONS.contains(&text(&value, "protocolVersion")) {
        return Err("不支持的 HTTP 协议".into());
    }
    if let Some(headers) = value["headers"].as_object() {
        let mut lines = Vec::new();
        for (key, values) in headers {
            let values = values
                .as_array()
                .map_or_else(|| std::slice::from_ref(values), Vec::as_slice);
            for value in values {
                let value = value.as_str().ok_or("请求头值必须为文本")?;
                if key.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
                    return Err("请求头格式无效".into());
                }
                lines.push(format!("{key}: {value}"));
            }
        }
        value["headers"] = json!(lines.join("\r\n"));
    }
    if edits.contains_key("headers") {
        value["headerFields"] = json!([]);
    }
    let url = text(&value, "url");
    if !url.is_empty() && !url.contains("{{") {
        let url = url::Url::parse(url).map_err(|_| "无效 HTTP URL")?;
        if !["http", "https"].contains(&url.scheme())
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("请输入不含内嵌凭据的 HTTP/HTTPS URL".into());
        }
    }
    let mut result = normalized(&value)?;
    result["id"] = value["id"].clone();
    for key in ["collectionEntryID", "lastResponseID"] {
        if let Some(old) = previous.get(key) {
            result[key] = old.clone();
        }
    }
    Ok(result)
}
fn collection(document: &Value, folder: &str) -> Value {
    let (parent, name) = folder.rsplit_once('/').unwrap_or(("", folder));
    let requests = rows(document,"collections").iter().filter(|entry| text(entry,"folder") == folder).map(|entry| {
        let request = &entry["request"];
        json!({"id":entry["id"],"name":request["name"],"method":request["method"],"url":request["url"]})
    }).collect::<Vec<_>>();
    json!({"id":folder,"name":name,"parent":parent,"requests":requests})
}
fn detach(document: &mut Value, ids: &[String]) {
    for request in rows_mut(document, "requests") {
        if ids
            .iter()
            .any(|id| id == text(request, "collectionEntryID"))
        {
            request["collectionEntryID"] = Value::Null;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updates_environment_and_collections_without_replaying_or_losing_data() {
        let document = json!({"version":1,"requests":[],"environments":[],"collections":[],"collectionFolders":[DEFAULT_FOLDER]});
        let mut composer = Composer {
            document: Some(document),
            ..Default::default()
        };
        let env = composer
            .handle_operation(
                Some("operation-1"),
                "environment_create",
                &json!({"name":"Local","variables":{"host":"127.0.0.1"}}),
            )
            .unwrap();
        assert_eq!(
            composer
                .handle_operation(
                    Some("operation-1"),
                    "environment_create",
                    &json!({"name":"Local"})
                )
                .unwrap(),
            env
        );
        assert_eq!(
            rows(composer.document.as_ref().unwrap(), "environments").len(),
            1
        );
        composer
            .handle_operation(
                None,
                "collection_create",
                &json!({"name":"folder","parent":DEFAULT_FOLDER}),
            )
            .unwrap();
        let value = composer.handle_operation(None,"collection_api_add",&json!({"collectionId":format!("{DEFAULT_FOLDER}/folder"),"request":{"url":"http://127.0.0.1/","headers":{"X-Hello":["one","two"]}}})).unwrap();
        assert_eq!(value["request"]["headers"], "X-Hello: one\r\nX-Hello: two");
        let old = composer.document.clone();
        assert!(
            composer
                .handle_operation(
                    None,
                    "collection_api_update",
                    &json!({"id":value["id"],"request":{"url":"file:///tmp/test"}})
                )
                .is_err()
        );
        assert_eq!(composer.document, old);
    }
}
