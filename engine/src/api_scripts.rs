//! Local API pre-request and post-response scripts, backed by embedded QuickJS.
use crate::wire::{Error, RequestData, ResponseData, Result};
use rquickjs::{Context, Runtime};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct ScriptResult {
    pub request: RequestData,
    pub logs: Vec<String>,
    pub tests: Vec<Value>,
    pub variables: Value,
}

pub fn execute(
    source: &str,
    request: &RequestData,
    response: Option<&ResponseData>,
    variables: &Value,
) -> Result<ScriptResult> {
    if source.len() > 1024 * 1024 {
        return Err(Error::bad("API script exceeds 1 MiB"));
    }
    if !variables.is_object() {
        return Err(Error::bad("API variables must be an object"));
    }
    let runtime = Runtime::new().map_err(Error::bad)?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + Duration::from_millis(250);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime).map_err(Error::bad)?;
    context.with(|ctx| {
        let code = format!(r#"
        "use strict";
        const request = {request};
        const response = {response};
        const environment = {variables};
        const logs = [], tests = [];
        const console = {{log: (...items) => {{ if (logs.length < 100) logs.push(items.map(v => typeof v === 'string' ? v : JSON.stringify(v)).join(' ').slice(0,4096)); }}}};
        console.error = console.warn = console.log;
        function assert(condition, message) {{ if (!condition) throw new Error(message || 'Assertion failed'); }}
        function test(name, action) {{
            try {{ const value=action(); if (value && typeof value.then === 'function') throw new Error('Async tests are not supported'); tests.push({{name:String(name),passed:true}}); }}
            catch (error) {{ tests.push({{name:String(name),passed:false,error:String(error.message || error)}}); }}
        }}
        const envAPI = {{get:k=>environment[k],set:(k,v)=>{{environment[k]=String(v)}},unset:k=>{{delete environment[k]}},has:k=>Object.hasOwn(environment,k)}};
        const pm = {{request, response: response ? {{...response,code:response.status,text:()=>response.body||'',json:()=>JSON.parse(response.body||'')}} : null, environment:envAPI, variables:envAPI, test}};
        const noCodegen = function() {{ throw new Error('Dynamic code generation is disabled'); }};
        Function.prototype.constructor = noCodegen; globalThis.Function = noCodegen; globalThis.eval = noCodegen;
        (()=>{{
        {source}
        }})();
        JSON.stringify({{request,logs,tests,variables:environment}})
        "#, request=serde_json::to_string(request)?,response=serde_json::to_string(&response)?,variables=serde_json::to_string(variables)?,source=source);
        let result: rquickjs::Result<String>=ctx.eval(code);
        let encoded=match result {
            Ok(value)=>value,
            Err(error)=> {
                if Instant::now()>=deadline{return Err(Error::bad("API script timed out after 250 ms"));}
                let message=ctx.catch().into_exception().and_then(|e|e.message()).unwrap_or_else(||error.to_string());
                return Err(Error::bad(format!("API script: {message}")));
            }
        };
        let value:Value=serde_json::from_str(&encoded)?;
        let variables=value["variables"].clone();
        if !variables.as_object().is_some_and(|v|v.values().all(Value::is_string)){return Err(Error::bad("API environment values must be strings"));}
        Ok(ScriptResult {request:serde_json::from_value::<RequestData>(value["request"].clone())?.validate()?,logs:serde_json::from_value(value["logs"].clone())?,tests:serde_json::from_value(value["tests"].clone())?,variables})
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scripts_change_request_and_report_assertions_without_hiding_failure() {
        let request = RequestData {
            method: "GET".into(),
            url: "http://example.test/".into(),
            headers: Default::default(),
            body: None,
            binary_body: None,
        };
        let response = ResponseData {
            status: 201,
            headers: Default::default(),
            body: Some("{\"ok\":true}".into()),
            binary_body: None,
        };
        let result=execute("request.headers['x-token']=environment.token; console.log('hello'); pm.environment.set('next','2'); test('status',()=>assert(response.status===201)); pm.test('failure',()=>assert(false,'expected'));",&request,Some(&response),&json!({"token":"test"})).unwrap();
        assert_eq!(result.request.headers["x-token"], "test");
        assert_eq!(result.logs, vec!["hello"]);
        assert_eq!(result.variables["next"], "2");
        assert_eq!(result.tests[0]["passed"], true);
        assert_eq!(result.tests[1]["passed"], false);
        assert!(
            execute("while(true){}", &request, None, &json!({}))
                .unwrap_err()
                .message
                .contains("timed out")
        );
    }
}
