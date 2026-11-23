use super::*;
use crate::storage::Session;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field {
    #[default]
    All,
    Url,
    Host,
    Path,
    Method,
    Status,
    Server,
    Client,
    Application,
    Interceptor,
    Console,
    RequestType,
    ResponseType,
    RequestHeaders,
    ResponseHeaders,
    RequestBody,
    ResponseBody,
}

impl Field {
    pub const ALL: [Self; 17] = [
        Self::All,
        Self::Url,
        Self::Host,
        Self::Path,
        Self::Method,
        Self::Status,
        Self::Server,
        Self::Client,
        Self::Application,
        Self::Interceptor,
        Self::Console,
        Self::RequestType,
        Self::ResponseType,
        Self::RequestHeaders,
        Self::ResponseHeaders,
        Self::RequestBody,
        Self::ResponseBody,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Url => "URL",
            Self::Host => "域名",
            Self::Path => "路径",
            Self::Method => "方法",
            Self::Status => "状态码",
            Self::Server => "服务器地址",
            Self::Client => "客户端地址",
            Self::Application => "应用程序",
            Self::Interceptor => "拦截器",
            Self::Console => "控制台",
            Self::RequestType => "请求类型",
            Self::ResponseType => "响应类型",
            Self::RequestHeaders => "请求头",
            Self::ResponseHeaders => "响应头",
            Self::RequestBody => "请求体",
            Self::ResponseBody => "响应体",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    #[default]
    Contains,
    NotContains,
    Equal,
    NotEqual,
    Starts,
    Ends,
    Regex,
    NotRegex,
    Wildcard,
    NotWildcard,
}
impl Operator {
    pub const ALL: [Self; 10] = [
        Self::Contains,
        Self::NotContains,
        Self::Equal,
        Self::NotEqual,
        Self::Starts,
        Self::Ends,
        Self::Regex,
        Self::NotRegex,
        Self::Wildcard,
        Self::NotWildcard,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "包含",
            Self::NotContains => "不包含",
            Self::Equal => "等于",
            Self::NotEqual => "不等于",
            Self::Starts => "开头为",
            Self::Ends => "结尾为",
            Self::Regex => "匹配正则",
            Self::NotRegex => "不匹配正则",
            Self::Wildcard => "匹配通配符",
            Self::NotWildcard => "不匹配通配符",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Condition {
    pub field: Field,
    pub operator: Operator,
    pub value: String,
    pub enabled: bool,
    pub sensitive: bool,
    pub deep: bool,
}
impl Default for Condition {
    fn default() -> Self {
        Self {
            field: Field::All,
            operator: Operator::Contains,
            value: String::new(),
            enabled: true,
            sensitive: false,
            deep: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConditionSet {
    pub any: bool,
    pub items: Vec<Condition>,
}
impl ConditionSet {
    pub fn active(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.enabled && !item.value.is_empty())
    }
    fn compile(&self) -> Result<Vec<Matcher<'_>>> {
        self.items
            .iter()
            .filter(|item| item.enabled && !item.value.is_empty())
            .map(Matcher::new)
            .collect()
    }
    pub fn validate(&self) -> std::result::Result<(), String> {
        self.compile()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
struct Matcher<'a> {
    condition: &'a Condition,
    value: String,
    pattern: Option<regex::Regex>,
}
impl<'a> Matcher<'a> {
    fn new(condition: &'a Condition) -> Result<Self> {
        let value = if condition.sensitive {
            condition.value.clone()
        } else {
            condition.value.to_lowercase()
        };
        let pattern = match condition.operator {
            Operator::Regex | Operator::NotRegex => Some(condition.value.clone()),
            Operator::Wildcard | Operator::NotWildcard => {
                let mut pattern = String::from("^");
                for (i, segment) in condition.value.split('*').enumerate() {
                    if i > 0 {
                        pattern.push_str(".*");
                    }
                    pattern.push_str(&regex::escape(segment).replace("\\?", "."));
                }
                pattern.push('$');
                Some(pattern)
            }
            _ => None,
        }
        .map(|pattern| {
            regex::RegexBuilder::new(&pattern)
                .case_insensitive(!condition.sensitive)
                .size_limit(256 * 1024)
                .build()
                .map_err(Error::bad)
        })
        .transpose()?;
        Ok(Self {
            condition,
            value,
            pattern,
        })
    }

    fn matches(&self, session: &Session) -> bool {
        let url = url::Url::parse(&session.url).ok();
        let status = session.status.to_string();
        let content_type = |headers: &str| -> String {
            headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-type")
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_default()
        };
        let request_type;
        let response_type;
        let all;
        let console;
        let interceptors;
        let actual = match self.condition.field {
            Field::Url => &session.url,
            Field::Host => url.as_ref().and_then(url::Url::host_str).unwrap_or(""),
            Field::Path => url.as_ref().map(url::Url::path).unwrap_or(""),
            Field::Method => &session.method,
            Field::Status => &status,
            Field::Server => session.server_ip.as_deref().unwrap_or(""),
            Field::Client => session.client_ip.as_deref().unwrap_or(""),
            Field::Application => session
                .application
                .as_ref()
                .map(|application| application.name.as_str())
                .unwrap_or(""),
            Field::Interceptor => {
                interceptors = session.interceptors.join("\n");
                &interceptors
            }
            Field::Console => {
                console = session.script_logs.join("\n");
                &console
            }
            Field::RequestType => {
                request_type = content_type(&session.request_headers);
                &request_type
            }
            Field::ResponseType => {
                response_type = content_type(&session.response_headers);
                &response_type
            }
            Field::RequestHeaders => &session.request_headers,
            Field::ResponseHeaders => &session.response_headers,
            Field::RequestBody => &session.request_body,
            Field::ResponseBody => &session.response_body,
            Field::All => {
                let mut fields = vec![
                    session.url.as_str(),
                    &session.method,
                    &status,
                    &session.request_headers,
                    &session.response_headers,
                    session.server_ip.as_deref().unwrap_or(""),
                    session.client_ip.as_deref().unwrap_or(""),
                ];
                if self.condition.deep {
                    fields.extend([session.request_body.as_str(), &session.response_body]);
                    fields.extend(session.script_logs.iter().map(String::as_str));
                }
                all = fields.join("\n");
                &all
            }
        };
        if let Some(regex) = &self.pattern {
            return regex.is_match(actual)
                ^ matches!(
                    self.condition.operator,
                    Operator::NotRegex | Operator::NotWildcard
                );
        }
        let folded;
        let actual = if self.condition.sensitive {
            actual
        } else {
            folded = actual.to_lowercase();
            &folded
        };
        match self.condition.operator {
            Operator::Contains => actual.contains(&self.value),
            Operator::NotContains => !actual.contains(&self.value),
            Operator::Equal => actual == self.value,
            Operator::NotEqual => actual != self.value,
            Operator::Starts => actual.starts_with(&self.value),
            Operator::Ends => actual.ends_with(&self.value),
            _ => false,
        }
    }
}

impl DesktopEngine {
    pub fn filter_captures(
        &self,
        ids: &[String],
        conditions: &ConditionSet,
    ) -> std::result::Result<Vec<String>, String> {
        let run = || -> Result<Vec<String>> {
            let matchers = conditions.compile()?;
            if matchers.is_empty() {
                return Ok(ids.to_vec());
            }
            let history = self.running.engine.history.lock().unwrap();
            let mut matches = Vec::new();
            for id in ids {
                if let Some(session) = history.get(id)? {
                    let pass = if conditions.any {
                        matchers.iter().any(|rule| rule.matches(&session))
                    } else {
                        matchers.iter().all(|rule| rule.matches(&session))
                    };
                    if pass {
                        matches.push(id.clone());
                    }
                }
            }
            Ok(matches)
        };
        run().map_err(|error| error.to_string())
    }
}
