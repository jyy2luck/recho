use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn request() -> RequestData {
    serde_json::from_value::<RequestData>(json!({"method":"POST", "url":"http://example.test/v1/item?q=1", "headers":{"Content-Type":"text/plain", "X-Old":"old"}, "body":"one one"})).unwrap().validate().unwrap()
}
fn response() -> ResponseData {
    serde_json::from_value(
        json!({"status":200,"headers":{"set-cookie":["a=1","b=2"]},"body":"old old"}),
    )
    .unwrap()
}
fn rule() -> Value {
    json!({"id":"test","name":"Test rule","match":{"url":"http://example.test/v1/*","method":"POST"}})
}
fn fixture(breakpoint: u64) -> (tempfile::TempDir, Arc<Rules>) {
    let dir = tempfile::tempdir().unwrap();
    let rules = Rules::open(dir.path(), breakpoint, 20);
    (dir, rules)
}

#[tokio::test]
async fn values_resolve_by_id_and_updates_keep_in_flight_rules_consistent() {
    let (directory, rules) = fixture(1000);
    let values = json!([
        {"id":"body","name":"Example body","kind":"json","text":"{\"item\":1}"},
        {"id":"response","name":"Annotate response","kind":"javascript","text":"response.headers['x-from-value'] = request.body;"}
    ]);
    rules.set_values(values.clone(), &json!([])).unwrap();
    let mut referenced = rule();
    referenced["request"] = json!({"bodyValue":"body"});
    referenced["mock"] = json!({"status":403,"bodyValue":"body"});
    referenced["response"] = json!({"bodyValue":"body","scriptValue":"response"});
    let saved = rules.set(json!([referenced])).unwrap();
    assert_eq!(saved["rules"][0]["mock"]["bodyValue"], "body");
    assert!(
        !saved["rules"][0]["mock"]
            .as_object()
            .unwrap()
            .contains_key("body")
    );
    assert_eq!(saved["rules"][0]["response"]["scriptValue"], "response");
    assert!(
        !saved["rules"][0]["response"]
            .as_object()
            .unwrap()
            .contains_key("script")
    );

    let selected_before_edit = rules.matching(&request());
    let mut edited = values.clone();
    edited[0]["name"] = json!("Renamed body");
    edited[0]["text"] = json!("{\"item\":2}");
    rules.set_values(edited.clone(), &values).unwrap();
    assert_eq!(rules.get(), saved);
    assert_eq!(
        rules
            .set_values(values.clone(), &values)
            .unwrap_err()
            .status,
        409
    );

    let restarted = Rules::open(directory.path(), 1000, 20);
    assert_eq!(restarted.get(), saved);
    assert_eq!(restarted.get_values()["values"], edited);
    for (selected, expected) in [
        (selected_before_edit, "{\"item\":1}"),
        (rules.matching(&request()), "{\"item\":2}"),
        (restarted.matching(&request()), "{\"item\":2}"),
    ] {
        assert!(selected[0].request_body());
        assert!(selected[0].response_body());
        assert_eq!(selected[0].interceptors(), ["重写", "脚本"]);
        let (changed, mocked) = rules.apply_request(request(), &selected).await.unwrap();
        assert_eq!(changed.body.as_deref(), Some(expected));
        let mocked = mocked.unwrap();
        assert_eq!(mocked.status, 403);
        assert_eq!(mocked.body.as_deref(), Some(expected));
        let result = rules
            .apply_response(&changed, mocked, &selected)
            .await
            .unwrap();
        assert_eq!(result.headers["x-from-value"], expected);
        assert_eq!(result.body.as_deref(), Some(expected));
    }

    let mut settings = crate::settings::Settings::default();
    settings.rewrite_enabled = false;
    rules.configure(&settings);
    assert_eq!(rules.matching(&request())[0].interceptors(), ["脚本"]);
    settings.rewrite_enabled = true;
    settings.script_enabled = false;
    rules.configure(&settings);
    assert_eq!(rules.matching(&request())[0].interceptors(), ["重写"]);
}

