//! Saved display filters; they never participate in traffic routing or rules.
use crate::{
    storage,
    wire::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const MAX_FILTERS: usize = 256;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyCondition {
    pub member: String,
    pub equals: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub id: String,
    pub url: String,
    pub body: BodyCondition,
    pub enabled: bool,
}

impl Filter {
    fn validate(&self) -> Result<()> {
        crate::wire::parse_url(&self.url)?;
        if self.url.contains('#')
            || self.url.len() > 16384
            || self.body.member.is_empty()
            || self.body.member.len() > 1024
        {
            return Err(Error::bad(
                "Filter requires a full HTTP(S) URL without a fragment and a nonempty JSON body member (max 1024 bytes)",
            ));
        }
        Ok(())
    }

    pub fn matches(&self, url: &str, body: Option<&Value>) -> bool {
        self.url == url
            && body
                .and_then(Value::as_object)
                .and_then(|body| body.get(&self.body.member))
                == Some(&self.body.equals)
    }
}

pub struct Filters {
    path: PathBuf,
    pub items: Vec<Filter>,
}

impl Filters {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("capture-list-filters.json");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES as u64) {
            return Err(Error::bad(
                "Capture list filters exceed 1 MiB; file preserved",
            ));
        }
        let items = match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > MAX_BYTES {
                    return Err(Error::bad(
                        "Capture list filters exceed 1 MiB; file preserved",
                    ));
                }
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Saved {
                    version: u32,
                    filters: Vec<Filter>,
                }
                let saved: Saved = serde_json::from_slice(&bytes)?;
                if saved.version != 1 || saved.filters.len() > MAX_FILTERS {
                    return Err(Error::bad(
                        "Invalid saved capture list filters; file preserved",
                    ));
                }
                let mut ids = HashSet::new();
                for filter in &saved.filters {
                    filter.validate()?;
                    if uuid::Uuid::parse_str(&filter.id).is_err() || !ids.insert(&filter.id) {
                        return Err(Error::bad("Invalid or duplicate capture list filter ID"));
                    }
                }
                storage::private_file(&path)?;
                saved.filters
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, items })
    }

    pub fn update(&mut self, method: &str, id: Option<&str>, input: Value) -> Result<Value> {
        let fields = input
            .as_object()
            .ok_or_else(|| Error::bad("Filter object required"))?;
        let allowed: &[&str] = match method {
            "POST" => &["url", "body"],
            "PATCH" => &["enabled"],
            "DELETE" => &[],
            _ => return Err(Error::bad("Invalid capture list filter operation")),
        };
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::bad("Unexpected capture list filter field"));
        }
        let mut next = self.items.clone();
        let result = match (method, id) {
            ("POST", None) => {
                if next.len() >= MAX_FILTERS {
                    return Err(Error::bad("Capture list filter limit reached (256)"));
                }
                let filter = Filter {
                    id: uuid::Uuid::new_v4().to_string(),
                    url: input["url"]
                        .as_str()
                        .ok_or_else(|| Error::bad("Filter URL required"))?
                        .into(),
                    body: serde_json::from_value(input["body"].clone())
                        .map_err(|e| Error::bad(format!("Invalid JSON body condition: {e}")))?,
                    enabled: true,
                };
                filter.validate()?;
                let result = json!(filter);
                next.push(filter);
                result
            }
            ("PATCH" | "DELETE", Some(id)) => {
                let index = next
                    .iter()
                    .position(|f| f.id == id)
                    .ok_or_else(|| Error::new(404, "Capture list filter not found"))?;
                if method == "DELETE" {
                    next.remove(index);
                    json!({"deleted":id})
                } else {
                    next[index].enabled = input["enabled"]
                        .as_bool()
                        .ok_or_else(|| Error::bad("enabled must be a boolean"))?;
                    json!(next[index])
                }
            }
            _ => return Err(Error::bad("Invalid capture list filter operation")),
        };
        let bytes = serde_json::to_vec(&json!({"version":1,"filters":next}))?;
        if bytes.len() > MAX_BYTES {
            return Err(Error::bad("Capture list filters exceed 1 MiB"));
        }
        storage::atomic_write(&self.path, &bytes)?;
        self.items = next;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_updates_and_failed_saves_preserve_filters() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Filters::load(dir.path()).unwrap();
        let filter = store.update("POST", None, json!({"url":"http://capture.test/path?q=1","body":{"member":"id","equals":"target"}})).unwrap();
        let saved = std::fs::read(&store.path).unwrap();
        for input in [
            json!({}),
            json!({"url":"file:///tmp/x","body":{"member":"id","equals":1}}),
            json!({"url":"http://capture.test/#x","body":{"member":"id","equals":1}}),
            json!({"url":"http://capture.test/","body":{"member":"","equals":1}}),
            json!({"url":"http://capture.test/","body":{"member":"id"}}),
        ] {
            assert!(store.update("POST", None, input).is_err());
            assert_eq!(std::fs::read(&store.path).unwrap(), saved);
        }
        let id = filter["id"].as_str().unwrap();
        assert!(
            store
                .update("PATCH", Some(id), json!({"enabled":"yes"}))
                .is_err()
        );
        assert!(store.update("DELETE", Some("missing"), json!({})).is_err());
        std::fs::remove_file(&store.path).unwrap();
        std::fs::create_dir(&store.path).unwrap();
        assert!(store.update("DELETE", Some(id), json!({})).is_err());
        assert!(
            store
                .update("PATCH", Some(id), json!({"enabled":false}))
                .is_err()
        );
        assert!(store.items[0].enabled);
        std::fs::remove_dir(&store.path).unwrap();
        std::fs::write(&store.path, b"broken").unwrap();
        assert!(Filters::load(dir.path()).is_err());
        assert_eq!(std::fs::read(&store.path).unwrap(), b"broken");
    }
}
