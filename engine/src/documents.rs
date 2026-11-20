//! Private, compare-and-save storage shared by the Flutter workspace and library.
use crate::{
    storage::atomic_write,
    wire::{Error, Result},
};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, sync::Mutex};

static WRITES: Mutex<()> = Mutex::new(());
const LIMIT: usize = 64 * 1024 * 1024;

fn defaults(kind: &str) -> Value {
    if kind == "platform" {
        json!({"version":1,"entries":[]})
    } else if kind == "library" {
        json!({"version":2,"bookmarks":[],"folders":[],"comparisonPool":[],"comparisonPair":[]})
    } else {
        json!({"version":1,"requests":[],"captures":[{"id":"default","name":"调试","group":"default","sessions":[],"search":"","conditions":[],"matchAll":true,"protocolFilter":"All","statusFilter":"All","hostFilter":"","selectedIDs":[],"hiddenIDs":[],"sortField":"时间","ascending":false,"columnNames":["方法","URL","应用程序","状态","耗时","大小"]}],"activeCaptureID":"default","highlights":[],"environments":[],"collections":[],"collectionFolders":["我的集合"],"scriptTemplates":[],"specifications":[],"protocolHistory":[]})
    }
}
fn validate(kind: &str, value: &Value) -> Result<()> {
    if kind == "platform" {
        if value["version"] != 1 || !value["entries"].is_array() {
            return Err(Error::bad("Invalid system proxy journal"));
        }
        for entry in value["entries"].as_array().unwrap() {
            if entry["id"].as_str().is_none_or(str::is_empty)
                || !entry["original"].is_object()
                || !entry["applied"].is_object()
            {
                return Err(Error::bad(
                    "Invalid system proxy ownership entry; journal preserved",
                ));
            }
        }
        return Ok(());
    }
    if value["version"] != if kind == "library" { 2 } else { 1 } {
        return Err(Error::bad(
            "Unsupported local archive version; original file preserved",
        ));
    }
    let lists: &[&str] = if kind == "library" {
        &["bookmarks", "comparisonPool"]
    } else {
        &[
            "requests",
            "captures",
            "environments",
            "collections",
            "highlights",
            "scriptTemplates",
        ]
    };
    for name in lists {
        let rows = value[name]
            .as_array()
            .ok_or_else(|| Error::bad(format!("{name} must be an array")))?;
        let mut ids = HashSet::new();
        for row in rows {
            let id = row["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::bad(format!("{name} needs nonempty IDs")))?;
            if !ids.insert(id) {
                return Err(Error::bad(format!("Duplicate {name} ID")));
            }
        }
    }
    if kind == "workspace" && value["captures"].as_array().is_none_or(Vec::is_empty) {
        return Err(Error::bad("At least one capture workspace is required"));
    }
    Ok(())
}
pub fn read(directory: &Path, kind: &str) -> Result<Value> {
    if !["workspace", "library", "platform"].contains(&kind) {
        return Err(Error::bad("Unknown local archive"));
    }
    let path = directory.join(format!("{kind}.json"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LIMIT as u64) {
        return Err(Error::bad("Local archive exceeds 64 MiB"));
    }
    let mut value = match std::fs::read(path) {
        Ok(bytes) => {
            crate::storage::private_file(&directory.join(format!("{kind}.json")))?;
            if bytes.len() > LIMIT {
                return Err(Error::bad("Local archive exceeds 64 MiB"));
            }
            serde_json::from_slice(&bytes)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => defaults(kind),
        Err(e) => return Err(e.into()),
    };
    if kind == "library" && value.is_array() {
        let bookmarks = value;
        value = defaults(kind);
        value["bookmarks"] = bookmarks;
    }
    validate(kind, &value)?;
    Ok(value)
}
pub fn write(directory: &Path, kind: &str, input: Value) -> Result<Value> {
    let _lock = WRITES
        .lock()
        .map_err(|_| Error::new(500, "Archive lock failed"))?;
    let current = read(directory, kind)?;
    if input.get("expected") != Some(&current) {
        return Err(Error::new(409, "Workspace changed; reload before saving"));
    }
    let next = input
        .get("document")
        .ok_or_else(|| Error::bad("Archive document required"))?;
    validate(kind, next)?;
    let bytes = serde_json::to_vec(next)?;
    if bytes.len() > LIMIT {
        return Err(Error::bad("Local archive exceeds 64 MiB"));
    }
    atomic_write(&directory.join(format!("{kind}.json")), &bytes)?;
    Ok(next.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archives_preserve_unknown_fields_reject_conflicts_and_never_replace_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let old = read(dir.path(), "workspace").unwrap();
        let mut next = old.clone();
        next["futureField"] = json!({"preserve":true});
        write(
            dir.path(),
            "workspace",
            json!({"expected":old,"document":next}),
        )
        .unwrap();
        assert_eq!(read(dir.path(), "workspace").unwrap(), next);
        assert_eq!(
            write(
                dir.path(),
                "workspace",
                json!({"expected":old,"document":old})
            )
            .unwrap_err()
            .status,
            409
        );
        std::fs::write(dir.path().join("workspace.json"), b"broken").unwrap();
        assert!(
            write(
                dir.path(),
                "workspace",
                json!({"expected":next,"document":old})
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("workspace.json")).unwrap(),
            b"broken"
        );
        std::fs::write(dir.path().join("library.json"), b"[]").unwrap();
        assert_eq!(read(dir.path(), "library").unwrap()["version"], 2);
    }
}
