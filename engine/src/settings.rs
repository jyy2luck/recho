use crate::{
    rules::wildcard,
    storage::atomic_write,
    wire::{Error, Result, parse_url},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

fn yes() -> bool {
    true
}
fn default_group() -> String {
    "default".into()
}
fn default_python() -> String {
    crate::python::default_executable().into()
}
fn original() -> String {
    "original".into()
}
fn default_listen() -> String {
    "127.0.0.1".into()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub mode: String,
    #[serde(default)]
    pub hide_bypassed: bool,
    #[serde(default)]
    pub patterns: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Upstream {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub patterns: Vec<String>,
}
impl Upstream {
    pub fn authorization(&self) -> Option<String> {
        let username = self.username.as_ref()?;
        let value = format!("{}:{}", username, self.password.as_deref().unwrap_or(""));
        const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for part in value.as_bytes().chunks(3) {
            let n = (u32::from(part[0]) << 16)
                | (u32::from(*part.get(1).unwrap_or(&0)) << 8)
                | u32::from(*part.get(2).unwrap_or(&0));
            encoded.push(DIGITS[((n >> 18) & 63) as usize] as char);
            encoded.push(DIGITS[((n >> 12) & 63) as usize] as char);
            encoded.push(if part.len() > 1 {
                DIGITS[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            encoded.push(if part.len() > 2 {
                DIGITS[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        Some(format!("Basic {encoded}"))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mirror {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub host: String,
    pub target: String,
    #[serde(default = "original")]
    pub host_policy: String,
    #[serde(default = "original")]
    pub sni_policy: String,
    #[serde(default = "original")]
    pub cn_policy: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayMatcher {
    pub r#type: String,
    pub pattern: String,
    #[serde(default)]
    pub ports: String,
    #[serde(default = "any_method")]
    pub method: String,
}
fn any_method() -> String {
    "*".into()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gateway {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub layer: String,
    pub mode: String,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub show_blocked: bool,
    #[serde(default)]
    pub matchers: Vec<GatewayMatcher>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReverseProxy {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_listen")]
    pub listen_host: String,
    pub port: u16,
    pub target: String,
    #[serde(default)]
    pub preserve_host: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub mcp_remote_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<u16>,
    #[serde(rename = "interceptTLS")]
    pub intercept_tls: bool,
    #[serde(rename = "interceptTLSConfigured")]
    pub intercept_tls_configured: bool,
    pub rule_folders: BTreeMap<String, Vec<String>>,
    pub rewrite_enabled: bool,
    pub script_enabled: bool,
    pub breakpoint_enabled: bool,
    pub incognito: bool,
    pub retention_days: u32,
    pub capture_group: String,
    pub python_executable: String,
    pub python_timeout_ms: u64,
    pub ssl_profiles: Vec<Profile>,
    #[serde(rename = "activeSSLProfile", alias = "activeSslProfile")]
    pub active_ssl_profile: Option<String>,
    pub access_profiles: Vec<Profile>,
    pub active_access_profile: Option<String>,
    pub access_enabled: bool,
    pub upstream_profiles: Vec<Upstream>,
    pub active_upstream_profile: Option<String>,
    pub upstream_enabled: bool,
    pub mirrors: Vec<Mirror>,
    pub mirror_enabled: bool,
    pub gateways: Vec<Gateway>,
    pub gateway_enabled: bool,
    pub reverse_proxies: Vec<ReverseProxy>,
    pub reverse_enabled: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mcp_remote_enabled: false,
            proxy_port: None,
            intercept_tls: false,
            intercept_tls_configured: false,
            rule_folders: BTreeMap::new(),
            rewrite_enabled: true,
            script_enabled: true,
            breakpoint_enabled: true,
            incognito: false,
            retention_days: 0,
            capture_group: default_group(),
            python_executable: default_python(),
            python_timeout_ms: 5000,
            ssl_profiles: vec![],
            active_ssl_profile: None,
            access_profiles: vec![],
            active_access_profile: None,
            access_enabled: false,
            upstream_profiles: vec![],
            active_upstream_profile: None,
            upstream_enabled: false,
            mirrors: vec![],
            mirror_enabled: false,
            gateways: vec![],
            gateway_enabled: false,
            reverse_proxies: vec![],
            reverse_enabled: false,
        }
    }
}
fn check_ids<'a>(ids: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        if id.is_empty() || !seen.insert(id) {
            return Err(Error::bad("Configuration IDs must be nonempty and unique"));
        }
    }
    Ok(())
}
fn patterns_valid(patterns: &[String]) -> Result<()> {
    if patterns
        .iter()
        .any(|p| p.is_empty() || p.chars().any(char::is_whitespace))
    {
        return Err(Error::bad("Patterns must be nonempty without whitespace"));
    }
    Ok(())
}
fn active_valid<'a>(active: &Option<String>, ids: impl Iterator<Item = &'a str>) -> Result<()> {
    if active
        .as_ref()
        .is_some_and(|active| !ids.into_iter().any(|id| id == active))
    {
        return Err(Error::bad("Active profile does not exist"));
    }
    Ok(())
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.capture_group.is_empty() || self.capture_group.len() > 256 {
            return Err(Error::bad("captureGroup must have 1 to 256 bytes"));
        }
        if self.python_timeout_ms == 0 || self.python_timeout_ms > 300000 {
            return Err(Error::bad("pythonTimeoutMs must be from 1 to 300000"));
        }
        if self.python_executable.is_empty() {
            return Err(Error::bad("pythonExecutable must not be empty"));
        }
        for (profiles, modes) in [
            (&self.ssl_profiles, ["intercept", "bypass"]),
            (&self.access_profiles, ["allow", "deny"]),
        ] {
            check_ids(profiles.iter().map(|p| p.id.as_str()))?;
            for p in profiles {
                if !modes.contains(&p.mode.as_str()) {
                    return Err(Error::bad("Invalid profile mode"));
                }
                patterns_valid(&p.patterns)?;
            }
        }
        active_valid(
            &self.active_ssl_profile,
            self.ssl_profiles.iter().map(|p| p.id.as_str()),
        )?;
        active_valid(
            &self.active_access_profile,
            self.access_profiles.iter().map(|p| p.id.as_str()),
        )?;
        check_ids(self.upstream_profiles.iter().map(|p| p.id.as_str()))?;
        active_valid(
            &self.active_upstream_profile,
            self.upstream_profiles.iter().map(|p| p.id.as_str()),
        )?;
        for p in &self.upstream_profiles {
            if p.username
                .as_ref()
                .is_some_and(|u| u.contains(':') || u.chars().any(char::is_control))
            {
                return Err(Error::bad(
                    "Upstream username cannot contain colon or control characters",
                ));
            }
            if p.port == 0 || !["allow", "bypass"].contains(&p.mode.as_str()) {
                return Err(Error::bad("Invalid upstream port or mode"));
            }
            parse_url(&format!("http://{}:{}", p.host, p.port))?;
            patterns_valid(&p.patterns)?;
        }
        check_ids(self.mirrors.iter().map(|p| p.id.as_str()))?;
        for p in &self.mirrors {
            patterns_valid(&[p.host.clone()])?;
            parse_url(&format!("https://{}", p.target))?;
            for policy in [&p.host_policy, &p.sni_policy, &p.cn_policy] {
                if !["preserve", "original", "target"].contains(&policy.as_str()) {
                    return Err(Error::bad("Invalid mirror policy"));
                }
            }
        }
        check_ids(self.gateways.iter().map(|p| p.id.as_str()))?;
        for p in &self.gateways {
            if !["l4", "l7"].contains(&p.layer.as_str())
                || ![
                    "allow",
                    "bypass",
                    "blockRequest",
                    "blockResponse",
                    "suspendRequest",
                    "suspendResponse",
                ]
                .contains(&p.mode.as_str())
            {
                return Err(Error::bad("Invalid gateway layer or mode"));
            }
            if p.layer == "l4" && !["allow", "bypass", "blockRequest"].contains(&p.mode.as_str()) {
                return Err(Error::bad("L4 supports allow, bypass and blockRequest"));
            }
            patterns_valid(&p.patterns)?;
            for matcher in &p.matchers {
                if !["domain", "ip", "http", "websocket"].contains(&matcher.r#type.as_str())
                    || (p.layer == "l4" && !["domain", "ip"].contains(&matcher.r#type.as_str()))
                {
                    return Err(Error::bad("Invalid gateway matcher type for layer"));
                }
                patterns_valid(&[matcher.pattern.clone()])?;
                ports_match(&matcher.ports, 80)?;
                if !["*", "ANY"].contains(&matcher.method.as_str()) {
                    crate::wire::method(&matcher.method)?;
                }
            }
        }
        check_ids(self.reverse_proxies.iter().map(|p| p.id.as_str()))?;
        let mut ports = HashSet::new();
        for p in &self.reverse_proxies {
            parse_url(&p.target)?;
            if p.listen_host.parse::<std::net::IpAddr>().is_err() || p.port == 0 {
                return Err(Error::bad(
                    "Reverse proxy requires a valid listen IP and nonzero port",
                ));
            }
            if p.enabled && !ports.insert((&p.listen_host, p.port)) {
                return Err(Error::bad("Duplicate reverse proxy listening address"));
            }
        }
        Ok(())
    }
    pub fn intercept(&self, host: &str, port: u16) -> bool {
        let Some(p) = self
            .ssl_profiles
            .iter()
            .find(|p| Some(&p.id) == self.active_ssl_profile.as_ref())
        else {
            return true;
        };
        let matched = host_matches(&p.patterns, host, port);
        if p.mode == "intercept" {
            matched
        } else {
            !matched
        }
    }
    pub fn hide_bypassed_ssl(&self, host: &str, port: u16) -> bool {
        self.ssl_profiles
            .iter()
            .find(|p| Some(&p.id) == self.active_ssl_profile.as_ref())
            .is_some_and(|p| p.hide_bypassed)
            && !self.intercept(host, port)
    }
    pub fn permits(&self, address: std::net::IpAddr) -> bool {
        if !self.access_enabled {
            return true;
        }
        let Some(p) = self
            .access_profiles
            .iter()
            .find(|p| Some(&p.id) == self.active_access_profile.as_ref())
        else {
            return true;
        };
        let matched = p
            .patterns
            .iter()
            .any(|pattern| ip_matches(pattern, address));
        if p.mode == "allow" { matched } else { !matched }
    }
    pub fn upstream(&self, host: &str, port: u16) -> Option<Upstream> {
        if !self.upstream_enabled {
            return None;
        };
        self.upstream_profiles
            .iter()
            .find(|p| {
                Some(&p.id) == self.active_upstream_profile.as_ref()
                    && (host_matches(&p.patterns, host, port) == (p.mode == "allow"))
            })
            .cloned()
    }
    pub fn mirror(&self, host: &str, port: u16, scheme: &str) -> Option<Mirror> {
        if !self.mirror_enabled {
            return None;
        };
        self.mirrors
            .iter()
            .find(|p| {
                let explicit_port = if p.host.starts_with('[') {
                    p.host.contains("]:")
                } else {
                    p.host.matches(':').count() == 1
                };
                p.enabled
                    && (explicit_port || port == if scheme == "https" { 443 } else { 80 })
                    && host_matches(&[p.host.clone()], host, port)
            })
            .cloned()
    }
}
pub fn host_matches(patterns: &[String], host: &str, port: u16) -> bool {
    patterns.iter().any(|p| {
        let p = p.to_ascii_lowercase();
        let host = host.to_ascii_lowercase();
        if p.starts_with('.') {
            host.ends_with(&p) || host == p[1..]
        } else {
            wildcard(&p, &host)
                || wildcard(&p, &format!("{host}:{port}"))
                || host.parse().is_ok_and(|ip| ip_matches(&p, ip))
        }
    })
}
pub fn ip_matches(pattern: &str, address: std::net::IpAddr) -> bool {
    if let Some((base, prefix)) = pattern.split_once('/') {
        let Ok(base) = base.parse::<std::net::IpAddr>() else {
            return false;
        };
        let Ok(prefix) = prefix.parse::<u32>() else {
            return false;
        };
        match (base, address) {
            (std::net::IpAddr::V4(base), std::net::IpAddr::V4(address)) if prefix <= 32 => {
                let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
                u32::from(base) & mask == u32::from(address) & mask
            }
            (std::net::IpAddr::V6(base), std::net::IpAddr::V6(address)) if prefix <= 128 => {
                let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
                u128::from(base) & mask == u128::from(address) & mask
            }
            _ => false,
        }
    } else {
        wildcard(pattern, &address.to_string())
    }
}
#[derive(Default, Clone)]
pub struct GatewayDecision {
    pub matched: bool,
    pub bypass: bool,
    pub block_request: bool,
    pub block_response: bool,
    pub suspend_request: bool,
    pub suspend_response: bool,
    pub show_blocked: bool,
}
impl Settings {
    pub fn gateway_with(
        &self,
        url: &url::Url,
        l4_only: bool,
        method: &str,
        websocket: bool,
        addresses: &[std::net::IpAddr],
    ) -> GatewayDecision {
        if !self.gateway_enabled {
            return GatewayDecision::default();
        }
        let host = url.host_str().unwrap_or("");
        let port = url.port_or_known_default().unwrap_or(80);
        let rules: Vec<_> = self
            .gateways
            .iter()
            .filter(|r| r.enabled && (!l4_only || r.layer == "l4"))
            .collect();
        let matched = |r: &&Gateway| {
            host_matches(&r.patterns, host, port)
                || r.patterns
                    .iter()
                    .any(|pattern| addresses.iter().any(|ip| ip_matches(pattern, *ip)))
                || (r.layer == "l7" && r.patterns.iter().any(|p| wildcard(p, url.as_str())))
                || r.matchers.iter().any(|m| {
                    if !ports_match(&m.ports, port).unwrap_or(false) {
                        return false;
                    }
                    match m.r#type.as_str() {
                        "domain" => host_matches(&[m.pattern.clone()], host, port),
                        "ip" => {
                            host.parse().is_ok_and(|ip| ip_matches(&m.pattern, ip))
                                || addresses.iter().any(|ip| ip_matches(&m.pattern, *ip))
                        }
                        "http" if !websocket => {
                            (["*", "ANY"].contains(&m.method.as_str())
                                || m.method.eq_ignore_ascii_case(method))
                                && wildcard(&m.pattern, url.as_str())
                        }
                        "websocket" if websocket => {
                            let ws = url
                                .as_str()
                                .replacen("https://", "wss://", 1)
                                .replacen("http://", "ws://", 1);
                            wildcard(&m.pattern, &ws) || wildcard(&m.pattern, url.as_str())
                        }
                        _ => false,
                    }
                })
        };
        if rules.iter().filter(|r| r.mode == "allow").any(matched) {
            return GatewayDecision {
                matched: true,
                ..Default::default()
            };
        }
        if rules.iter().filter(|r| r.mode == "bypass").any(matched) {
            return GatewayDecision {
                bypass: true,
                matched: true,
                ..Default::default()
            };
        }
        let mut result = GatewayDecision {
            bypass: rules.iter().any(|r| r.mode == "allow"),
            matched: rules.iter().any(|r| r.mode == "allow"),
            ..Default::default()
        };
        for rule in rules.iter().filter(|r| matched(r)) {
            result.matched = true;
            match rule.mode.as_str() {
                "blockRequest" => {
                    result.block_request = true;
                    result.show_blocked |= rule.show_blocked;
                }
                "blockResponse" => {
                    result.block_response = true;
                    result.show_blocked |= rule.show_blocked;
                }
                "suspendRequest" => result.suspend_request = true,
                "suspendResponse" => result.suspend_response = true,
                _ => {}
            }
        }
        result
    }
    pub fn needs_gateway_addresses(&self) -> bool {
        self.gateway_enabled
            && self.gateways.iter().filter(|r| r.enabled).any(|r| {
                r.matchers.iter().any(|m| m.r#type == "ip")
                    || r.patterns.iter().any(|p| {
                        p.split_once('/')
                            .is_some_and(|(ip, _)| ip.parse::<std::net::IpAddr>().is_ok())
                    })
            })
    }
}
fn ports_match(expression: &str, port: u16) -> Result<bool> {
    if expression.trim().is_empty() || expression.trim() == "*" {
        return Ok(true);
    }
    let mut matched = false;
    for part in expression.split(',').map(str::trim) {
        let (start, end) = if let Some((start, end)) = part.split_once('-') {
            (
                start
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| Error::bad("Invalid port range"))?,
                end.trim()
                    .parse::<u16>()
                    .map_err(|_| Error::bad("Invalid port range"))?,
            )
        } else {
            let port = part
                .parse::<u16>()
                .map_err(|_| Error::bad("Invalid port"))?;
            (port, port)
        };
        if start == 0 || start > end {
            return Err(Error::bad("Invalid port range"));
        }
        matched |= (start..=end).contains(&port);
    }
    Ok(matched)
}

pub struct SettingsStore {
    path: PathBuf,
    data: Mutex<Settings>,
    error: Option<String>,
}
impl SettingsStore {
    pub fn open(directory: &Path) -> Self {
        let path = directory.join("settings.json");
        let loaded = (|| -> Result<Settings> {
            let value: Settings = serde_json::from_slice(&fs::read(&path)?)?;
            value.validate()?;
            Ok(value)
        })();
        let (data, error) = match loaded {
            Ok(value) => (value, None),
            Err(_) if !path.exists() => (Settings::default(), None),
            Err(error) => (
                Settings::default(),
                Some(format!(
                    "Cannot load settings: {error}; original file preserved"
                )),
            ),
        };
        Self {
            path,
            data: Mutex::new(data),
            error,
        }
    }
    pub fn snapshot(&self) -> Settings {
        self.data.lock().unwrap().clone()
    }
    pub fn get(&self) -> Value {
        let mut value = serde_json::to_value(self.snapshot()).unwrap();
        value["error"] = json!(self.error);
        value
    }
    pub fn updated(&self, patch: Value) -> Result<Settings> {
        if let Some(error) = &self.error {
            return Err(Error::new(409, error));
        }
        let object = patch
            .as_object()
            .ok_or_else(|| Error::bad("settings must be an object"))?;
        let mut value = serde_json::to_value(self.snapshot())?;
        if let Some(expected) = object.get("_expected") {
            let expected = expected
                .as_object()
                .ok_or_else(|| Error::bad("_expected must be an object"))?;
            for (key, old) in expected {
                if value.get(key).unwrap_or(&Value::Null) != old {
                    return Err(Error::new(
                        409,
                        format!("Setting {key} changed since it was loaded; reload before saving"),
                    ));
                }
            }
        }
        for (key, item) in object {
            if key != "_expected" {
                value[key] = item.clone();
            }
        }
        let next: Settings = serde_json::from_value(value)?;
        next.validate()?;
        Ok(next)
    }
    pub fn save(&self, next: Settings) -> Result<Value> {
        atomic_write(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        *self.data.lock().unwrap() = next;
        Ok(self.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_profiles_persist_and_stale_updates_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let next=store.updated(json!({"sslProfiles":[{"id":"local","name":"local","mode":"intercept","patterns":["*.test"],"hideBypassed":true}],"activeSSLProfile":"local","incognito":true})).unwrap();
        store.save(next).unwrap();
        let reloaded = SettingsStore::open(dir.path());
        assert_eq!(reloaded.get(), store.get());
        assert!(reloaded.snapshot().intercept("api.test", 443));
        assert!(!reloaded.snapshot().intercept("outside.example", 443));
        assert!(
            reloaded
                .snapshot()
                .hide_bypassed_ssl("outside.example", 443)
        );
        assert_eq!(
            store
                .updated(json!({"_expected":{"incognito":false},"incognito":false}))
                .err()
                .unwrap()
                .status,
            409
        );
        assert!(
            store
                .updated(json!({"activeSSLProfile":"missing"}))
                .is_err()
        );
        assert!(store.updated(json!({"incognito":"yes"})).is_err());
        assert_eq!(SettingsStore::open(dir.path()).get(), store.get());
    }
    #[test]
    fn structured_gateway_ports_dns_ip_methods_and_priority() {
        let settings:Settings=serde_json::from_value(json!({"gatewayEnabled":true,"gateways":[
            {"id":"block","name":"block","layer":"l7","mode":"blockRequest","matchers":[{"type":"http","pattern":"http://service.test:8080/*","method":"POST","ports":"8080-8082,9090"}]},
            {"id":"ip","name":"ip","layer":"l4","mode":"bypass","matchers":[{"type":"ip","pattern":"10.0.0.0/8","ports":"8080"}]}
        ]})).unwrap();
        settings.validate().unwrap();
        let url = parse_url("http://service.test:8080/foo").unwrap();
        assert!(
            settings
                .gateway_with(&url, false, "POST", false, &[])
                .block_request
        );
        assert!(
            !settings
                .gateway_with(&url, false, "GET", false, &[])
                .block_request
        );
        let resolved = ["10.1.2.3".parse().unwrap()];
        let policy = settings.gateway_with(&url, false, "POST", false, &resolved);
        assert!(policy.bypass);
        assert!(!policy.block_request);
        assert!(ip_matches("2001:db8::/32", "2001:db8::7".parse().unwrap()));
        assert!(!ip_matches("2001:db8::/32", "2002::7".parse().unwrap()));
        assert!(ports_match("8000-7000", 8080).is_err());
        assert!(ports_match("65536", 80).is_err());
    }
    #[test]
    fn mirror_default_ports_and_folder_metadata_survive_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        let next = store.updated(json!({"mirrorEnabled":true,"mirrors":[
            {"id":"default","name":"default","folder":"Local","host":"service.test","target":"127.0.0.1:8080"},
            {"id":"custom","name":"custom","host":"service.test:8443","target":"127.0.0.1:8081"}
        ],"gateways":[{"id":"g","name":"g","folder":"Local","layer":"l7","mode":"bypass","patterns":["*.test"]}]})).unwrap();
        store.save(next).unwrap();
        let settings = SettingsStore::open(dir.path()).snapshot();
        assert_eq!(
            settings.mirror("service.test", 80, "http").unwrap().id,
            "default"
        );
        assert_eq!(
            settings.mirror("service.test", 443, "https").unwrap().id,
            "default"
        );
        assert_eq!(
            settings.mirror("service.test", 8443, "https").unwrap().id,
            "custom"
        );
        assert!(settings.mirror("service.test", 8080, "http").is_none());
        assert!(settings.mirror("service.test", 443, "http").is_none());
        assert_eq!(settings.mirrors[0].folder.as_deref(), Some("Local"));
        assert_eq!(settings.gateways[0].folder.as_deref(), Some("Local"));
    }
}
