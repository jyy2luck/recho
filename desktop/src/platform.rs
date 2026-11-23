mod certificates;
#[cfg(target_os = "macos")]
mod macos;

pub(super) use certificates::Certificates;
use recho_core::native::DesktopEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

const ARCHIVE: &str = "/platform/archive";
const ENABLED: &str = "_ProtocolEnabled";
const HTTP: &[&str] = &["HTTPEnable", "HTTPProxy", "HTTPPort"];
const HTTPS: &[&str] = &["HTTPSEnable", "HTTPSProxy", "HTTPSPort"];
const SOCKS: &[&str] = &["SOCKSEnable", "SOCKSProxy", "SOCKSPort"];
const AUTO: &[&str] = &["ProxyAutoConfigEnable", "ProxyAutoDiscoveryEnable"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProxyPreferences {
    pub http: bool,
    pub https: bool,
    pub socks: bool,
    pub auto: bool,
}

impl Default for ProxyPreferences {
    fn default() -> Self {
        Self {
            http: true,
            https: true,
            socks: false,
            auto: false,
        }
    }
}

impl ProxyPreferences {
    pub const SOCKS_SUPPORTED: bool = cfg!(target_os = "macos");

    fn validate(self) -> Result<Self, String> {
        if self.socks && !Self::SOCKS_SUPPORTED {
            return Err("此平台的系统代理不支持 SOCKS5".into());
        }
        if !self.http && !self.https && !self.socks {
            return Err("请至少选择一种系统代理协议".into());
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProxyProtocol {
    Http,
    Https,
    Socks,
}

impl ProxyProtocol {
    pub const ALL: [Self; 3] = [Self::Http, Self::Https, Self::Socks];

    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::Socks => "SOCKS5",
        }
    }

    pub fn supported(self) -> bool {
        self != Self::Socks || ProxyPreferences::SOCKS_SUPPORTED
    }

    fn selected(self, preferences: &mut ProxyPreferences) -> &mut bool {
        match self {
            Self::Http => &mut preferences.http,
            Self::Https => &mut preferences.https,
            Self::Socks => &mut preferences.socks,
        }
    }

    pub fn enabled(self, mut preferences: ProxyPreferences) -> bool {
        *self.selected(&mut preferences)
    }

    pub fn toggled(self, mut preferences: ProxyPreferences) -> ProxyPreferences {
        let selected = self.selected(&mut preferences);
        *selected = !*selected;
        preferences
    }
}

pub(super) struct ProxyOwner {
    journal: Option<Value>,
    active: bool,
    lock: Option<File>,
    preferences: ProxyPreferences,
    #[cfg(target_os = "macos")]
    authorization: macos::Authorization,
}

impl ProxyOwner {
    pub(super) fn new(_directory: &Path) -> Self {
        Self {
            journal: None,
            active: false,
            lock: None,
            preferences: ProxyPreferences::default(),
            #[cfg(target_os = "macos")]
            authorization: macos::Authorization::default(),
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.journal.as_ref().is_none_or(|v| !entries(v).is_empty())
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    pub(super) fn preferences(&self) -> ProxyPreferences {
        self.preferences
    }

    pub(super) async fn set_preferences(
        &mut self,
        engine: &DesktopEngine,
        preferences: ProxyPreferences,
    ) -> Result<(), String> {
        let preferences = preferences.validate()?;
        self.read_journal(engine).await?;
        let mut next = self.journal.clone().expect("journal loaded");
        next["preferences"] = json!(preferences);
        self.save_document(engine, next).await?;
        self.preferences = preferences;
        Ok(())
    }

    pub(super) async fn recover(&mut self, engine: &DesktopEngine) -> Result<(), String> {
        self.restore(engine).await?;
        let journal = self.journal.as_ref().expect("journal loaded");
        if let Some(preferences) = journal.get("preferences") {
            self.preferences = serde_json::from_value::<ProxyPreferences>(preferences.clone())
                .map_err(|error| format!("系统代理偏好无效：{error}"))?
                .validate()?;
        }
        Ok(())
    }

    pub(super) async fn enable_automatically(
        &mut self,
        engine: &DesktopEngine,
        port: u16,
    ) -> Result<(), String> {
        if self.preferences.auto && !self.active() && !self.pending() {
            self.set(engine, true, port).await?;
        }
        Ok(())
    }

    pub(super) async fn set(
        &mut self,
        engine: &DesktopEngine,
        enabled: bool,
        port: u16,
    ) -> Result<(), String> {
        if !enabled {
            self.restore_with_confirmation(engine, true).await?;
            if self.preferences.auto {
                self.set_preferences(
                    engine,
                    ProxyPreferences {
                        auto: false,
                        ..self.preferences
                    },
                )
                .await?;
            }
            return Ok(());
        }
        if port == 0 {
            return Err("代理尚未开始监听".into());
        }
        self.take_lock()?;
        let result = self.enable(engine, port).await;
        if !self.pending() {
            self.lock = None;
        }
        result
    }

    async fn enable(&mut self, engine: &DesktopEngine, port: u16) -> Result<(), String> {
        self.read_journal(engine).await?;
        if self.pending() {
            return Err("请先恢复上次接管的系统代理".into());
        }
        let services = read(engine, vec![]).await?;
        if services.is_empty() {
            return Err("没有可用的网络服务".into());
        }
        let ownership: Vec<_> = services
            .iter()
            .filter_map(|service| {
                let original = &service["values"];
                let applied = target(original, port, cfg!(windows), self.preferences);
                (applied != *original)
                    .then(|| json!({"id": service["id"], "original": original, "applied": applied}))
            })
            .collect();
        if ownership.is_empty() {
            return Err("系统代理已经指向当前端口，请在原配置程序中关闭".into());
        }
        let changes = ownership
            .iter()
            .map(|entry| {
                json!({"id": entry["id"], "expected": entry["original"], "desired": entry["applied"]})
            })
            .collect();
        self.save(engine, ownership).await?;
        match self.apply(engine, changes, true).await {
            Ok(()) => {
                self.active = true;
                Ok(())
            }
            Err(error) => match self.restore(engine).await {
                Ok(()) => Err(error),
                Err(restore) => Err(format!("{error}。恢复系统代理失败：{restore}")),
            },
        }
    }

    pub(super) async fn restore(&mut self, engine: &DesktopEngine) -> Result<(), String> {
        self.restore_with_confirmation(engine, false).await
    }

    async fn restore_with_confirmation(
        &mut self,
        engine: &DesktopEngine,
        interactive: bool,
    ) -> Result<(), String> {
        self.read_journal(engine).await?;
        if !self.pending() {
            self.lock = None;
            return Ok(());
        }
        self.take_lock()?;
        self.read_journal(engine).await?;
        let recorded = entries(self.journal.as_ref().expect("journal loaded"));
        let ids = recorded
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
            .collect();
        let services = read(engine, ids).await?;
        let (changes, unavailable) = restoration(recorded, &services, cfg!(windows));
        self.apply(engine, changes, interactive).await?;
        self.active = false;
        self.save(engine, unavailable).await?;
        if self.pending() {
            Err("原网络服务暂不可用，系统代理恢复记录已保留".into())
        } else {
            self.lock = None;
            Ok(())
        }
    }

    async fn read_journal(&mut self, engine: &DesktopEngine) -> Result<(), String> {
        let value = engine.request("GET", ARCHIVE, Value::Null).await?;
        let journal = &value["document"];
        if journal["version"] != 1 || !journal["entries"].is_array() {
            return Err("无法读取系统代理恢复记录".into());
        }
        self.journal = Some(journal.clone());
        Ok(())
    }

    async fn save(&mut self, engine: &DesktopEngine, entries: Vec<Value>) -> Result<(), String> {
        let mut document = self.journal.clone().expect("journal loaded");
        document["entries"] = json!(entries);
        self.save_document(engine, document).await
    }

    async fn save_document(
        &mut self,
        engine: &DesktopEngine,
        document: Value,
    ) -> Result<(), String> {
        engine
            .request(
                "PUT",
                ARCHIVE,
                json!({"expected": self.journal, "document": document}),
            )
            .await?;
        self.journal = Some(document);
        Ok(())
    }

    fn take_lock(&mut self) -> Result<(), String> {
        if self.lock.is_none() {
            let base = std::env::var_os(if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "HOME"
            })
            .ok_or("无法定位用户目录以锁定系统代理")?;
            let root = std::path::PathBuf::from(base).join(if cfg!(windows) {
                "recho"
            } else {
                "Library/Application Support/recho"
            });
            std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(root.join("system-proxy.lock"))
                .map_err(|error| error.to_string())?;
            file.try_lock()
                .map_err(|error| format!("系统代理正在由另一个 recho 窗口管理：{error}"))?;
            self.lock = Some(file);
        }
        Ok(())
    }

    async fn apply(
        &self,
        _engine: &DesktopEngine,
        changes: Vec<Value>,
        _interactive: bool,
    ) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        #[cfg(windows)]
        return _engine
            .request("POST", "/system-proxy", json!({"changes": changes}))
            .await
            .map(|_| ());
        #[cfg(target_os = "macos")]
        {
            let authorization = self.authorization.clone();
            tokio::task::spawn_blocking(move || {
                macos::apply(&changes, &authorization, _interactive)
            })
            .await
            .map_err(|error| error.to_string())?
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        Err("当前平台不支持系统代理".into())
    }
}

fn entries(journal: &Value) -> &[Value] {
    journal["entries"].as_array().map_or(&[], Vec::as_slice)
}

async fn read(_engine: &DesktopEngine, _ids: Vec<String>) -> Result<Vec<Value>, String> {
    #[cfg(windows)]
    return _engine
        .request("GET", "/system-proxy", Value::Null)
        .await?
        .get("services")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "系统代理返回了无效的网络服务".into());
    #[cfg(target_os = "macos")]
    return tokio::task::spawn_blocking(move || macos::read(_ids))
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(not(any(target_os = "macos", windows)))]
    Err("当前平台不支持系统代理".into())
}

fn target(original: &Value, port: u16, windows: bool, preferences: ProxyPreferences) -> Value {
    if windows {
        let server = [("http", preferences.http), ("https", preferences.https)]
            .into_iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(protocol, _)| format!("{protocol}=127.0.0.1:{port}"))
            .collect::<Vec<_>>()
            .join(";");
        return json!({
            "flags": 3,
            "server": server,
            "pac": original["pac"],
        });
    }
    let mut desired = original.clone();
    desired[ENABLED] = json!(1);
    for (group, enabled) in [
        (HTTP, preferences.http),
        (HTTPS, preferences.https),
        (SOCKS, preferences.socks),
    ] {
        desired[group[0]] = json!(u8::from(enabled));
        if enabled {
            desired[group[1]] = json!("127.0.0.1");
            desired[group[2]] = json!(port);
        }
    }
    for key in AUTO {
        desired[key] = json!(0);
    }
    desired
}

