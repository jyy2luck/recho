use super::{STANDARD, draft, id, rows, text};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use std::{fs::File, io::Read, path::Path};
use url::{Url, form_urlencoded};

const LIMIT: usize = 16 * 1024 * 1024;

pub(super) fn render(source: &str, variables: &Map<String, Value>) -> Result<String, String> {
    let mut remaining = source;
    let mut output = String::new();
    while let Some((literal, start)) = remaining.split_once("{{") {
        let Some((key, rest)) = start.split_once("}}") else {
            break;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(['{', '}']) {
            output.push_str(literal);
            output.push_str("{{");
            remaining = start;
            continue;
        }
        output.push_str(literal);
        let value = variables
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("未定义环境变量 {key}"))?;
        if output.len().saturating_add(value.len()) > LIMIT {
            return Err("变量展开超过 16 MiB".into());
        }
        output.push_str(value);
        remaining = rest;
    }
    output.push_str(remaining);
    if output.len() > LIMIT {
        return Err("字段超过 16 MiB".into());
    }
    Ok(output)
}
fn header(headers: &mut String, name: &str, value: &str) -> Result<(), String> {
    if value.contains(['\r', '\n']) {
        return Err("请求头值不能含换行".into());
    }
    *headers = headers
        .lines()
        .filter(|line| {
            !line.trim().is_empty()
                && !line
                    .split_once(':')
                    .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    if !headers.is_empty() {
        headers.push_str("\r\n");
    }
    headers.push_str(name);
    headers.push_str(": ");
    headers.push_str(value);
    Ok(())
}
pub(super) fn file_bytes(path: &str, remaining: usize) -> Result<Vec<u8>, String> {
    if !Path::new(path).is_absolute() {
        return Err("请填写绝对文件路径".into());
    }
    let file = File::open(path).map_err(|error| format!("读取正文文件失败：{error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("正文路径必须是普通文件".into());
    }
    if metadata.len() > remaining as u64 {
        return Err("请求正文超过 16 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(remaining as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > remaining {
        return Err("请求正文超过 16 MiB".into());
    }
    Ok(bytes)
}

pub(super) fn payload(request: &Value, variables: &Map<String, Value>) -> Result<Value, String> {
    if request["bodyIncomplete"] == true {
        return Err("请补全捕获的请求正文".into());
    }
    let field = |key| render(text(request, key), variables);
    let url = field("url")?;
    let url = url.trim();
    let parsed = Url::parse(url).map_err(|_| "请输入有效的 HTTP/HTTPS URL")?;
    if !["http", "https"].contains(&parsed.scheme())
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("请输入不含内嵌凭据的 HTTP/HTTPS URL".into());
    }
    let raw = text(request, "kind") == "rawHTTP";
    let mut result = if raw {
        json!({
            "url":url, "raw":field("body")?, "requestID":id(),
            "verifyTLS": request["verifyTLS"].as_bool().unwrap_or(true),
            "timeout":request["timeout"].as_f64().unwrap_or(30.0),
            "autoSaveHistory":request["autoSaveHistory"].as_bool().unwrap_or(true),
        })
    } else {
        json!({"url":url, "method":text(request,"method"), "protocol":text(request,"protocolVersion")})
    };
    if !raw {
        let mut headers = field("headers")?;
        match text(request, "authorizationType") {
            "Basic" => {
                let encoded = STANDARD.encode(format!(
                    "{}:{}",
                    field("authorizationUser")?,
                    field("authorizationSecret")?
                ));
                header(&mut headers, "Authorization", &format!("Basic {encoded}"))?;
            }
            "Bearer" => header(
                &mut headers,
                "Authorization",
                &format!("Bearer {}", field("authorizationSecret")?),
            )?,
            _ => {}
        }
        let body_fields = || {
            rows(request, "bodyFields").iter().filter(|field| {
                field["enabled"] != false
                    && (!text(field, "name").is_empty() || !text(field, "value").is_empty())
            })
        };
        match text(request, "bodyMode") {
            "表单 URL 编码" => {
                let mut form = form_urlencoded::Serializer::new(String::new());
                for entry in body_fields() {
                    form.append_pair(
                        &render(text(entry, "name"), variables)?,
                        &render(text(entry, "value"), variables)?,
                    );
                }
                let body = form.finish();
                if body.len() > LIMIT {
                    return Err("请求正文超过 16 MiB".into());
                }
                result["body"] = json!(body);
                header(
                    &mut headers,
                    "Content-Type",
                    "application/x-www-form-urlencoded",
                )?;
            }
            "Multipart" => {
                let boundary = format!("recho-{}", id());
                let quoted = |input: &str| {
                    input
                        .replace(['\r', '\n'], "")
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                };
                let mut bytes = Vec::new();
                for entry in body_fields() {
                    let name = render(text(entry, "name"), variables)?;
                    let value = render(text(entry, "value"), variables)?;
                    let prefix = format!(
                        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
                        quoted(&name)
                    );
                    bytes.extend_from_slice(prefix.as_bytes());
                    if entry["isFile"] == true {
                        let filename = Path::new(&value)
                            .file_name()
                            .and_then(|file| file.to_str())
                            .ok_or("无法识别文件名")?;
                        bytes.extend_from_slice(format!("; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",quoted(filename)).as_bytes());
                        bytes.extend(file_bytes(&value, LIMIT.saturating_sub(bytes.len()))?);
                    } else {
                        bytes.extend_from_slice(b"\r\n\r\n");
                        bytes.extend_from_slice(value.as_bytes());
                    }
                    bytes.extend_from_slice(b"\r\n");
                    if bytes.len() > LIMIT {
                        return Err("请求正文超过 16 MiB".into());
                    }
                }
                bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
                if bytes.len() > LIMIT {
                    return Err("请求正文超过 16 MiB".into());
                }
                result["bodyBase64"] = json!(STANDARD.encode(bytes));
                header(
                    &mut headers,
                    "Content-Type",
                    &format!("multipart/form-data; boundary={boundary}"),
                )?;
            }
            "文件" => {
                result["bodyBase64"] =
                    json!(STANDARD.encode(file_bytes(&field("bodyFile")?, LIMIT)?));
                let mime = field("bodyMIME")?;
                header(
                    &mut headers,
                    "Content-Type",
                    if mime.is_empty() {
                        "application/octet-stream"
                    } else {
                        mime.as_str()
                    },
                )?;
            }
            "Base64" => {
                let body = text(request, "body");
                let encoded = if body.len() > LIMIT {
                    // A valid Base64 body can be larger than the decoded engine limit.
                    if body.contains("{{") || body.len() > LIMIT.div_ceil(3) * 4 {
                        return Err("Base64 正文超过限制".into());
                    }
                    body.to_string()
                } else {
                    field("body")?
                };
                let decoded = STANDARD
                    .decode(encoded.as_bytes())
                    .map_err(|_| "Base64 正文编码无效")?;
                if decoded.len() > LIMIT {
                    return Err("请求正文超过 16 MiB".into());
                }
                result["bodyBase64"] = json!(encoded);
            }
            _ => {
                result["body"] = json!(field("body")?);
            }
        }
        result["headers"] = json!(headers);
    }
    result["variables"] = json!(variables);
    if request["scriptEnabled"] == true {
        if text(request, "scriptLanguage") == "javascript" {
            result["beforeScript"] = request["beforeScript"].clone();
            result["afterScript"] = request["afterScript"].clone();
        } else {
            result["pythonScript"] = request["pythonScript"].clone();
        }
    }
    Ok(result)
}

pub(super) fn query_rows(url: &str) -> Vec<Value> {
    let base = url.split_once('#').map_or(url, |(base, _)| base);
    let Some((_, query)) = base.split_once('?') else {
        return vec![];
    };
    query.split('&').filter(|item| !item.is_empty()).map(|item| {
        let value = form_urlencoded::parse(item.as_bytes()).next();
        let (name,value) = value.as_ref().map_or(("", ""), |(name,value)| (name.as_ref(), value.as_ref()));
        json!({"name":name,"value":value,"enabled":true,"raw":item,"originalName":name,"originalValue":value})
    }).collect()
}
pub(super) fn query_url(url: &str, entries: &[Value]) -> String {
    let (base, fragment) = url.find('#').map_or((url, ""), |index| url.split_at(index));
    let base = base.split_once('?').map_or(base, |(base, _)| base);
    let query = entries
        .iter()
        .filter(|field| field["enabled"] != false)
        .filter(|field| {
            !text(field, "name").is_empty()
                || !text(field, "value").is_empty()
                || field["raw"].is_string()
        })
        .map(|field| {
            if field["raw"].is_string()
                && field["name"] == field["originalName"]
                && field["value"] == field["originalValue"]
            {
                text(field, "raw").to_owned()
            } else {
                form_urlencoded::Serializer::new(String::new())
                    .append_pair(text(field, "name"), text(field, "value"))
                    .finish()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        format!("{base}{fragment}")
    } else {
        format!("{base}?{query}{fragment}")
    }
}

// Import the subset exported by this editor. Parse arguments as data, never run curl or a shell.
fn words(command: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if q == c => quote = None,
            (None, '\'' | '"') => {
                quote = Some(c);
                token_started = true;
            }
            (None | Some('"'), '\\') => {
                let next = chars.next().ok_or("cURL 末尾缺少转义字符")?;
                if next == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                } else if next != '\n' {
                    token_started = true;
                    current.push(next);
                }
            }
            (None, c) if c.is_whitespace() => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            _ => {
                current.push(c);
                token_started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("cURL 引号未闭合".into());
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
}
pub(super) fn request_from_curl(command: &str) -> Result<Value, String> {
    let mut words = words(command)?.into_iter();
    if !matches!(words.next().as_deref(), Some("curl" | "curl.exe")) {
        return Err("请输入以 curl 开头的命令".into());
    }
    let mut request = draft("http");
    let mut method = None;
    let mut url = None;
    let mut body: Option<String> = None;
    let mut headers = Vec::new();
    while let Some(arg) = words.next() {
        let (option, value) = if arg.starts_with("--") {
            arg.split_once('=')
                .map_or((arg.as_str(), None), |(key, value)| {
                    (key, Some(value.to_string()))
                })
        } else {
            (arg.as_str(), None)
        };
        let mut next = || {
            value
                .clone()
                .or_else(|| words.next())
                .ok_or_else(|| format!("缺少 {option} 的参数"))
        };
        match option {
            "-X" | "--request" => method = Some(next()?),
            "-I" | "--head" => method = Some("HEAD".into()),
            "-H" | "--header" => {
                let line = next()?;
                if line.contains(['\r', '\n']) || !line.contains(':') {
                    return Err("cURL 请求头格式无效".into());
                }
                headers.push(line);
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--json" => {
                let value = next()?;
                if option != "--data-raw" && value.starts_with('@') {
                    return Err("cURL 文件引用需在编辑器中手动选择".into());
                }
                if option == "--json" {
                    headers.push("Content-Type: application/json".into());
                }
                let bytes = body.get_or_insert_with(String::new);
                if !bytes.is_empty() {
                    bytes.push('&');
                }
                bytes.push_str(&value);
            }
            "--url" => {
                if url.is_some() {
                    return Err("请一次导入一个 URL".into());
                }
                url = Some(next()?);
            }
            "--http1.1" => request["protocolVersion"] = json!("HTTP/1.1"),
            "--http2" => request["protocolVersion"] = json!("HTTP/2"),
            "--http3" | "--http3-only" => request["protocolVersion"] = json!("HTTP/3"),
            "-u" | "--user" => {
                let credentials = next()?;
                let (user, secret) = credentials
                    .split_once(':')
                    .ok_or("Basic 授权需要 username:password")?;
                request["authorizationType"] = json!("Basic");
                request["authorizationUser"] = json!(user);
                request["authorizationSecret"] = json!(secret);
            }
            "-s" | "--silent" | "-S" | "--show-error" | "-v" | "--verbose" | "--compressed" => {}
            value if !value.starts_with('-') => {
                if url.is_some() {
                    return Err("请一次导入一个 URL".into());
                }
                url = Some(value.into());
            }
            _ => return Err(format!("暂不支持 cURL 参数 {option}")),
        }
    }
    let url = url.ok_or("cURL 命令缺少 URL")?;
    let parsed = Url::parse(&url).map_err(|_| "请输入完整的 HTTP/HTTPS URL")?;
    let method = method.unwrap_or_else(|| if body.is_some() { "POST" } else { "GET" }.into());
    request["name"] = json!(parsed.host_str().unwrap_or("未命名"));
    request["method"] = json!(method);
    request["url"] = json!(url);
    request["headers"] = json!(headers.join("\r\n"));
    request["body"] = json!(body.unwrap_or_default());
    Ok(request)
}

pub(super) fn curl(request: &Value, variables: &Map<String, Value>) -> Result<String, String> {
    let request = payload(request, variables)?;
    if request.get("bodyBase64").is_some() {
        return Err("二进制正文请导出请求 JSON 后使用 API 编辑器发送".into());
    }
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut result = format!(
        "curl -X {} {}",
        quote(text(&request, "method")),
        quote(text(&request, "url"))
    );
    for line in text(&request, "headers")
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        result.push_str(&format!(" -H {}", quote(line)));
    }
    match text(&request, "protocol") {
        "HTTP/1.1" => result.push_str(" --http1.1"),
        "HTTP/2" => result.push_str(" --http2"),
        "HTTP/3" => result.push_str(" --http3-only"),
        _ => {}
    }
    if !text(&request, "body").is_empty() {
        result.push_str(&format!(" --data-raw {}", quote(text(&request, "body"))));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_duplicate_headers_query_encoding_and_binary_data() {
        let mut request = request_from_curl("curl -X POST 'https://example.test/?a=%2f&a=2#frag' -H 'X-Test: one' -H 'x-test: two' --data-raw 'we'\\''re'").unwrap();
        assert_eq!(text(&request, "body"), "we're");
        assert_eq!(text(&request, "headers"), "X-Test: one\r\nx-test: two");
        let url = text(&request, "url");
        let mut params = query_rows(url);
        params[1]["value"] = json!("foo bar");
        assert_eq!(
            query_url(url, &params),
            "https://example.test/?a=%2f&a=foo+bar#frag"
        );
        request["url"] = json!("{{base}}/api");
        request["bodyMode"] = json!("Base64");
        request["body"] = json!("AAEC/w==");
        request["authorizationType"] = json!("Bearer");
        request["authorizationSecret"] = json!("{{token}}");
        let vars = Map::from_iter([
            ("base".into(), json!("http://127.0.0.1:1234")),
            ("token".into(), json!("local-token")),
        ]);
        let result = payload(&request, &vars).unwrap();
        assert_eq!(result["url"], "http://127.0.0.1:1234/api");
        assert_eq!(result["bodyBase64"], "AAEC/w==");
        assert_eq!(
            result["headers"],
            "X-Test: one\r\nx-test: two\r\nAuthorization: Bearer local-token"
        );
        assert!(request_from_curl("curl -d @password.txt https://example.test/").is_err());
        request["authorizationSecret"] = json!("foo\r\nbar");
        assert!(payload(&request, &vars).is_err());
    }
}
