use crate::wire::{Error, RequestData, ResponseData, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

static LOGS: OnceLock<Mutex<VecDeque<Value>>> = OnceLock::new();
pub fn default_executable() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "/usr/bin/python3"
    }
}
fn log(url: &str, phase: &str, text: &str, rule_id: Option<&str>) {
    if text.is_empty() {
        return;
    }
    let mut logs = LOGS
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .unwrap();
    logs.push_back(
        json!({"timestamp":chrono::Utc::now().to_rfc3339(),"url":url,"phase":phase,"text":text,"ruleId":rule_id}),
    );
    while logs.len() > 200 {
        logs.pop_front();
    }
}
pub fn logs() -> Value {
    json!({"logs":LOGS.get_or_init(|| Mutex::new(VecDeque::new())).lock().unwrap().iter().cloned().collect::<Vec<_>>()})
}
pub fn validate(source: &str) -> Result<()> {
    if source.trim().is_empty() || source.len() > 1024 * 1024 {
        return Err(Error::bad("Python source must contain 1–1048576 bytes"));
    }
    Ok(())
}
async fn collect(
    mut pipe: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut result = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok((result, truncated));
        }
        let retain = count.min(limit.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..retain]);
        truncated |= retain < count;
    }
}
pub async fn execute(
    executable: &str,
    source: &str,
    request: &RequestData,
    response: Option<&ResponseData>,
    shared: &Value,
    timeout: u64,
) -> Result<(RequestData, Option<ResponseData>, Value)> {
    execute_for_rule(None, executable, source, request, response, shared, timeout).await
}

pub async fn execute_for_rule(
    rule_id: Option<&str>,
    executable: &str,
    source: &str,
    request: &RequestData,
    response: Option<&ResponseData>,
    shared: &Value,
    timeout: u64,
) -> Result<(RequestData, Option<ResponseData>, Value)> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return Err(Error::new(
            501,
            "Mobile builds do not include a Python interpreter; use JavaScript or run this script on desktop",
        ));
    }
    validate(source)?;
    let executable = if executable.is_empty() {
        default_executable()
    } else {
        executable
    };
    let mut child = Command::new(executable).env("PYTHONIOENCODING", "utf-8").args(["-u", "-c", include_str!("python_bridge.py")])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().map_err(|error| Error::bad(format!("Cannot start Python {executable}: {error}. Select a Python 3 executable in script settings.")))?;
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let errors = child.stderr.take().unwrap();
    let mut request_value = serde_json::to_value(request)?;
    if let Some(bytes) = &request.binary_body {
        request_value["bodyBase64"] = json!(STANDARD.encode(bytes));
    }
    let mut response_value = serde_json::to_value(response)?;
    if let Some(bytes) = response.and_then(|r| r.binary_body.as_ref()) {
        response_value["bodyBase64"] = json!(STANDARD.encode(bytes));
    }
    let payload = serde_json::to_vec(
        &json!({"source":source,"request":request_value,"response":response_value,"shared":shared}),
    )?;
    let phase = if response.is_some() {
        "response"
    } else {
        "request"
    };
    let result = tokio::time::timeout(Duration::from_millis(timeout.max(1)), async {
        let write = async move {
            input.write_all(&payload).await?;
            input.shutdown().await?;
            drop(input);
            Ok::<(), std::io::Error>(())
        };
        let (write, output, errors, status) = tokio::join!(
            write,
            collect(output, 40 * 1024 * 1024),
            collect(errors, 64 * 1024),
            child.wait()
        );
        let (errors, truncated) = errors?;
        let mut errors = String::from_utf8_lossy(&errors).into_owned();
        if truncated {
            errors.push_str("\n[Console truncated at 64 KiB]");
        }
        log(&request.url, phase, &errors, rule_id);
        let status = status?;
        if !status.success() {
            return Err(Error::bad(format!("Python hook failed: {errors}")));
        }
        write?;
        let (output, truncated) = output?;
        if truncated {
            return Err(Error::new(413, "Python hook output exceeds 40 MiB"));
        }
        let mut value: Value = serde_json::from_slice(&output)
            .map_err(|error| Error::bad(format!("Invalid Python hook output: {error}")))?;
        let binary = take_binary(&mut value["request"])?;
        let mut request =
            serde_json::from_value::<RequestData>(value["request"].clone())?.validate()?;
        request.binary_body = binary;
        let response = if let Some(value) = value.get_mut("response") {
            let binary = take_binary(value)?;
            let edited = serde_json::from_value::<ResponseData>(value.clone())?;
            let mut edited = if response.is_some_and(|r| r.status == 101) && edited.status == 101 {
                edited.validate_for_upgrade()?
            } else {
                edited.validate()?
            };
            edited.binary_body = binary;
            Some(edited)
        } else {
            None
        };
        let mut shared = value["shared"].clone();
        shared["_console"] = json!(errors);
        Ok((request, response, shared))
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let message = format!("Python hook timed out after {timeout} ms");
            log(&request.url, phase, &message, rule_id);
            Err(Error::new(504, message))
        }
    }
}

