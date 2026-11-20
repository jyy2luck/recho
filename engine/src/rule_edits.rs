use regex::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::{self, Error, Headers, Result};

fn yes() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Modification {
    pub r#type: String,
    #[serde(default)]
    search: String,
    #[serde(default)]
    replacement: String,
    #[serde(default)]
    regex: bool,
    #[serde(default = "yes")]
    case_sensitive: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Modification {
    pub fn validate(&self, phase: &str) -> Result<()> {
        if ![
            "body",
            "queryAdd",
            "queryReplace",
            "queryRemove",
            "headerAdd",
            "headerReplace",
            "headerRemove",
        ]
        .contains(&self.r#type.as_str())
        {
            return Err(Error::bad("Unknown rewrite edit type"));
        }
        if self.r#type.starts_with("query") && phase != "request" {
            return Err(Error::bad("Query edits require a request action"));
        }
        if self.r#type.ends_with("Add") {
            if self.r#type == "headerAdd" {
                one_header(&self.replacement)?;
            } else {
                query_pair(&self.replacement)?;
            }
        } else {
            if self.search.is_empty() {
                return Err(Error::bad("Rewrite match must not be empty"));
            }
            self.matcher()?;
        }
        Ok(())
    }

    fn matcher(&self) -> Result<Regex> {
        let mut search = self.search.clone();
        // Header names are case insensitive and stored canonically by HTTP/2 and
        // Hyper. Literal matching normalizes only the name, keeping value casing.
        if !self.regex && self.r#type.starts_with("header") {
            if let Some((name, value)) = search.split_once(':') {
                search = format!("{}:{}", name.to_ascii_lowercase(), value);
            }
        }
        RegexBuilder::new(&if self.regex {
            search
        } else {
            regex::escape(&search)
        })
        .case_insensitive(!self.case_sensitive)
        .build()
        .map_err(Error::bad)
    }

    fn replace(&self, input: &str) -> Result<String> {
        let matcher = self.matcher()?;
        Ok(if self.regex {
            matcher
                .replace_all(input, self.replacement.as_str())
                .into_owned()
        } else {
            matcher
                .replace_all(input, NoExpand(&self.replacement))
                .into_owned()
        })
    }

    fn query(&self, query: &str) -> Result<String> {
        let mut parts: Vec<String> = if query.is_empty() {
            vec![]
        } else {
            query.split('&').map(str::to_owned).collect()
        };
        if self.r#type == "queryAdd" {
            let (name, value) = query_pair(&self.replacement)?;
            parts.push(encode_pair(name, value));
        } else {
            let matcher = self.matcher()?;
            let mut changed = Vec::with_capacity(parts.len());
            for raw in parts {
                let (name, value) = url::form_urlencoded::parse(raw.as_bytes())
                    .next()
                    .unwrap_or_default();
                let line = format!("{name}={value}");
                if !matcher.is_match(&line) {
                    changed.push(raw);
                    continue;
                }
                if self.r#type == "queryRemove" {
                    continue;
                }
                let replaced = self.replace(&line)?;
                let (name, value) = query_pair(&replaced)?;
                changed.push(encode_pair(name, value));
            }
            parts = changed;
        }
        Ok(parts.join("&"))
    }

    fn headers(&self, headers: &mut Headers) -> Result<()> {
        let mut lines: Vec<String> = wire::header_text(headers)
            .lines()
            .map(str::to_owned)
            .collect();
        if self.r#type == "headerAdd" {
            one_header(&self.replacement)?;
            lines.push(self.replacement.clone());
        } else {
            let matcher = self.matcher()?;
            let mut changed = Vec::with_capacity(lines.len());
            for line in lines {
                if !matcher.is_match(&line) {
                    changed.push(line);
                    continue;
                }
                if self.r#type == "headerRemove" {
                    continue;
                }
                let replaced = self.replace(&line)?;
                one_header(&replaced)?;
                changed.push(replaced);
            }
            lines = changed;
        }
        *headers = wire::parse_headers(&lines.join("\n"))?;
        Ok(())
    }

    pub fn apply(
        &self,
        url: Option<&mut String>,
        headers: &mut Headers,
        body: &mut Option<String>,
        binary: &mut Option<Vec<u8>>,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.r#type == "body" {
            let input = if let Some(text) = body.as_ref() {
                text.as_str()
            } else if let Some(bytes) = binary.as_ref() {
                std::str::from_utf8(bytes).map_err(|_| {
                    Error::bad("Modify data requires UTF-8 text; use Python to edit binary bodies")
                })?
            } else {
                return Err(Error::bad("Modify data requires a buffered body"));
            };
            *body = Some(self.replace(input)?);
            *binary = None;
        } else if self.r#type.starts_with("header") {
            self.headers(headers)?;
        } else {
            let target = url.ok_or_else(|| Error::bad("Query edits require a request"))?;
            let mut parsed = wire::parse_url(target)?;
            let query = self.query(parsed.query().unwrap_or(""))?;
            parsed.set_query(if query.is_empty() { None } else { Some(&query) });
            *target = parsed.to_string();
        }
        Ok(())
    }

    pub fn preview(&self, data: &str) -> Result<String> {
        self.validate("request")?;
        if !self.enabled {
            return Ok(data.into());
        }
        if self.r#type == "body" {
            self.replace(data)
        } else if self.r#type.starts_with("query") {
            self.query(data)
        } else {
            let mut headers = wire::parse_headers(data)?;
            self.headers(&mut headers)?;
            Ok(wire::header_text(&headers))
        }
    }
}

fn query_pair(value: &str) -> Result<(&str, &str)> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| Error::bad("Query edit must use key=value"))?;
    if name.is_empty() || name.chars().any(char::is_control) || value.chars().any(char::is_control)
    {
        return Err(Error::bad(
            "Query edit requires a nonempty name and no control characters",
        ));
    }
    Ok((name, value))
}
fn encode_pair(name: &str, value: &str) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair(name, value);
    serializer.finish()
}
fn one_header(line: &str) -> Result<()> {
    if line.contains(['\r', '\n']) || line.trim().is_empty() {
        return Err(Error::bad(
            "A header edit must contain one Name: Value line",
        ));
    }
    wire::parse_headers(line)?;
    Ok(())
}

pub fn preview(input: Value) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        edit: Modification,
        data: String,
    }
    let input: Input = serde_json::from_value(input)?;
    Ok(serde_json::json!({"result":input.edit.preview(&input.data)?}))
}
