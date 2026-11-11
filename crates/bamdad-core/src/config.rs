use std::collections::HashMap;
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::kind::CoreKind;
use crate::link::{self, ProxySpec};
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_socks_port")]
    pub socks_port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub system_proxy: bool,
    #[serde(default = "default_api_addr")]
    pub api_addr: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_accent")]
    pub accent: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            socks_port: default_socks_port(),
            http_port: default_http_port(),
            system_proxy: false,
            api_addr: default_api_addr(),
            theme: default_theme(),
            accent: default_accent(),
        }
    }
}

fn default_socks_port() -> u16 {
    10808
}
fn default_http_port() -> u16 {
    10809
}
fn default_api_addr() -> String {
    "127.0.0.1".to_string()
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_accent() -> String {
    "cyan".to_string()
}

fn default_group_name() -> String {
    "Default".to_string()
}
fn custom_protocol() -> String {
    "custom".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub core: CoreKind,
    #[serde(default = "custom_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub port: u16,
    /// A complete native configuration for `core`.
    pub config: Value,
    /// Original share link, if imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Subscription group id. `None` means the pinned Default group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Human-readable subscription group name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// Subscription source URL, if this node came from a subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_url: Option<String>,
}

impl Profile {
    pub fn from_spec(spec: &ProxySpec, core: CoreKind) -> Self {
        Self {
            id: new_id(),
            name: spec.name.clone(),
            core,
            protocol: spec.protocol.as_str().to_string(),
            address: spec.address.clone(),
            port: spec.port,
            config: config_from_spec(core, spec),
            source: Some(spec.source.clone()),
            group_id: None,
            group_name: None,
            group_url: None,
        }
    }

    pub fn group_key(&self) -> String {
        self.group_id
            .clone()
            .unwrap_or_else(|| "default".to_string())
    }

    pub fn group_title(&self) -> String {
        self.group_name
            .clone()
            .unwrap_or_else(|| "Default".to_string())
    }

    pub fn is_default_group(&self) -> bool {
        self.group_id.is_none()
    }

    pub fn endpoint(&self) -> String {
        if self.protocol.eq_ignore_ascii_case("direct") {
            "local direct".to_string()
        } else if self.address.is_empty() || self.port == 0 {
            "custom config".to_string()
        } else {
            format!("{}:{}", self.address, self.port)
        }
    }

    fn ensure_fields(&mut self) {
        if self.id.is_empty() {
            self.id = new_id();
        }
        if self.protocol.is_empty() {
            self.protocol = custom_protocol();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Legacy selected profile name, kept so old stores still load.
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub selected_id: Option<String>,
    #[serde(default = "default_group_name")]
    pub default_group_name: String,
    /// Custom display order per group: group key -> profile ids.
    #[serde(default)]
    pub group_order: HashMap<String, Vec<String>>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            settings: AppSettings::default(),
            profiles: Vec::new(),
            selected: None,
            selected_id: None,
            default_group_name: default_group_name(),
            group_order: HashMap::new(),
        }
    }
}

impl Store {
    pub fn load_or_default(path: &Path) -> Self {
        let mut store = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| Self::default()),
            Err(_) => Self::default(),
        };
        store.remove_legacy_defaults();
        store.ensure_defaults();
        store
    }

    fn remove_legacy_defaults(&mut self) {
        self.profiles.retain(|profile| {
            !(profile.source.is_none()
                && profile.protocol.eq_ignore_ascii_case("direct")
                && matches!(profile.name.as_str(), "Direct · Xray" | "Direct · sing-box"))
        });
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn ensure_defaults(&mut self) {
        for profile in &mut self.profiles {
            profile.ensure_fields();
            // Link-based profiles are generated artifacts: re-render them so
            // transport/config fixes (for example XHTTP settings) reach users
            // without requiring a re-import.
            if let Some(source) = profile.source.clone() {
                if !source.trim().is_empty() {
                    if let Ok(spec) = crate::link::parse(Some(&profile.name), &source) {
                        let mut core = profile.core;
                        if matches!(spec.network.as_str(), "xhttp" | "splithttp")
                            && core == CoreKind::SingBox
                        {
                            core = CoreKind::Xray;
                        }
                        profile.core = core;
                        profile.protocol = spec.protocol.as_str().to_string();
                        profile.address = spec.address.clone();
                        profile.port = spec.port;
                        profile.config = config_from_spec(core, &spec);
                    }
                }
            }
        }

        // Migrate a legacy name-based selection.
        if self.selected_id.is_none() {
            if let Some(name) = self.selected.as_ref() {
                self.selected_id = self
                    .profiles
                    .iter()
                    .find(|profile| &profile.name == name)
                    .map(|profile| profile.id.clone());
            }
        }

        let selected_is_valid = self
            .selected_id
            .as_ref()
            .is_some_and(|id| self.profiles.iter().any(|profile| &profile.id == id));
        if !selected_is_valid {
            self.selected_id = self.profiles.first().map(|profile| profile.id.clone());
        }

        self.selected = self
            .selected_id
            .as_ref()
            .and_then(|id| self.profile_by_id(id))
            .map(|profile| profile.name.clone());
    }

    pub fn profile_by_id(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.profiles.iter().position(|profile| profile.id == id)
    }

    pub fn selected_profile(&self) -> Option<&Profile> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.profile_by_id(id))
            .or_else(|| self.profiles.first())
    }

    pub fn add_profile(&mut self, profile: Profile) {
        self.selected_id = Some(profile.id.clone());
        self.selected = Some(profile.name.clone());
        self.profiles.push(profile);
    }

    pub fn remove_profile(&mut self, id: &str) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        self.profiles.remove(index);
        if self.selected_id.as_deref() == Some(id) {
            self.selected_id = self.profiles.first().map(|profile| profile.id.clone());
        }
        true
    }

    /// Remove every node belonging to a subscription group. The pinned
    /// Default group (`None`) cannot be removed.
    pub fn remove_group(&mut self, group_id: &str) -> bool {
        if group_id == "default" || group_id.is_empty() {
            return false;
        }
        let before = self.profiles.len();
        self.profiles
            .retain(|profile| profile.group_id.as_deref() != Some(group_id));
        if self
            .selected_id
            .as_ref()
            .is_some_and(|id| self.profile_by_id(id).is_none())
        {
            self.selected_id = self.profiles.first().map(|profile| profile.id.clone());
        }
        self.profiles.len() != before
    }
}