fn restoration(recorded: &[Value], services: &[Value], windows: bool) -> (Vec<Value>, Vec<Value>) {
    let mut changes = Vec::new();
    let mut unavailable = Vec::new();
    for entry in recorded {
        let Some(service) = services.iter().find(|v| v["id"] == entry["id"]) else {
            unavailable.push(entry.clone());
            continue;
        };
        let current = &service["values"];
        let original = &entry["original"];
        let applied = &entry["applied"];
        let desired = if windows {
            if current != applied {
                continue;
            }
            original.clone()
        } else {
            let matches = |keys: &[&str]| keys.iter().all(|k| current.get(k) == applied.get(k));
            let mut desired = current.as_object().cloned().unwrap_or_default();
            for keys in [HTTP, HTTPS, SOCKS, &AUTO[..1], &AUTO[1..], &[ENABLED][..]] {
                if !matches(keys)
                    || (keys == [ENABLED] && !(matches(HTTP) && matches(HTTPS) && matches(SOCKS)))
                {
                    continue;
                }
                for &key in keys {
                    if let Some(value) = original.get(key) {
                        desired.insert(key.to_string(), value.clone());
                    } else {
                        desired.remove(key);
                    }
                }
            }
            Value::Object(desired)
        };
        if desired != *current {
            changes.push(json!({"id": entry["id"], "expected": current, "desired": desired}));
        }
    }
    (changes, unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_owned_groups_and_exact_absence() {
        let original = json!({"_ProtocolEnabled": 0, "HTTPEnable": false, "SOCKSProxy": "prior"});
        let applied = target(&original, 7000, false, ProxyPreferences::default());
        let recorded = [json!({"id":"wifi", "original":original, "applied":applied})];
        let (changes, unavailable) =
            restoration(&recorded, &[json!({"id":"wifi", "values":applied})], false);
        assert!(unavailable.is_empty());
        assert_eq!(changes[0]["desired"], original);
        let mut changed = applied.clone();
        changed["HTTPProxy"] = json!("another-proxy");
        let (changes, _) = restoration(&recorded, &[json!({"id":"wifi", "values":changed})], false);
        let desired = &changes[0]["desired"];
        assert_eq!(desired["HTTPProxy"], "another-proxy");
        assert_eq!(desired["HTTPPort"], 7000);
        assert_eq!(desired[ENABLED], 1);
        assert_eq!(desired["SOCKSProxy"], "prior");
        assert!(desired.get("HTTPSProxy").is_none());
        assert_eq!(restoration(&recorded, &[], false).1, recorded);
    }

    #[test]
    fn restores_wininet_settings_as_one_group() {
        let original =
            json!({"flags": 13, "server": "prior:8080", "pac": "https://example.test/proxy.pac"});
        let applied = target(&original, 7000, true, ProxyPreferences::default());
        let recorded = [json!({"id":"default", "original":original, "applied":applied})];
        let (changes, _) = restoration(
            &recorded,
            &[json!({"id":"default", "values":applied})],
            true,
        );
        assert_eq!(changes[0]["desired"], original);
        let mut changed = applied;
        changed["flags"] = json!(1);
        assert!(
            restoration(
                &recorded,
                &[json!({"id":"default", "values":changed})],
                true
            )
            .0
            .is_empty()
        );
    }

    #[test]
    fn protocol_preferences_keep_unselected_addresses_and_restore_socks() {
        let preferences = ProxyPreferences {
            http: false,
            https: false,
            socks: true,
            auto: true,
        };
        let original = json!({ENABLED:1,"HTTPEnable":1,"HTTPProxy":"old","HTTPPort":9000,"SOCKSEnable":0,"SOCKSProxy":"old-socks"});
        let applied = target(&original, 7000, false, preferences);
        assert_eq!(applied["HTTPProxy"], "old");
        assert_eq!(applied["HTTPEnable"], 0);
        assert_eq!(applied["SOCKSEnable"], 1);
        assert_eq!(applied["SOCKSProxy"], "127.0.0.1");
        let recorded = [json!({"id":"wifi", "original":original, "applied":applied})];
        let (changes, _) =
            restoration(&recorded, &[json!({"id":"wifi", "values": applied})], false);
        assert_eq!(changes[0]["desired"], original);
    }

    #[tokio::test]
    async fn preferences_persist_without_owning_the_os_proxy() {
        let path = std::env::temp_dir().join(format!("recho-proxy-test-{}", uuid::Uuid::new_v4()));
        let engine = DesktopEngine::start(path.clone(), Some(0)).await.unwrap();
        let mut owner = ProxyOwner::new(&path);
        owner.recover(&engine).await.unwrap();
        assert!(!owner.pending());
        let preferences = ProxyPreferences {
            https: false,
            auto: true,
            ..ProxyPreferences::default()
        };
        owner.set_preferences(&engine, preferences).await.unwrap();
        let mut reopened = ProxyOwner::new(&path);
        reopened.recover(&engine).await.unwrap();
        assert_eq!(reopened.preferences(), preferences);
        assert!(!reopened.active());
        assert!(!reopened.pending());
        assert!(
            reopened
                .set_preferences(
                    &engine,
                    ProxyPreferences {
                        http: false,
                        ..preferences
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(reopened.preferences(), preferences);
        engine.stop().await;
        std::fs::remove_dir_all(path).unwrap();
    }
}
