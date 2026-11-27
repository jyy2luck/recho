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
    Highlight,
    Notes,
}

impl Field {
    pub const ALL: [Self; 16] = [
        Self::All,
        Self::Url,
        Self::Method,
        Self::RequestType,
        Self::RequestHeaders,
        Self::RequestBody,
        Self::ResponseType,
        Self::ResponseHeaders,
        Self::ResponseBody,
        Self::Status,
        Self::Server,
        Self::Client,
        Self::Highlight,
        Self::Interceptor,
        Self::Notes,
        Self::Console,
    ];
    pub const LEGACY: [Self; 3] = [Self::Host, Self::Path, Self::Application];

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
            Self::Highlight => "高亮",
            Self::Notes => "备注",
        }
    }

    pub fn choices(self) -> &'static [&'static str] {
        match self {
            Self::Method => &[
                "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT", "TRACE",
            ],
            Self::Status => &[
                "100", "101", "102", "103", "200", "201", "202", "203", "204", "205", "206", "207",
                "208", "226", "300", "301", "302", "303", "304", "305", "306", "307", "308", "400",
                "401", "402", "403", "404", "405", "406", "407", "408", "409", "410", "411", "412",
                "413", "414", "415", "416", "417", "418", "421", "422", "423", "424", "425", "426",
                "428", "429", "431", "444", "494", "495", "496", "497", "499", "451", "500", "501",
                "502", "503", "504", "505", "506", "507", "508", "510", "511", "521", "522", "523",
                "524", "525", "526", "527",
            ],
            Self::Highlight => &["无", "红色", "黄色", "绿色", "蓝色", "青色", "划线"],
            Self::Interceptor => &["镜像", "网关", "重写", "断点", "脚本"],
            _ => &[],
        }
    }

    pub fn supports_deep(self) -> bool {
        matches!(self, Self::All | Self::RequestBody | Self::ResponseBody)
    }

    pub fn operators(self) -> &'static [Operator] {
        if self.choices().is_empty() {
            &Operator::ALL
        } else {
            &Operator::ALL[..2]
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
        Self::Wildcard,
        Self::NotWildcard,
        Self::Regex,
        Self::NotRegex,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "包含",
            Self::NotContains => "不包含",
            Self::Equal => "等于",
            Self::NotEqual => "不等于",
            Self::Starts => "开始于",
            Self::Ends => "结束于",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
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
            values: None,
            enabled: true,
            sensitive: false,
            deep: false,
        }
    }
}

impl Condition {
    pub fn set_field(&mut self, field: Field) {
        let enumerated = !field.choices().is_empty();
        if enumerated || !self.field.choices().is_empty() {
            self.value.clear();
            self.values = enumerated.then(Vec::new);
        }
        self.field = field;
        self.restrict_operator();
    }

    pub fn restrict_operator(&mut self) {
        if !self.field.operators().contains(&self.operator) {
            self.operator = match self.operator {
                Operator::NotEqual | Operator::NotRegex | Operator::NotWildcard => {
                    Operator::NotContains
                }
                _ => Operator::Contains,
            };
        }
    }

    fn active(&self) -> bool {
        self.enabled
            && match self.choices() {
                Some(values) => values.iter().any(|value| !value.is_empty()),
                None => !self.value.is_empty(),
            }
    }

    fn choices(&self) -> Option<&[String]> {
        (!self.field.choices().is_empty())
            .then_some(self.values.as_deref())
            .flatten()
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
        self.items.iter().any(Condition::active)
    }
    fn compile(&self) -> Result<Vec<Matcher<'_>>> {
        self.items
            .iter()
            .filter(|item| item.active())
            .map(Matcher::new)
            .collect()
    }
    pub fn validate(&self) -> std::result::Result<(), String> {
        self.compile()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn history_matcher(&self) -> Result<impl Fn(&Session) -> bool + '_> {
        let matchers = self.compile()?;
        Ok(move |session: &Session| {
            matchers.is_empty() || matches_set(&matchers, self.any, session, "无")
        })
    }
}