#[test]
fn invalid_values_and_references_leave_saved_rules_and_values_intact() {
    let (directory, rules) = fixture(1000);
    let values = json!([
        {"id":"body","name":"Text","kind":"text","text":"1"},
        {"id":"python","name":"Python","kind":"python","text":"def onResponse(context, response): return response"}
    ]);
    rules.set_values(values.clone(), &json!([])).unwrap();
    let mut disabled = rule();
    disabled["enabled"] = json!(false);
    disabled["response"] = json!({"bodyValue":"body","pythonValue":"python"});
    let before = rules.set(json!([disabled.clone()])).unwrap();
    let path = directory.path().join("rules.json");
    let snapshot = fs::read(&path).unwrap();

    for invalid_action in [
        json!({"body":"inline","bodyValue":"body"}),
        json!({"bodyFile":"abc","bodyValue":"body"}),
        json!({"bodyValue":"python"}),
        json!({"bodyValue":"absent"}),
        json!({"bodyValue":null}),
        json!({"scriptValue":"python"}),
        json!({"script":"", "scriptValue":"python"}),
        json!({"pythonValue":"absent"}),
        json!({"python":"inline", "pythonValue":"python"}),
    ] {
        let mut invalid = disabled.clone();
        invalid["response"] = invalid_action;
        assert!(rules.set(json!([invalid])).is_err());
    }
    for invalid_mock in [
        json!({"bodyValue":"body","bodyBase64":"AA=="}),
        json!({"bodyValue":"body","body":"inline"}),
        json!({"bodyValue":3}),
    ] {
        let mut invalid = disabled.clone();
        invalid["mock"] = invalid_mock;
        assert!(rules.set(json!([invalid])).is_err());
    }

    let mut edited = values.clone();
    edited[0]["kind"] = json!("json");
    assert!(rules.set_values(edited, &values).is_err());
    assert!(rules.set_values(json!([values[1]]), &values).is_err());
    assert!(rules.set_values(json!([values[0]]), &values).is_err());
    for invalid in [
        json!({"id":"new","name":"Bad JSON","kind":"json","text":"{"}),
        json!({"id":"new","name":"Bad JavaScript","kind":"javascript","text":"if ("}),
        json!({"id":"new","name":"Empty Python","kind":"python","text":"  "}),
        values[0].clone(),
    ] {
        assert!(
            rules
                .set_values(json!([values[0], values[1], invalid]), &values)
                .is_err()
        );
    }
    assert_eq!(rules.get(), before);
    assert_eq!(rules.get_values()["values"], values);
    assert_eq!(fs::read(&path).unwrap(), snapshot);

    rules.set(json!([])).unwrap();
    rules.set_values(json!([]), &values).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
        json!({"rules":[]})
    );
}