fn take_binary(value: &mut Value) -> Result<Option<Vec<u8>>> {
    value
        .as_object_mut()
        .ok_or_else(|| Error::bad("Python message must be an object"))?
        .remove("bodyBase64")
        .map(|v| {
            STANDARD
                .decode(
                    v.as_str()
                        .ok_or_else(|| Error::bad("bodyBase64 must be a string"))?,
                )
                .map_err(Error::bad)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn documented_python_hooks_shared_headers_queries_and_json() {
        let request = RequestData {
            binary_body: None,
            method: "GET".into(),
            url: "http://example.test/a?x=1&x=2&raw=%2f".into(),
            headers: Default::default(),
            body: Some("".into()),
        };
        let source = "from recho import *\ndef onRequest(context, request):\n context.shared = {'x': request.queries['x']}\n request.headers.add('X-Test', 'one')\n request.headers.add('x-test', 'two')\n return request\ndef onResponse(context, response):\n response.body.jsonify()\n response.body['x'] = context.shared['x']\n response.code = 201\n print('shared-ok')\n return response\n";
        let (request, _, shared) = execute(
            crate::python::default_executable(),
            source,
            &request,
            None,
            &Value::Null,
            5000,
        )
        .await
        .unwrap();
        assert!(request.url.ends_with("x=1&x=2&raw=%2f"));
        assert_eq!(request.headers["x-test"], json!(["one", "two"]));
        let response = ResponseData {
            binary_body: None,
            status: 200,
            headers: Default::default(),
            body: Some("{}".into()),
        };
        let (_, response, _) = execute(
            crate::python::default_executable(),
            source,
            &request,
            Some(&response),
            &shared,
            5000,
        )
        .await
        .unwrap();
        let response = response.unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(
            serde_json::from_str::<Value>(response.body.as_ref().unwrap()).unwrap()["x"],
            "1"
        );
        assert!(logs().to_string().contains("shared-ok"));
    }
    #[tokio::test]
    async fn python_timeout_drop_and_traceback_are_errors() {
        let request = RequestData {
            binary_body: None,
            method: "GET".into(),
            url: "http://example.test/".into(),
            headers: Default::default(),
            body: None,
        };
        for source in [
            "def onRequest(c, r): return None",
            "def onRequest(c, r): return 1/0",
        ] {
            assert!(
                execute(
                    crate::python::default_executable(),
                    source,
                    &request,
                    None,
                    &Value::Null,
                    5000
                )
                .await
                .is_err()
            );
        }
        let error = execute(
            crate::python::default_executable(),
            "while True: pass",
            &request,
            None,
            &Value::Null,
            300,
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 504);
    }
    #[tokio::test]
    async fn python_preserves_and_edits_binary_and_multipart_data() {
        let request = RequestData {
            method: "POST".into(),
            url: "http://example.test/".into(),
            headers: Default::default(),
            body: None,
            binary_body: Some(vec![0, 255, 1]),
        };
        let source = "from recho import *\ndef onRequest(c,r):\n assert r.body.isBinary\n r.body.binary(r.body.payload + b'\\x80')\n c.env['token']='saved'\n return r";
        let (request, _, shared) = execute(
            crate::python::default_executable(),
            source,
            &request,
            None,
            &json!({"_context":{"env":{"old":"one"}}}),
            5000,
        )
        .await
        .unwrap();
        assert_eq!(request.binary_body, Some(vec![0, 255, 1, 128]));
        assert_eq!(shared["_context"]["env"]["token"], "saved");
        let source = "from recho import *\ndef onRequest(c,r):\n r.headers['content-type']='multipart/form-data; boundary=local-test'\n r.body.multiparts([HttpMultipartBody.text('hello',name='field')])\n return r";
        let (request, _, _) = execute(
            crate::python::default_executable(),
            source,
            &request,
            None,
            &Value::Null,
            5000,
        )
        .await
        .unwrap();
        assert!(
            String::from_utf8(request.binary_body.clone().unwrap())
                .unwrap()
                .contains("name=\"field\"\r\n\r\nhello")
        );
        let source = "from recho import *\ndef onRequest(c,r):\n assert r.body.isMultipart\n assert r.body[0].name=='field'\n r.body[0]=HttpMultipartBody.text('changed',name='field')\n return r";
        let (request, _, _) = execute(
            crate::python::default_executable(),
            source,
            &request,
            None,
            &Value::Null,
            5000,
        )
        .await
        .unwrap();
        assert!(
            String::from_utf8(request.binary_body.unwrap())
                .unwrap()
                .contains("changed")
        );
    }
}