fn content_type(headers: &str) -> &str {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-type")
                .then(|| value.split(';').next().unwrap_or_default().trim())
        })
        .unwrap_or_default()
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
        let pattern = match (condition.choices(), condition.operator) {
            (Some(_), _) => None,
            (_, Operator::Regex | Operator::NotRegex) => Some(condition.value.clone()),
            (_, Operator::Wildcard | Operator::NotWildcard) => {
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

    fn matches(&self, session: &Session, highlight: &str) -> bool {
        let field = self.condition.field;
        let status = session.status.to_string();
        if let Some(values) = self.condition.choices() {
            let matched =
                values
                    .iter()
                    .filter(|value| !value.is_empty())
                    .any(|value| match field {
                        Field::Method => value == &session.method,
                        Field::Status => value == &status,
                        Field::Highlight => value == highlight,
                        Field::Interceptor => session.interceptors.contains(value),
                        _ => false,
                    });
            return matched
                ^ matches!(
                    self.condition.operator,
                    Operator::NotContains | Operator::NotEqual
                );
        }
        let url = matches!(field, Field::Host | Field::Path)
            .then(|| url::Url::parse(&session.url).ok())
            .flatten();
        let all;
        let console;
        let interceptors;
        let actual = match field {
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
            Field::RequestType => content_type(&session.request_headers),
            Field::ResponseType => content_type(&session.response_headers),
            Field::RequestHeaders => &session.request_headers,
            Field::ResponseHeaders => &session.response_headers,
            // shortcut: Body searches use the capture preview. Read archived bodies if full-body search is required.
            Field::RequestBody => &session.request_body,
            Field::ResponseBody => &session.response_body,
            Field::Highlight => highlight,
            Field::Notes => &session.notes,
            Field::All => {
                let mut fields = vec![
                    session.url.as_str(),
                    &session.method,
                    &status,
                    &session.request_headers,
                    &session.response_headers,
                    session.server_ip.as_deref().unwrap_or(""),
                    session.client_ip.as_deref().unwrap_or(""),
                    &session.notes,
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
        highlights: &ConditionSet,
        highlight: &str,
    ) -> std::result::Result<CaptureMatches, String> {
        let run = || -> Result<CaptureMatches> {
            let filters = conditions.compile()?;
            let markers = highlights.compile()?;
            let mut visible = (!filters.is_empty()).then(Vec::new);
            let mut marked = Vec::new();
            let history = self.running.engine.history.lock().unwrap();
            for id in ids {
                if let Some(session) = history.get(id)? {
                    let is_marked = !markers.is_empty()
                        && matches_set(&markers, highlights.any, &session, "无");
                    if is_marked {
                        marked.push(id.clone());
                    }
                    if let Some(visible) = &mut visible {
                        let color = if is_marked { highlight } else { "无" };
                        if matches_set(&filters, conditions.any, &session, color) {
                            visible.push(id.clone());
                        }
                    }
                }
            }
            Ok(CaptureMatches { visible, marked })
        };
        run().map_err(|error| error.to_string())
    }
}

pub struct CaptureMatches {
    pub visible: Option<Vec<String>>,
    pub marked: Vec<String>,
}

fn matches_set(matchers: &[Matcher<'_>], any: bool, session: &Session, highlight: &str) -> bool {
    if any {
        matchers.iter().any(|rule| rule.matches(session, highlight))
    } else {
        matchers.iter().all(|rule| rule.matches(session, highlight))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filters_use_saved_metadata_and_the_displayed_highlight() {
        let directory = tempfile::tempdir().unwrap();
        let engine = DesktopEngine::start(directory.path().into(), 0)
            .await
            .unwrap();
        let records = [
            ("get", "GET", 200, "other"),
            ("post", "POST", 404, "review"),
            ("delete", "DELETE", 200, "review"),
        ];
        engine
            .running
            .engine
            .history
            .lock()
            .unwrap()
            .import_records(
                records
                    .into_iter()
                    .map(|(id, method, status, body)| {
                        (
                            Session {
                                id: id.into(),
                                method: method.into(),
                                status,
                                request_body: body.into(),
                                response_headers: "CONTENT-TYPE: text/plain; charset=utf-8".into(),
                                notes: body.into(),
                                interceptors: vec!["重写".into(), "脚本".into()],
                                ..Session::default()
                            },
                            None,
                            None,
                        )
                    })
                    .collect(),
            )
            .unwrap();
        let ids = records.map(|(id, ..)| id.to_owned());
        let mut conditions: ConditionSet = serde_json::from_value(json!({
            "items": [
                {"field":"Method","values":["GET","POST"]},
                {"field":"Highlight","values":["划线"]},
                {"field":"Status","operator":"NotContains","values":["40"]},
                {"field":"Interceptor","values":["脚本"]},
                {"field":"ResponseType","operator":"Equal","value":"text/plain"}
            ]
        }))
        .unwrap();
        let mut highlights: ConditionSet = serde_json::from_value(json!({
            "items": [{"field":"Notes","value":"review"}]
        }))
        .unwrap();
        let matches = engine
            .filter_captures(&ids, &conditions, &highlights, "划线")
            .unwrap();
        assert_eq!(matches.visible.unwrap(), ["post"]);
        assert_eq!(matches.marked, ["post", "delete"]);

        conditions.any = true;
        conditions.items.truncate(2);
        conditions.items.push(Condition::default());
        conditions.items.push(Condition {
            field: Field::Url,
            operator: Operator::Regex,
            value: "[".into(),
            enabled: false,
            ..Condition::default()
        });
        highlights.items.clear();
        let matches = engine
            .filter_captures(&ids, &conditions, &highlights, "划线")
            .unwrap();
        assert_eq!(matches.visible.unwrap(), ["get", "post"]);
        assert!(matches.marked.is_empty());
        conditions.items[3].enabled = true;
        assert!(conditions.validate().is_err());
        engine.stop().await;
    }

    #[test]
    fn changing_control_type_resets_only_incompatible_values() {
        let mut item = Condition {
            value: "keep me".into(),
            operator: Operator::NotRegex,
            ..Condition::default()
        };
        item.set_field(Field::RequestBody);
        assert_eq!(item.value, "keep me");
        item.set_field(Field::Method);
        assert_eq!(item.operator, Operator::NotContains);
        assert!(item.value.is_empty());
        item.values.as_mut().unwrap().push("GET".into());
        item.set_field(Field::Url);
        assert!(item.values.is_none());
        assert!(item.value.is_empty());
    }
}
