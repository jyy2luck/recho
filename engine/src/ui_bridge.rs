//! Queue workspace edits for the GUI's single owner of its persistent model.
use crate::wire::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;
struct Operation {
    name: String,
    args: Value,
    sender: oneshot::Sender<Result<Value>>,
}
pub struct UIBridge {
    pending: Mutex<BTreeMap<String, Operation>>,
}
struct Guard {
    bridge: Arc<UIBridge>,
    id: String,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.bridge.pending.lock().unwrap().remove(&self.id);
    }
}
impl UIBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(BTreeMap::new()),
        })
    }
    pub fn pending(&self) -> Value {
        json!({"operations":self.pending.lock().unwrap().iter().map(|(id,op)|json!({"id":id,"name":op.name,"arguments":op.args})).collect::<Vec<_>>()})
    }
    pub fn complete(&self, id: &str, value: Value) -> Result<Value> {
        if value.get("result").is_none() && value.get("error").is_none() {
            return Err(Error::bad("UI operation needs result or error"));
        }
        let item = self
            .pending
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| Error::new(404, "Workspace operation expired or completed"))?;
        let result = if let Some(error) = value["error"].as_str() {
            Err(Error::bad(error))
        } else {
            Ok(value["result"].clone())
        };
        item.sender
            .send(result)
            .map_err(|_| Error::new(404, "Workspace caller disconnected"))?;
        Ok(json!({"ok":true}))
    }
    pub async fn request(self: &Arc<Self>, name: &str, args: Value) -> Result<Value> {
        if !args.is_object() {
            return Err(Error::bad("Workspace arguments must be an object"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.len() >= 100 {
                return Err(Error::new(503, "Too many pending workspace operations"));
            }
            pending.insert(
                id.clone(),
                Operation {
                    name: name.into(),
                    args,
                    sender,
                },
            );
        }
        let _guard = Guard {
            bridge: self.clone(),
            id,
        };
        tokio::time::timeout(Duration::from_secs(10), receiver)
            .await
            .map_err(|_| Error::new(504, "Workspace did not respond; keep the Recho window open"))?
            .map_err(|_| Error::new(503, "Workspace closed before the operation completed"))?
    }
    pub fn close(&self) {
        self.pending.lock().unwrap().clear();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn operations_resolve_once_and_cancel_without_leaking() {
        let bridge = UIBridge::new();
        let clone = bridge.clone();
        let request =
            tokio::spawn(async move { clone.request("environment_list", json!({})).await });
        tokio::task::yield_now().await;
        let pending = bridge.pending();
        let id = pending["operations"][0]["id"].as_str().unwrap();
        bridge.complete(id, json!({"result":["one"]})).unwrap();
        assert_eq!(request.await.unwrap().unwrap(), json!(["one"]));
        assert!(bridge.complete(id, json!({"result":[]})).is_err());
        let clone = bridge.clone();
        let cancelled =
            tokio::spawn(async move { clone.request("environment_list", json!({})).await });
        tokio::task::yield_now().await;
        cancelled.abort();
        let _ = cancelled.await;
        assert!(
            bridge.pending()["operations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