pub fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("p{:x}{:x}", nanos, COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub fn load_store() -> Store {
    Store::load_or_default(&paths::store_path())
}

pub fn save_store(store: &Store) -> Result<()> {
    paths::ensure_layout()?;
    store.save(&paths::store_path())
}

pub fn default_profiles() -> Vec<Profile> {
    vec![
        Profile {
            id: new_id(),
            name: "Direct · Xray".to_string(),
            core: CoreKind::Xray,
            protocol: "direct".to_string(),
            address: "local".to_string(),
            port: 0,
            source: None,
            group_id: None,
            group_name: None,
            group_url: None,
            config: json!({
                "log": { "loglevel": "warning" },
                "inbounds": [
                    {
                        "tag": "socks",
                        "listen": "127.0.0.1",
                        "port": 10808,
                        "protocol": "socks",
                        "settings": { "auth": "noauth", "udp": true }
                    },
                    {
                        "tag": "http",
                        "listen": "127.0.0.1",
                        "port": 10809,
                        "protocol": "http",
                        "settings": {}
                    }
                ],
                "outbounds": [
                    { "tag": "direct", "protocol": "freedom" },
                    { "tag": "block", "protocol": "blackhole" }
                ]
            }),
        },
        Profile {
            id: new_id(),
            name: "Direct · sing-box".to_string(),
            core: CoreKind::SingBox,
            protocol: "direct".to_string(),
            address: "local".to_string(),
            port: 0,
            source: None,
            group_id: None,
            group_name: None,
            group_url: None,
            config: json!({
                "log": { "level": "warn", "timestamp": true },
                "inbounds": [
                    { "type": "socks", "tag": "socks", "listen": "127.0.0.1", "listen_port": 10808 },
                    { "type": "http", "tag": "http", "listen": "127.0.0.1", "listen_port": 10809 }
                ],
                "outbounds": [
                    { "type": "direct", "tag": "direct" },
                    { "type": "block", "tag": "block" }
                ],
                "route": { "final": "direct", "auto_detect_interface": true }
            }),
        },
    ]
}

/// Keep the old helper name working for the default Xray test profile.
pub fn default_profile() -> Profile {
    default_profiles().remove(0)
}

pub fn config_from_spec(core: CoreKind, spec: &ProxySpec) -> Value {
    match core {
        CoreKind::Xray => config_from_outbound(link::xray_outbound(spec)),
        CoreKind::SingBox => singbox_config_from_outbound(link::singbox_outbound(spec)),
    }
}

/// Wrap an Xray outbound in a complete config with local inbounds and a routing
/// table that keeps private traffic direct.
pub fn config_from_outbound(outbound: Value) -> Value {
    json!({
        "log": { "loglevel": "warning" },
        "inbounds": [
            {
                "tag": "socks",
                "listen": "127.0.0.1",
                "port": 10808,
                "protocol": "socks",
                "settings": { "auth": "noauth", "udp": true },
                "sniffing": { "enabled": true, "destOverride": ["http", "tls", "quic"] }
            },
            {
                "tag": "http",
                "listen": "127.0.0.1",
                "port": 10809,
                "protocol": "http",
                "settings": {}
            }
        ],
        "outbounds": [
            outbound,
            { "tag": "direct", "protocol": "freedom" },
            { "tag": "block", "protocol": "blackhole" }
        ],
        "routing": {
            "domainStrategy": "IPIfNonMatch",
            "rules": [
                {
                    "type": "field",
                    "ip": [
                        "127.0.0.0/8",
                        "10.0.0.0/8",
                        "172.16.0.0/12",
                        "192.168.0.0/16",
                        "169.254.0.0/16",
                        "::1/128",
                        "fc00::/7",
                        "fe80::/10"
                    ],
                    "outboundTag": "direct"
                }
            ]
        }
    })
}

/// Wrap a sing-box outbound in a complete config.
pub fn singbox_config_from_outbound(outbound: Value) -> Value {
    json!({
        "log": { "level": "warn", "timestamp": true },
        "inbounds": [
            { "type": "socks", "tag": "socks", "listen": "127.0.0.1", "listen_port": 10808 },
            { "type": "http", "tag": "http", "listen": "127.0.0.1", "listen_port": 10809 }
        ],
        "outbounds": [
            outbound,
            { "type": "direct", "tag": "direct" },
            { "type": "block", "tag": "block" }
        ],
        "route": {
            "rules": [
                {
                    "ip_cidr": [
                        "127.0.0.0/8",
                        "10.0.0.0/8",
                        "172.16.0.0/12",
                        "192.168.0.0/16",
                        "169.254.0.0/16",
                        "::1/128",
                        "fc00::/7",
                        "fe80::/10"
                    ],
                    "outbound": "direct"
                }
            ],
            "final": "proxy",
            "auto_detect_interface": true
        }
    })
}

/// Pick a free TCP port on the loopback interface.
pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn managed_socks_inbound(core: CoreKind, port: u16) -> Value {
    match core {
        CoreKind::Xray => json!({
            "tag": "socks",
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": true },
            "sniffing": { "enabled": true, "destOverride": ["http", "tls", "quic"] }
        }),
        CoreKind::SingBox => json!({
            "type": "socks",
            "tag": "socks",
            "listen": "127.0.0.1",
            "listen_port": port
        }),
    }
}

fn managed_http_inbound(core: CoreKind, port: u16) -> Value {
    match core {
        CoreKind::Xray => json!({
            "tag": "http",
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "http",
            "settings": {}
        }),
        CoreKind::SingBox => json!({
            "type": "http",
            "tag": "http",
            "listen": "127.0.0.1",
            "listen_port": port
        }),
    }
}

fn ensure_dns_route(core: CoreKind, object: &mut serde_json::Map<String, Value>) {
    match core {
        CoreKind::Xray => {
            if !object.contains_key("dns") {
                object.insert(
                    "dns".into(),
                    json!({
                        // DoH/DoT travel over the proxy's TCP stream, so DNS
                        // does not depend on the server supporting UDP.
                        "servers": ["https://1.1.1.1/dns-query", "tcp://8.8.8.8"]
                    }),
                );
            }
            if !object.contains_key("outbounds") {
                object.insert("outbounds".into(), json!([]));
            }
            if let Some(outbounds) = object.get_mut("outbounds").and_then(Value::as_array_mut) {
                if !outbounds
                    .iter()
                    .any(|outbound| outbound.get("protocol").and_then(Value::as_str) == Some("dns"))
                {
                    outbounds.push(json!({ "tag": "dns-out", "protocol": "dns" }));
                }
            }
            if !object.contains_key("routing") {
                object.insert("routing".into(), json!({ "rules": [] }));
            }
            if let Some(routing) = object.get_mut("routing").and_then(Value::as_object_mut) {
                let rules = routing
                    .entry("rules".to_string())
                    .or_insert_with(|| json!([]));
                if let Some(rules) = rules.as_array_mut() {
                    let has_dns_rule = rules.iter().any(|rule| {
                        rule.get("port").and_then(Value::as_u64) == Some(53)
                            || rule.get("outboundTag").and_then(Value::as_str) == Some("dns-out")
                    });
                    if !has_dns_rule {
                        rules.insert(
                            0,
                            json!({
                                "type": "field",
                                "port": 53,
                                "outboundTag": "dns-out"
                            }),
                        );
                    }
                }
            }
        }
        CoreKind::SingBox => {
            if !object.contains_key("dns") {
                object.insert(
                    "dns".into(),
                    json!({
                        "servers": [
                            {
                                "type": "https",
                                "tag": "dns-1",
                                "server": "1.1.1.1",
                                "path": "/dns-query"
                            },
                            { "type": "tcp", "tag": "dns-2", "server": "8.8.8.8" }
                        ]
                    }),
                );
            }

            // sing-box 1.13+ removed the DNS outbound. Use rule actions and
            // an explicit default domain resolver instead.
            let resolver_tag = object
                .get("dns")
                .and_then(|dns| dns.get("servers"))
                .and_then(Value::as_array)
                .and_then(|servers| servers.first())
                .and_then(|server| server.get("tag"))
                .and_then(Value::as_str)
                .unwrap_or("dns-1")
                .to_string();

            if !object.contains_key("route") {
                object.insert("route".into(), json!({ "rules": [] }));
            }
            if let Some(route) = object.get_mut("route").and_then(Value::as_object_mut) {
                if !route.contains_key("default_domain_resolver") {
                    route.insert("default_domain_resolver".into(), json!(resolver_tag));
                }
                let rules = route
                    .entry("rules".to_string())
                    .or_insert_with(|| json!([]));
                if let Some(rules) = rules.as_array_mut() {
                    let has_dns_rule = rules.iter().any(|rule| {
                        rule.get("action").and_then(Value::as_str) == Some("hijack-dns")
                            || rule.get("port").and_then(Value::as_u64) == Some(53)
                    });
                    if !has_dns_rule {
                        rules.insert(0, json!({ "port": 53, "action": "hijack-dns" }));
                    }
                }
            }
        }
    }
}

/// Final preparation before the config is handed to the core:
/// * align inbound ports with user settings,
/// * inject the telemetry endpoint in the core's native format.
pub fn prepare_config(
    core: CoreKind,
    config: &Value,
    settings: &AppSettings,
    api_port: u16,
) -> Result<Value> {
    let mut config = config.clone();
    let object = config.as_object_mut().ok_or_else(|| {
        crate::error::CoreError::InvalidLink("config must be a JSON object".into())
    })?;

    let mut has_socks = false;
    let mut has_http = false;
    let mut has_inbounds_array = false;

    if let Some(inbounds) = object.get_mut("inbounds").and_then(Value::as_array_mut) {
        has_inbounds_array = true;
        for inbound in inbounds.iter_mut() {
            let Some(inbound) = inbound.as_object_mut() else {
                continue;
            };
            let tag = inbound.get("tag").and_then(Value::as_str).unwrap_or("");
            let protocol = inbound
                .get("protocol")
                .or_else(|| inbound.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_mixed = protocol == "mixed";
            let port_key = if core == CoreKind::Xray {
                "port"
            } else {
                "listen_port"
            };

            if tag == "socks" || protocol == "socks" || is_mixed {
                inbound.insert(port_key.into(), json!(settings.socks_port));
                has_socks = true;
            } else if tag == "http" || protocol == "http" {
                inbound.insert(port_key.into(), json!(settings.http_port));
                has_http = true;
            }
        }

        // The Android VPN always needs a SOCKS listener on the managed port,
        // and the desktop system proxy needs the HTTP listener too. Add them
        // when a custom/profile config does not provide them.
        if !has_socks {
            inbounds.push(managed_socks_inbound(core, settings.socks_port));
        }
        if !has_http {
            inbounds.push(managed_http_inbound(core, settings.http_port));
        }
    }

    if !has_inbounds_array {
        object.insert(
            "inbounds".into(),
            json!([
                managed_socks_inbound(core, settings.socks_port),
                managed_http_inbound(core, settings.http_port)
            ]),
        );
    }

    match core {
        CoreKind::Xray => {
            object.insert(
                "api".into(),
                json!({
                    "tag": "api",
                    "listen": format!("{}:{}", settings.api_addr, api_port),
                    "services": ["StatsService"]
                }),
            );
            object.insert("stats".into(), json!({}));
            object.insert(
                "policy".into(),
                json!({
                    "system": {
                        "statsOutboundUplink": true,
                        "statsOutboundDownlink": true
                    }
                }),
            );
        }
        CoreKind::SingBox => {
            let experimental = object.entry("experimental").or_insert_with(|| json!({}));
            let experimental = experimental.as_object_mut().ok_or_else(|| {
                crate::error::CoreError::InvalidLink(
                    "sing-box experimental must be an object".into(),
                )
            })?;
            experimental.insert(
                "clash_api".into(),
                json!({
                    "external_controller": format!("{}:{}", settings.api_addr, api_port)
                }),
            );
        }
    }

    ensure_dns_route(core, object);

    // Android bans the netlink socket that sing-box's CLI network monitor
    // uses for automatic interface detection. The VPN already binds traffic
    // through the app's physical network, so disable it on Android.
    if core == CoreKind::SingBox && cfg!(target_os = "android") {
        if let Some(route) = object.get_mut("route").and_then(Value::as_object_mut) {
            route.insert("auto_detect_interface".into(), json!(false));
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_xray_telemetry() {
        let settings = AppSettings::default();
        let config =
            prepare_config(CoreKind::Xray, &default_profile().config, &settings, 12345).unwrap();
        assert_eq!(config["api"]["listen"], "127.0.0.1:12345");
        assert_eq!(config["stats"], json!({}));
        assert_eq!(
            config["policy"]["system"]["statsOutboundUplink"],
            json!(true)
        );
        assert!(config["dns"]["servers"].is_array());
        assert!(config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|outbound| outbound["protocol"] == "dns"));
    }

    #[test]
    fn injects_singbox_clash_api() {
        let settings = AppSettings::default();
        let profile = default_profiles().remove(1);
        let config = prepare_config(CoreKind::SingBox, &profile.config, &settings, 23456).unwrap();
        assert_eq!(
            config["experimental"]["clash_api"]["external_controller"],
            "127.0.0.1:23456"
        );
        assert_eq!(config["inbounds"][0]["listen_port"], json!(10808));
    }

    #[test]
    fn renders_the_same_spec_for_both_cores() {
        let spec = link::parse(
            None,
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?security=tls&sni=example.com&type=ws&path=/ws#Node",
        )
        .unwrap();

        let xray = config_from_spec(CoreKind::Xray, &spec);
        assert_eq!(xray["outbounds"][0]["protocol"], "vless");
        assert_eq!(xray["outbounds"][0]["streamSettings"]["network"], "ws");

        let singbox = config_from_spec(CoreKind::SingBox, &spec);
        assert_eq!(singbox["outbounds"][0]["type"], "vless");
        assert_eq!(singbox["outbounds"][0]["transport"]["type"], "ws");
        assert_eq!(singbox["route"]["final"], "proxy");
    }

    #[test]
    fn adds_managed_inbounds_when_missing() {
        let settings = AppSettings::default();
        let input = json!({
            "inbounds": [],
            "outbounds": [{ "tag": "proxy", "protocol": "freedom" }]
        });
        let prepared = prepare_config(CoreKind::Xray, &input, &settings, 12345).unwrap();
        let inbounds = prepared["inbounds"].as_array().unwrap();
        assert!(inbounds
            .iter()
            .any(|inbound| inbound["protocol"] == "socks"));
        assert!(inbounds.iter().any(|inbound| inbound["protocol"] == "http"));
        let socks = inbounds
            .iter()
            .find(|inbound| inbound["protocol"] == "socks")
            .unwrap();
        assert_eq!(socks["port"], json!(settings.socks_port));
    }

    #[test]
    fn migrations_fill_profile_ids_and_cores() {
        let mut store = Store {
            profiles: vec![Profile {
                id: String::new(),
                name: "Legacy".to_string(),
                core: CoreKind::default(),
                protocol: String::new(),
                address: String::new(),
                port: 0,
                config: default_profile().config,
                source: None,
                group_id: None,
                group_name: None,
                group_url: None,
            }],
            ..Default::default()
        };
        store.ensure_defaults();
        assert!(!store.profiles[0].id.is_empty());
        assert_eq!(store.profiles[0].core, CoreKind::Xray);
        assert_eq!(store.profiles[0].protocol, "custom");
    }

    #[test]
    fn regenerates_xhttp_profiles_on_load() {
        let source = "vless://11111111-2222-3333-4444-555555555555@example.com:443?type=xhttp&security=tls&sni=example.com&path=%2Fxhttp&host=example.com&mode=auto#XHTTP";
        let mut store = Store {
            profiles: vec![Profile {
                id: "p1".to_string(),
                name: "XHTTP".to_string(),
                core: CoreKind::SingBox,
                protocol: "vless".to_string(),
                address: "example.com".to_string(),
                port: 443,
                config: json!({ "outbounds": [] }),
                source: Some(source.to_string()),
                group_id: None,
                group_name: None,
                group_url: None,
            }],
            ..Default::default()
        };
        store.ensure_defaults();
        let profile = &store.profiles[0];
        assert_eq!(profile.core, CoreKind::Xray);
        assert_eq!(
            profile.config["outbounds"][0]["streamSettings"]["network"],
            json!("xhttp")
        );
        assert_eq!(
            profile.config["outbounds"][0]["streamSettings"]["xhttpSettings"]["mode"],
            json!("auto")
        );
    }

    #[test]
    fn migrates_legacy_store_selection() {
        let mut store = Store {
            selected: Some("Direct · Xray".to_string()),
            profiles: default_profiles(),
            ..Default::default()
        };
        store.ensure_defaults();
        assert!(store.selected_id.is_some());
        assert_eq!(store.selected, Some("Direct · Xray".to_string()));
    }
}