#[test]
fn single_rule_toggle_preserves_actions_conflicts_and_failed_writes() {
    let (directory, rules) = fixture(1000);
    rules.set(json!([{"id":"script","match":{"url":"*"},"request":{"script":"request.headers['x-test']='yes';"}}])).unwrap();
    let original = rules.get()["rules"].clone();
    assert_eq!(
        rules.set_enabled("script", false, true).unwrap()["enabled"],
        false
    );
    let mut expected = original.clone();
    expected[0]["enabled"] = json!(false);
    assert_eq!(rules.get()["rules"], expected);
    assert!(rules.set_enabled("script", true, true).is_err());
    assert_eq!(rules.get()["rules"], expected);
    assert!(rules.set_enabled("missing", true, false).is_err());
    assert_eq!(
        Rules::open(directory.path(), 1000, 20).get()["rules"],
        expected
    );
    let path = directory.path().join("rules.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(rules.set_enabled("script", true, false).is_err());
    assert_eq!(rules.get()["rules"], expected);
}

#[tokio::test]
async fn heterogeneous_modifications_keep_order_case_duplicates_and_untouched_query_bytes() {
    let (_dir, rules) = fixture(1000);
    let edits = json!([
        {"type":"body","search":"one","replacement":"step","caseSensitive":false},
        {"type":"queryAdd","replacement":"stage=one"},
        {"type":"headerAdd","replacement":"X-Chain: One"},
        {"type":"body","search":"step","replacement":"done"},
        {"type":"queryReplace","search":"stage=one","replacement":"stage=two"},
        {"type":"headerReplace","search":"X-Chain: One","replacement":"X-Renamed: Two"},
        {"type":"queryRemove","search":"dup=a"},
        {"type":"headerRemove","search":"X-Old: old"},
        {"type":"body","search":"done","replacement":"wrong","enabled":false},
        {"type":"headerAdd","replacement":"X-Renamed: Three"},
        {"type":"headerReplace","search":"x-renamed: (.*)","replacement":"X-Capture: $1","regex":true}
    ]);
    rules.set(json!([{"id":"ordered","match":{"url":"*"},"request":{"mode":"modify","body":"ONE","edits":edits}}])).unwrap();
    let mut req = request();
    req.url = "http://example.test/v1?raw=%2f&flag&dup=a&dup=b".into();
    let (changed, _) = rules
        .apply_request(req.clone(), &rules.matching(&req))
        .await
        .unwrap();
    assert_eq!(changed.body.as_deref(), Some("done"));
    assert_eq!(
        changed.url,
        "http://example.test/v1?raw=%2f&flag&dup=b&stage=two"
    );
    assert_eq!(changed.headers["x-capture"], json!(["Two", "Three"]));
    assert!(!changed.headers.contains_key("x-old"));
    assert!(!changed.headers.contains_key("x-renamed"));
    assert_eq!(
        preview_edit(
            json!({"edit":{"type":"body","search":"$","replacement":"$1"},"data":"cost $"})
        )
        .unwrap()["result"],
        "cost $1"
    );
    assert_eq!(preview_edit(json!({"edit":{"type":"headerReplace","search":"X-Test: OLD","replacement":"X-Next: new","caseSensitive":false},"data":"X-Test: old\nX-Keep: yes"})).unwrap()["result"], "x-keep: yes\nx-next: new");
    assert!(preview_edit(json!({"edit":{"type":"headerReplace","search":"one","replacement":"bad\r\nx-injected: yes"},"data":"x-test: one"})).is_err());
    assert!(rules.set(json!([{"match":{"url":"*"},"response":{"edits":[{"type":"queryRemove","search":"x=1"}]}}])).is_err());
}

#[tokio::test]
async fn ordered_header_edits_preserve_binary_data_without_buffering() {
    let (_dir, rules) = fixture(1000);
    rules.set(json!([{"match":{"url":"*"},"request":{"edits":[{"type":"headerAdd","replacement":"X-One: yes"},{"type":"body","search":"one","replacement":"two","enabled":false}]},"response":{"edits":[{"type":"headerAdd","replacement":"Set-Cookie: c=3"}]}}])).unwrap();
    let mut req = request();
    req.body = None;
    req.binary_body = Some(vec![0, 255, 128, 1]);
    let selected = rules.matching(&req);
    assert!(!selected[0].request_body());
    assert!(!selected[0].response_body());
    let (changed, _) = rules.apply_request(req, &selected).await.unwrap();
    assert_eq!(changed.binary_body, Some(vec![0, 255, 128, 1]));
    assert_eq!(changed.headers["x-one"], "yes");
    let mut res = response();
    res.body = None;
    res.binary_body = Some(vec![255, 0]);
    let changed = rules
        .apply_response(&changed, res, &selected)
        .await
        .unwrap();
    assert_eq!(changed.binary_body, Some(vec![255, 0]));
    assert_eq!(changed.headers["set-cookie"], json!(["a=1", "b=2", "c=3"]));
}
async fn wait_pending(rules: &Rules) -> Value {
    for _ in 0..100 {
        let pending = rules.pending()["pending"].clone();
        if !pending.as_array().unwrap().is_empty() {
            return pending[0].clone();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("breakpoint did not appear");
}

#[tokio::test]
async fn ordered_wildcard_rules_keep_their_response_match_after_redirect() {
    let (_dir, rules) = fixture(1000);
    let mut first = rule();
    first["request"] = json!({"headers":{"X-Old":null,"X-New":"yes"},"replace":{"search":"one","replacement":"two"},"redirect":"https://gateway.test/rewritten?q=2"});
    let mut second = rule();
    second["id"] = json!("second");
    second["request"] = json!({"replace":{"search":"two","replacement":"three"}});
    second["response"] = json!({"status":201,"headers":{"X-Response":"rewritten"},"replace":{"search":"old","replacement":"new"}});
    let mut disabled = rule();
    disabled["id"] = json!("disabled");
    disabled["enabled"] = json!(false);
    disabled["request"] = json!({"body":"wrong"});
    rules.set(json!([first, second, disabled])).unwrap();
    let selected = rules.matching(&request());
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(Rule::request_body));
    let (req, _) = rules.apply_request(request(), &selected).await.unwrap();
    assert_eq!(req.url, "https://gateway.test/rewritten?q=2");
    assert_eq!(req.body.as_deref(), Some("three three"));
    assert_eq!(req.headers["host"], "gateway.test");
    assert!(!req.headers.contains_key("x-old"));
    assert_eq!(req.headers["x-new"], "yes");
    assert!(rules.matching(&req).is_empty());
    let res = rules
        .apply_response(&req, response(), &selected)
        .await
        .unwrap();
    assert_eq!(res.status, 201);
    assert_eq!(res.body.as_deref(), Some("new new"));
    assert_eq!(res.headers["set-cookie"], json!(["a=1", "b=2"]));
    assert_eq!(res.headers["x-response"], "rewritten");
    assert!(!wildcard("http://example.test/v1/item?q=2", &request().url));
    assert!(wildcard("*v1/*?q=*", &request().url));
}

#[tokio::test]
async fn mocks_and_javascript_use_request_and_response_without_host_globals() {
    let (_dir, rules) = fixture(1000);
    let mut value = rule();
    value["request"] = json!({"script":"if (typeof process !== 'undefined' || typeof require !== 'undefined') throw Error('host globals'); request.body += ' script';"});
    value["mock"] = json!({"status":418,"body":"mocked"});
    value["response"] = json!({"script":"response.status = 202; response.body = request.body + ':' + response.body.toUpperCase(); response.headers['x-script'] = 'yes';"});
    rules.set(json!([value])).unwrap();
    let selected = rules.matching(&request());
    let (req, mock) = rules.apply_request(request(), &selected).await.unwrap();
    let res = rules
        .apply_response(&req, mock.unwrap(), &selected)
        .await
        .unwrap();
    assert_eq!(res.status, 202);
    assert_eq!(res.body.as_deref(), Some("one one script:MOCKED"));
    assert_eq!(res.headers["x-script"], "yes");
}

#[tokio::test]
async fn script_time_and_output_limits_fail_explicitly() {
    let (_dir, rules) = fixture(1000);
    for (source, message) in [
        ("while (true) {}", "Script execution timed out"),
        ("request.url = 'file:///tmp/invalid'", "HTTP or HTTPS"),
        (
            "request.constructor.constructor('return process')()",
            "Code generation from strings disallowed",
        ),
        (
            "request.headers['x-inject'] = 'x\\r\\ny: yes'",
            "Invalid character",
        ),
    ] {
        let mut value = rule();
        value["request"] = json!({"script":source});
        rules.set(json!([value])).unwrap();
        let selected = rules.matching(&request());
        let error = rules
            .apply_request(request(), &selected)
            .await
            .err()
            .unwrap();
        assert!(error.message.contains(message), "{}", error.message);
    }
}

#[test]
fn invalid_updates_preserve_the_previous_private_configuration() {
    let (dir, rules) = fixture(1000);
    let mut good = rule();
    good["request"] = json!({"headers":{"X-List":["a","b"]},"body":"persisted"});
    rules.set(json!([good.clone()])).unwrap();
    let location = dir.path().join("rules.json");
    let stored = fs::read(&location).unwrap();
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&location).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let restarted = Rules::open(dir.path(), 1000, 20);
    assert_eq!(restarted.get(), rules.get());
    for action in [
        json!({"headers":{"x-injection":"value\r\nmalicious: yes"}}),
        json!({"script":"if ("}),
        json!({"typo":true}),
    ] {
        let mut invalid = rule();
        invalid["request"] = action;
        assert!(rules.set(json!([invalid])).is_err());
        assert_eq!(fs::read(&location).unwrap(), stored);
    }
    assert!(rules.set(json!([good.clone(), good])).is_err());
    assert_eq!(rules.get()["rules"][0]["request"]["body"], "persisted");
}

#[test]
fn corrupt_files_remain_intact_and_keep_their_visible_error() {
    let dir = tempfile::tempdir().unwrap();
    let location = dir.path().join("rules.json");
    fs::write(&location, "{broken").unwrap();
    let rules = Rules::open(dir.path(), 1000, 20);
    assert_eq!(rules.get()["rules"], json!([]));
    assert!(
        rules.get()["error"]
            .as_str()
            .unwrap()
            .contains("has been preserved")
    );
    assert!(rules.set(json!([{"typo":true}])).is_err());
    assert!(rules.get()["error"].is_string());
    assert_eq!(fs::read_to_string(location).unwrap(), "{broken");
}

#[tokio::test]
async fn request_and_response_breakpoints_validate_edits_before_resuming() {
    let (_dir, rules) = fixture(2000);
    let mut value = rule();
    value["request"] = json!({"breakpoint":true});
    value["response"] = json!({"breakpoint":true});
    rules.set(json!([value])).unwrap();
    let selected = rules.matching(&request());
    let task_rules = rules.clone();
    let task_selected = selected.clone();
    let task =
        tokio::spawn(async move { task_rules.apply_request(request(), &task_selected).await });
    let pending = wait_pending(&rules).await;
    let id = pending["id"].as_str().unwrap();
    let mut invalid = request();
    invalid.url = "file:///tmp/no".into();
    assert!(
        rules
            .resume(id, json!({"action":"continue","request":invalid}))
            .is_err()
    );
    assert_eq!(rules.pending()["pending"].as_array().unwrap().len(), 1);
    let mut edited = request();
    edited.body = Some("edited request".into());
    rules
        .resume(id, json!({"action":"continue","request":edited}))
        .unwrap();
    let (req, _) = task.await.unwrap().unwrap();
    assert_eq!(req.body.as_deref(), Some("edited request"));
    let task_rules = rules.clone();
    let task =
        tokio::spawn(async move { task_rules.apply_response(&req, response(), &selected).await });
    let pending = wait_pending(&rules).await;
    assert_eq!(pending["phase"], "response");
    let id = pending["id"].as_str().unwrap();
    assert!(
        rules
            .resume(id, json!({"action":"continue","request":request()}))
            .is_err()
    );
    rules.resume(id, json!({"action":"continue","response":{"status":203,"headers":{"X-Edited":"yes"},"body":"edited response"}})).unwrap();
    let res = task.await.unwrap().unwrap();
    assert_eq!(res.status, 203);
    assert_eq!(res.body.as_deref(), Some("edited response"));
    assert!(rules.resume(id, json!({"action":"continue"})).is_err());
}

#[tokio::test]
async fn drop_timeout_and_shutdown_do_not_forward_paused_work() {
    let (_dir, rules) = fixture(25);
    let mut value = rule();
    value["request"] = json!({"breakpoint":true});
    rules.set(json!([value])).unwrap();
    let selected = rules.matching(&request());
    let task_rules = rules.clone();
    let task_selected = selected.clone();
    let task =
        tokio::spawn(async move { task_rules.apply_request(request(), &task_selected).await });
    let pending = wait_pending(&rules).await;
    rules
        .resume(pending["id"].as_str().unwrap(), json!({"action":"drop"}))
        .unwrap();
    assert!(
        task.await
            .unwrap()
            .err()
            .unwrap()
            .message
            .contains("Dropped at breakpoint")
    );
    assert_eq!(
        rules
            .apply_request(request(), &selected)
            .await
            .err()
            .unwrap()
            .status,
        504
    );
    assert_eq!(rules.pending()["pending"], json!([]));
    let task_rules = rules.clone();
    let task = tokio::spawn(async move { task_rules.apply_request(request(), &selected).await });
    wait_pending(&rules).await;
    rules.close();
    assert!(
        task.await
            .unwrap()
            .err()
            .unwrap()
            .message
            .contains("closed while waiting")
    );
}

#[tokio::test]
async fn cancelled_request_and_response_tasks_remove_their_breakpoints() {
    let (_dir, rules) = fixture(2000);
    let mut value = rule();
    value["request"] = json!({"breakpoint":true});
    value["response"] = json!({"breakpoint":true});
    rules.set(json!([value])).unwrap();
    for response_phase in [false, true] {
        let task_rules = rules.clone();
        let selected = rules.matching(&request());
        let task = tokio::spawn(async move {
            if response_phase {
                task_rules
                    .apply_response(&request(), response(), &selected)
                    .await
                    .map(|_| ())
            } else {
                task_rules
                    .apply_request(request(), &selected)
                    .await
                    .map(|_| ())
            }
        });
        let pending = wait_pending(&rules).await;
        task.abort();
        let _ = task.await;
        assert_eq!(rules.pending()["pending"], json!([]));
        assert_eq!(
            rules
                .resume(
                    pending["id"].as_str().unwrap(),
                    json!({"action":"continue"})
                )
                .err()
                .unwrap()
                .status,
            404
        );
    }
}

#[tokio::test]
async fn header_only_rules_do_not_require_buffering_or_impose_a_rule_quota() {
    let (_dir, rules) = fixture(1000);
    let values: Vec<_> = (0..101)
        .map(|index| {
            let mut value = rule();
            value["id"] = json!(format!("rule-{index}"));
            value["request"] = json!({"headers":{format!("x-rule-{index}"):"yes"}});
            value
        })
        .collect();
    rules.set(json!(values)).unwrap();
    let selected = rules.matching(&request());
    assert_eq!(selected.len(), 101);
    assert!(!selected.iter().any(Rule::request_body));
    assert!(!selected.iter().any(Rule::response_body));
    let (req, _) = rules.apply_request(request(), &selected).await.unwrap();
    assert_eq!(req.headers["x-rule-100"], "yes");
    assert_eq!(req.body, request().body);
}

#[tokio::test]
async fn capture_python_rules_beyond_free_quota_keep_their_own_console_output() {
    let (_dir, rules) = fixture(1000);
    let values: Vec<_> = (0..4).map(|index| {
        let source = format!(
            "def onRequest(context, request):\n    context.shared = {{'rule': {index}}}\n    request.headers['x-local-rule-{index}'] = 'executed'\n    print('request-{index}')\n    return request\ndef onResponse(context, response):\n    assert context.shared['rule'] == {index}\n    response.headers['x-local-rule-{index}'] = 'executed'\n    print('response-{index}')\n    return response\n"
        );
        json!({"id":format!("console-local-{index}"),"match":{"url":"http://local-console.test/*"},"request":{"python":source},"response":{"python":source}})
    }).collect();
    rules.set(json!(values)).unwrap();
    let mut req = request();
    req.url = "http://local-console.test/isolated".into();
    let selected = rules.matching(&req);
    assert_eq!(selected.len(), 4);
    let mut shared = json!({});
    let (req, _) = rules
        .apply_request_flow(req, &selected, &mut shared)
        .await
        .unwrap();
    let res = rules
        .apply_response_flow(&req, response(), &selected, &mut shared)
        .await
        .unwrap();
    let logs = crate::python::logs();
    for index in 0..4 {
        let id = format!("console-local-{index}");
        let entries: Vec<_> = logs["logs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["ruleId"] == id)
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["phase"], "request");
        assert_eq!(entries[0]["text"], format!("request-{index}\n"));
        assert_eq!(entries[1]["phase"], "response");
        assert_eq!(entries[1]["text"], format!("response-{index}\n"));
        assert_eq!(req.headers[&format!("x-local-rule-{index}")], "executed");
        assert_eq!(res.headers[&format!("x-local-rule-{index}")], "executed");
    }

    // Tracebacks must retain the rule identity just like normal console output.
    assert!(
        crate::python::execute_for_rule(
            Some("console-local-error"),
            crate::python::default_executable(),
            "def onRequest(context, request):\n    raise ValueError('local console failure')\n",
            &req,
            None,
            &json!({}),
            5000
        )
        .await
        .is_err()
    );
    let logs = crate::python::logs();
    let error = logs["logs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["ruleId"] == "console-local-error")
        .unwrap();
    assert!(
        error["text"]
            .as_str()
            .unwrap()
            .contains("local console failure")
    );
}

#[tokio::test]
async fn first_redirect_rematches_and_replacement_precedes_modification() {
    let (_dir, rules) = fixture(1000);
    rules.set(json!([
        {"id":"modify","match":{"url":"https://destination.test/*"},"request":{"mode":"modify","replace":{"search":"(new)","replacement":"$1!","regex":true}}},
        {"id":"redirect","match":{"url":"http://example.test/v?/item*"},"request":{"redirect":"https://destination.test/prefix/*","preserveHost":true}},
        {"id":"other-redirect","match":{"url":"http://example.test/*"},"request":{"redirect":"https://wrong.test/"}},
        {"id":"destination-redirect","match":{"url":"https://destination.test/*"},"request":{"redirect":"https://also-wrong.test/"}},
        {"id":"replace","match":{"url":"https://destination.test/*"},"request":{"mode":"replace","body":"new","headers":{"x-only":"yes"}}},
        {"id":"source","match":{"url":"http://example.test/*"},"request":{"body":"source-only"}}
    ])).unwrap();
    let (req, selected) = rules.prepare(request()).unwrap();
    assert_eq!(req.url, "https://destination.test/prefix/v1/item?q=1");
    let (req, _) = rules.apply_request(req, &selected).await.unwrap();
    assert_eq!(req.body.as_deref(), Some("new!"));
    assert_eq!(req.headers.len(), 1);
    assert_eq!(req.headers["x-only"], "yes");
    assert!(wildcard("a?c", "a中c"));
    assert!(!wildcard("a?c", "ac"));
    assert!(wildcard("*?*", "x"));
    assert!(!wildcard("*?*", ""));
}

#[tokio::test]
async fn query_changes_preserve_unmodified_encoded_components_and_duplicates() {
    let (_dir, rules) = fixture(1000);
    rules.set(json!([{"match":{"url":"*"},"request":{"query":[{"name":"x","search":"^(.+)$","replacement":"v-$1","regex":true},{"name":"gone","remove":true},{"name":"add","value":"a b"}]}}])).unwrap();
    let mut req = request();
    req.url = "http://example.test/?raw=%2f&x=1&x=2&flag&gone=z".into();
    let (req, selected) = rules.prepare(req).unwrap();
    let (req, _) = rules.apply_request(req, &selected).await.unwrap();
    assert_eq!(
        req.url,
        "http://example.test/?raw=%2f&x=v-1&x=v-2&flag&add=a+b"
    );
}

#[tokio::test]
async fn ordered_header_and_body_edits_keep_duplicate_headers_and_reject_injection() {
    let (_dir, rules) = fixture(1000);
    let value = json!({"match":{"url":"*"},"request":{"headerEdits":[{"name":"X-Old","regex":true,"search":"^(.+)$","replacement":"prefix-$1"},{"name":"x-old","search":"prefix-old","replacement":"changed"}],"replace":{"search":"one","replacement":"two"},"bodyReplacements":[{"search":"(two)","replacement":"$1!","regex":true},{"search":"!","replacement":"?"}]},"response":{"headerEdits":[{"name":"Set-Cookie","regex":true,"search":"=([0-9])","replacement":"=${1}0"}]}});
    rules.set(json!([value])).unwrap();
    let (req, selected) = rules.prepare(request()).unwrap();
    let (req, _) = rules.apply_request(req, &selected).await.unwrap();
    assert_eq!(req.headers["x-old"], "changed");
    assert_eq!(req.body.as_deref(), Some("two? two?"));
    let response = rules
        .apply_response(&req, response(), &selected)
        .await
        .unwrap();
    assert_eq!(response.headers["set-cookie"], json!(["a=10", "b=20"]));
    assert!(rules.set(json!([{"match":{"url":"*"},"request":{"headerEdits":[{"name":"x","value":"one\r\nInjected: true"}]}}])).is_err());
    assert!(rules.set(json!([{"match":{"url":"*"},"request":{"bodyReplacements":[{"search":"(","replacement":"","regex":true}]}}])).is_err());
}

#[test]
fn interceptor_kinds_distinguish_scripts_breakpoints_and_rewrites() {
    let (_directory, rules) = fixture(1000);
    rules.set(json!([
        {"id":"script","match":{"url":"*"},"request":{"script":"request.body='ok';"}},
        {"id":"pause","match":{"url":"*"},"response":{"breakpoint":true}},
        {"id":"rewrite","match":{"url":"*"},"request":{"headers":{"x-test":"ok"}}},
        {"id":"combined","match":{"url":"*"},"response":{"status":201,"breakpoint":true,"script":"response.body='ok';"}}
    ])).unwrap();
    let selected = rules.matching(&request());
    assert_eq!(selected[0].interceptors(), vec!["脚本"]);
    assert_eq!(selected[1].interceptors(), vec!["断点"]);
    assert_eq!(selected[2].interceptors(), vec!["重写"]);
    assert_eq!(selected[3].interceptors(), vec!["重写", "断点", "脚本"]);
}
