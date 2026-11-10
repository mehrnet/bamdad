//! Protocol-agnostic share-link parsing and native config generation.
//!
//! A link is parsed once into [`ProxySpec`]. The same spec is then rendered for
//! Xray or sing-box, so every protocol gets the same profile, latency and UI
//! treatment.

use std::collections::HashMap;

use base64::Engine;
use percent_encoding::percent_decode_str;
use serde_json::{json, Value};
use url::Url;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vless => "vless",
            Self::Vmess => "vmess",
            Self::Trojan => "trojan",
            Self::Shadowsocks => "shadowsocks",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "vless" => Some(Self::Vless),
            "vmess" => Some(Self::Vmess),
            "trojan" => Some(Self::Trojan),
            "ss" | "shadowsocks" => Some(Self::Shadowsocks),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Security {
    #[default]
    None,
    Tls,
    Reality,
}

/// The normalized result of parsing a `vless://`, `vmess://`, `trojan://` or
/// `ss://` link. Transport and TLS variants are represented as fields so both
/// supported cores can render the same endpoint.
#[derive(Debug, Clone, Default)]
pub struct ProxySpec {
    pub name: String,
    pub source: String,
    pub protocol: Protocol,
    pub address: String,
    pub port: u16,
    /// UUID (VLESS/VMess), password (Trojan/Shadowsocks).
    pub secret: String,
    /// Shadowsocks method.
    pub method: String,
    /// VMess `alterId`.
    pub alter_id: u32,
    /// VLESS `flow`, e.g. `xtls-rprx-vision`.
    pub flow: String,
    /// VLESS `encryption` or VMess security method.
    pub encryption: String,
    pub security: Security,
    pub sni: String,
    pub fingerprint: String,
    pub public_key: String,
    pub short_id: String,
    pub spider_x: String,
    pub allow_insecure: bool,
    pub alpn: Vec<String>,
    /// `tcp`, `ws`, `grpc`, `h2`/`http`.
    pub network: String,
    pub path: String,
    pub host_header: String,
    pub service_name: String,
    pub header_type: String,
    /// XHTTP mode (`auto`, `packet-up`, `stream-up`).
    pub xhttp_mode: String,
    /// XHTTP `extra` object from the share link.
    pub xhttp_extra: Option<Value>,
    /// WebSocket early-data length (`ed=`).
    pub ws_max_early_data: u32,
    /// WebSocket early-data header name (`eh=`).
    pub ws_early_data_header: String,
    /// gRPC `mode=multi`.
    pub grpc_multi_mode: bool,
    /// gRPC authority/host override.
    pub grpc_authority: String,
}

impl ProxySpec {
    pub fn endpoint(&self) -> String {
        if self.address.is_empty() {
            "custom config".to_string()
        } else {
            format!("{}:{}", self.address, self.port)
        }
    }
}

pub fn parse(name_hint: Option<&str>, link: &str) -> Result<ProxySpec> {
    let link = link.trim();
    let (scheme, rest) = link
        .split_once("://")
        .ok_or_else(|| CoreError::InvalidLink("missing scheme".into()))?;

    let mut spec = match scheme.to_ascii_lowercase().as_str() {
        "vless" => parse_vless(rest)?,
        "trojan" => parse_trojan(rest)?,
        "vmess" => parse_vmess(rest)?,
        "ss" => parse_shadowsocks(rest)?,
        other => return Err(CoreError::UnsupportedScheme(other.to_string())),
    };

    spec.source = link.to_string();
    if spec.name.trim().is_empty() {
        spec.name = name_hint
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{} · {}", spec.protocol.as_str(), spec.endpoint()));
    }
    if spec.sni.is_empty() {
        spec.sni = spec.address.clone();
    }
    if spec.host_header.is_empty() {
        spec.host_header = spec.sni.clone();
    }
    if spec.network.is_empty() {
        spec.network = "tcp".to_string();
    }
    Ok(spec)
}

fn decode_percent(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

fn query_map(url: &Url) -> HashMap<String, String> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn security_from(params: &HashMap<String, String>) -> Security {
    match params
        .get("security")
        .or_else(|| params.get("tls"))
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("reality") => Security::Reality,
        Some("tls") | Some("xtls") => Security::Tls,
        _ => Security::None,
    }
}

fn fill_common(spec: &mut ProxySpec, params: &HashMap<String, String>, host: &str) {
    spec.security = security_from(params);
    spec.network = params
        .get("type")
        .or_else(|| params.get("net"))
        .cloned()
        .unwrap_or_else(|| "tcp".to_string());
    spec.sni = params
        .get("sni")
        .or_else(|| params.get("peer"))
        .cloned()
        .unwrap_or_else(|| host.to_string());
    spec.fingerprint = params
        .get("fp")
        .or_else(|| params.get("fingerprint"))
        .cloned()
        .unwrap_or_else(|| "chrome".to_string());
    spec.public_key = params.get("pbk").cloned().unwrap_or_default();
    spec.short_id = params.get("sid").cloned().unwrap_or_default();
    spec.spider_x = params.get("spx").cloned().unwrap_or_default();
    spec.allow_insecure = params
        .get("allowInsecure")
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    spec.path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    spec.host_header = params
        .get("host")
        .cloned()
        .unwrap_or_else(|| spec.sni.clone());
    spec.service_name = params
        .get("serviceName")
        .or_else(|| params.get("service_name"))
        .cloned()
        .unwrap_or_default();
    spec.header_type = params.get("headerType").cloned().unwrap_or_default();
    spec.xhttp_mode = params.get("mode").cloned().unwrap_or_default();
    spec.xhttp_extra = params
        .get("extra")
        .and_then(|value| serde_json::from_str(&decode_percent(value)).ok());
    spec.ws_max_early_data = params
        .get("ed")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    spec.ws_early_data_header = params.get("eh").cloned().unwrap_or_default();
    spec.grpc_multi_mode = params.get("mode").map(String::as_str) == Some("multi");
    spec.grpc_authority = params.get("authority").cloned().unwrap_or_default();
    if spec.network == "raw" {
        spec.network = "tcp".to_string();
    }
    spec.alpn = params
        .get("alpn")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
}

fn parse_vless(rest: &str) -> Result<ProxySpec> {
    let url = Url::parse(&format!("vless://{rest}"))
        .map_err(|e| CoreError::InvalidLink(format!("vless: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| CoreError::InvalidLink("vless: missing host".into()))?
        .to_string();
    let port = url
        .port()
        .ok_or_else(|| CoreError::InvalidLink("vless: missing port".into()))?;
    let params = query_map(&url);

    let mut spec = ProxySpec {
        protocol: Protocol::Vless,
        address: host.clone(),
        port,
        secret: decode_percent(url.username()),
        encryption: params
            .get("encryption")
            .cloned()
            .unwrap_or_else(|| "none".to_string()),
        flow: params.get("flow").cloned().unwrap_or_default(),
        ..Default::default()
    };
    fill_common(&mut spec, &params, &host);
    spec.name = url.fragment().map(decode_percent).unwrap_or_default();
    Ok(spec)
}

fn parse_trojan(rest: &str) -> Result<ProxySpec> {
    let url = Url::parse(&format!("trojan://{rest}"))
        .map_err(|e| CoreError::InvalidLink(format!("trojan: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| CoreError::InvalidLink("trojan: missing host".into()))?
        .to_string();
    let params = query_map(&url);

    let mut spec = ProxySpec {
        protocol: Protocol::Trojan,
        address: host.clone(),
        port: url.port().unwrap_or(443),
        secret: decode_percent(url.username()),
        ..Default::default()
    };
    fill_common(&mut spec, &params, &host);
    // Trojan defaults to TLS when the link does not say otherwise.
    if !params.contains_key("security") && !params.contains_key("tls") {
        spec.security = Security::Tls;
    }
    spec.name = url.fragment().map(decode_percent).unwrap_or_default();
    Ok(spec)
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for engine in engines {
        if let Ok(bytes) = engine.decode(cleaned.as_bytes()) {
            return Ok(bytes);
        }
    }
    Err(CoreError::InvalidLink("invalid base64 payload".into()))
}

fn parse_vmess(rest: &str) -> Result<ProxySpec> {
    let decoded = decode_base64(rest.split('#').next().unwrap_or(rest))?;
    let value: Value = serde_json::from_slice(&decoded)
        .map_err(|e| CoreError::InvalidLink(format!("vmess: {e}")))?;

    let host = value
        .get("add")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::InvalidLink("vmess: missing add".into()))?
        .to_string();
    let port = value
        .get("port")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .ok_or_else(|| CoreError::InvalidLink("vmess: missing port".into()))?;
    let port: u16 = port
        .try_into()
        .map_err(|_| CoreError::InvalidLink("vmess: invalid port".into()))?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::InvalidLink("vmess: missing id".into()))?
        .to_string();
    let alter_id = value
        .get("aid")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0);
    let alter_id: u32 = alter_id
        .try_into()
        .map_err(|_| CoreError::InvalidLink("vmess: invalid alterId".into()))?;
    let security_method = value
        .get("scy")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("auto")
        .to_string();

    let mut params = HashMap::new();
    for key in ["net", "type", "host", "path", "sni", "alpn", "fp"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                params.insert(key.to_string(), text.to_string());
            }
        }
    }
    if value.get("tls").and_then(Value::as_str) == Some("tls") {
        params.insert("security".into(), "tls".into());
    }

    let mut spec = ProxySpec {
        protocol: Protocol::Vmess,
        address: host.clone(),
        port,
        secret: id,
        alter_id,
        encryption: security_method,
        ..Default::default()
    };
    fill_common(&mut spec, &params, &host);
    spec.name = value
        .get("ps")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(spec)
}

fn parse_shadowsocks(rest: &str) -> Result<ProxySpec> {
    let (body, fragment) = match rest.split_once('#') {
        Some((body, fragment)) => (body, Some(decode_percent(fragment))),
        None => (rest, None),
    };

    let (credentials, address) = if let Some((credentials, address)) = body.rsplit_once('@') {
        (credentials.to_string(), address.to_string())
    } else {
        // Legacy form: ss://base64(method:password@host:port)
        let decoded = String::from_utf8(decode_base64(body)?)
            .map_err(|_| CoreError::InvalidLink("ss: payload is not UTF-8".into()))?;
        let (credentials, address) = decoded
            .rsplit_once('@')
            .ok_or_else(|| CoreError::InvalidLink("ss: missing '@'".into()))?;
        (credentials.to_string(), address.to_string())
    };

    let credentials = if credentials.contains(':') {
        decode_percent(&credentials)
    } else {
        String::from_utf8(decode_base64(&credentials)?)
            .map_err(|_| CoreError::InvalidLink("ss: credentials are not UTF-8".into()))?
    };

    let (method, password) = credentials
        .split_once(':')
        .ok_or_else(|| CoreError::InvalidLink("ss: missing method/password".into()))?;

    let (host, port) = parse_host_port(&address)?;

    Ok(ProxySpec {
        name: fragment.unwrap_or_default(),
        protocol: Protocol::Shadowsocks,
        address: host,
        port,
        secret: password.to_string(),
        method: method.to_string(),
        network: "tcp".to_string(),
        ..Default::default()
    })
}

fn parse_host_port(address: &str) -> Result<(String, u16)> {
    if let Some(stripped) = address.strip_prefix('[') {
        let (host, rest) = stripped
            .split_once(']')
            .ok_or_else(|| CoreError::InvalidLink("malformed IPv6 address".into()))?;
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| CoreError::InvalidLink("missing port".into()))?
            .parse()
            .map_err(|_| CoreError::InvalidLink("invalid port".into()))?;
        return Ok((host.to_string(), port));
    }

    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| CoreError::InvalidLink("missing port".into()))?;
    let port = port
        .parse()
        .map_err(|_| CoreError::InvalidLink("invalid port".into()))?;
    Ok((host.to_string(), port))
}

// ---------------------------------------------------------------------------
// Xray rendering
// ---------------------------------------------------------------------------

pub fn xray_outbound(spec: &ProxySpec) -> Value {
    let stream = xray_stream_settings(spec);
    let outbound = match spec.protocol {
        Protocol::Vless => {
            let mut user = json!({
                "id": spec.secret,
                "encryption": if spec.encryption.is_empty() { "none" } else { &spec.encryption },
            });
            if !spec.flow.is_empty() {
                user["flow"] = json!(spec.flow);
            }
            json!({
                "tag": "proxy",
                "protocol": "vless",
                "settings": { "vnext": [{
                    "address": spec.address,
                    "port": spec.port,
                    "users": [user]
                }] },
                "streamSettings": stream
            })
        }
        Protocol::Vmess => json!({
            "tag": "proxy",
            "protocol": "vmess",
            "settings": { "vnext": [{
                "address": spec.address,
                "port": spec.port,
                "users": [{
                    "id": spec.secret,
                    "alterId": spec.alter_id,
                    "security": if spec.encryption.is_empty() { "auto" } else { &spec.encryption }
                }]
            }] },
            "streamSettings": stream
        }),
        Protocol::Trojan => json!({
            "tag": "proxy",
            "protocol": "trojan",
            "settings": { "servers": [{
                "address": spec.address,
                "port": spec.port,
                "password": spec.secret
            }] },
            "streamSettings": stream
        }),
        Protocol::Shadowsocks => json!({
            "tag": "proxy",
            "protocol": "shadowsocks",
            "settings": { "servers": [{
                "address": spec.address,
                "port": spec.port,
                "method": spec.method,
                "password": spec.secret
            }] }
        }),
    };
    prune_nulls(outbound)
}

fn xray_stream_settings(spec: &ProxySpec) -> Value {
    let mut stream = json!({ "network": spec.network });

    match spec.security {
        Security::Tls => {
            stream["security"] = json!("tls");
            let mut tls = json!({
                "serverName": spec.sni,
                "allowInsecure": spec.allow_insecure,
            });
            if !spec.alpn.is_empty() {
                tls["alpn"] = json!(spec.alpn);
            }
            if !spec.fingerprint.is_empty() {
                tls["fingerprint"] = json!(spec.fingerprint);
            }
            stream["tlsSettings"] = tls;
        }
        Security::Reality => {
            stream["security"] = json!("reality");
            let mut reality = json!({
                "serverName": spec.sni,
                "fingerprint": spec.fingerprint,
                "publicKey": spec.public_key,
                "shortId": spec.short_id,
                "spiderX": spec.spider_x,
            });
            if !spec.alpn.is_empty() {
                reality["alpn"] = json!(spec.alpn);
            }
            stream["realitySettings"] = reality;
        }
        Security::None => {
            stream["security"] = json!("none");
        }
    }

    match spec.network.as_str() {
        "ws" => {
            let mut settings = json!({
                "path": spec.path,
                "headers": { "Host": spec.host_header }
            });
            if spec.ws_max_early_data > 0 {
                settings["maxEarlyData"] = json!(spec.ws_max_early_data);
            }
            if !spec.ws_early_data_header.is_empty() {
                settings["earlyDataHeaderName"] = json!(spec.ws_early_data_header);
            }
            stream["wsSettings"] = settings;
        }
        "grpc" => {
            let mut settings = json!({ "serviceName": spec.service_name });
            if spec.grpc_multi_mode {
                settings["multiMode"] = json!(true);
            }
            if !spec.grpc_authority.is_empty() {
                settings["authority"] = json!(spec.grpc_authority);
            }
            stream["grpcSettings"] = settings;
        }
        "h2" | "http" => {
            stream["httpSettings"] = json!({
                "path": spec.path,
                "host": [spec.host_header]
            });
        }
        "xhttp" | "splithttp" => {
            // XHTTP replaced the old SpliHTTP name; both use xhttpSettings.
            stream["network"] = json!("xhttp");
            let mut settings = json!({
                "path": spec.path,
                "host": spec.host_header
            });
            if !spec.xhttp_mode.is_empty() {
                settings["mode"] = json!(spec.xhttp_mode);
            }
            if let Some(extra) = &spec.xhttp_extra {
                settings["extra"] = extra.clone();
            }
            stream["xhttpSettings"] = settings;
        }
        "httpupgrade" => {
            stream["httpupgradeSettings"] = json!({
                "path": spec.path,
                "host": spec.host_header
            });
        }
        "tcp" => {
            if spec.header_type == "http" {
                stream["tcpSettings"] = json!({
                    "header": {
                        "type": "http",
                        "request": {
                            "path": [spec.path],
                            "headers": { "Host": [spec.host_header] }
                        }
                    }
                });
            }
        }
        _ => {}
    }

    prune_nulls(stream)
}

// ---------------------------------------------------------------------------
// sing-box rendering
// ---------------------------------------------------------------------------

pub fn singbox_outbound(spec: &ProxySpec) -> Value {
    let mut outbound = match spec.protocol {
        Protocol::Vless => {
            let mut value = json!({
                "type": "vless",
                "tag": "proxy",
                "server": spec.address,
                "server_port": spec.port,
                "uuid": spec.secret,
            });
            if !spec.flow.is_empty() {
                value["flow"] = json!(spec.flow);
            }
            value
        }
        Protocol::Vmess => json!({
            "type": "vmess",
            "tag": "proxy",
            "server": spec.address,
            "server_port": spec.port,
            "uuid": spec.secret,
            "security": if spec.encryption.is_empty() { "auto" } else { &spec.encryption },
            "alter_id": spec.alter_id,
        }),
        Protocol::Trojan => json!({
            "type": "trojan",
            "tag": "proxy",
            "server": spec.address,
            "server_port": spec.port,
            "password": spec.secret,
        }),
        Protocol::Shadowsocks => json!({
            "type": "shadowsocks",
            "tag": "proxy",
            "server": spec.address,
            "server_port": spec.port,
            "method": spec.method,
            "password": spec.secret,
        }),
    };

    if spec.protocol != Protocol::Shadowsocks {
        if let Some(tls) = singbox_tls(spec) {
            outbound["tls"] = tls;
        }
        if let Some(transport) = singbox_transport(spec) {
            outbound["transport"] = transport;
        }
    }

    prune_nulls(outbound)
}

fn singbox_tls(spec: &ProxySpec) -> Option<Value> {
    match spec.security {
        Security::None => None,
        Security::Tls => {
            let mut tls = json!({
                "enabled": true,
                "server_name": spec.sni,
                "insecure": spec.allow_insecure,
            });
            if !spec.alpn.is_empty() {
                tls["alpn"] = json!(spec.alpn);
            }
            if !spec.fingerprint.is_empty() {
                tls["utls"] = json!({
                    "enabled": true,
                    "fingerprint": spec.fingerprint
                });
            }
            Some(prune_nulls(tls))
        }
        Security::Reality => Some(prune_nulls(json!({
            "enabled": true,
            "server_name": spec.sni,
            "utls": {
                "enabled": true,
                "fingerprint": if spec.fingerprint.is_empty() { "chrome" } else { &spec.fingerprint }
            },
            "reality": {
                "enabled": true,
                "public_key": spec.public_key,
                "short_id": spec.short_id
            }
        }))),
    }
}

fn singbox_transport(spec: &ProxySpec) -> Option<Value> {
    match spec.network.as_str() {
        "ws" => Some(prune_nulls(json!({
            "type": "ws",
            "path": spec.path,
            "headers": { "Host": spec.host_header },
            "max_early_data": if spec.ws_max_early_data > 0 { Some(spec.ws_max_early_data) } else { None },
            "early_data_header_name": if spec.ws_early_data_header.is_empty() { None } else { Some(spec.ws_early_data_header.clone()) }
        }))),
        "grpc" => Some(prune_nulls(json!({
            "type": "grpc",
            "service_name": spec.service_name,
            "authority": if spec.grpc_authority.is_empty() { None } else { Some(spec.grpc_authority.clone()) }
        }))),
        "h2" | "http" => Some(prune_nulls(json!({
            "type": "http",
            "path": spec.path,
            "host": [spec.host_header]
        }))),
        "httpupgrade" => Some(prune_nulls(json!({
            "type": "httpupgrade",
            "path": spec.path,
            "host": spec.host_header
        }))),
        _ => None,
    }
}

fn prune_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, prune_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(prune_nulls).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vless_reality() -> ProxySpec {
        let link = "vless://11111111-2222-3333-4444-555555555555@example.com:443?security=reality&sni=example.com&fp=chrome&pbk=abc&sid=01&type=grpc&serviceName=grpc#My%20Node";
        parse(None, link).unwrap()
    }

    #[test]
    fn parses_vless_reality() {
        let spec = vless_reality();
        assert_eq!(spec.name, "My Node");
        assert_eq!(spec.protocol, Protocol::Vless);
        assert_eq!(spec.security, Security::Reality);
        assert_eq!(spec.network, "grpc");

        let xray = xray_outbound(&spec);
        assert_eq!(xray["protocol"], "vless");
        assert_eq!(xray["streamSettings"]["security"], "reality");

        let singbox = singbox_outbound(&spec);
        assert_eq!(singbox["type"], "vless");
        assert_eq!(singbox["tls"]["reality"]["public_key"], "abc");
        assert_eq!(singbox["transport"]["type"], "grpc");
    }

    #[test]
    fn renders_xhttp_transport() {
        let spec = parse(
            None,
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?type=xhttp&security=tls&sni=example.com&path=%2Fxhttp&host=example.com&mode=auto#XHTTP",
        )
        .unwrap();
        assert_eq!(spec.network, "xhttp");
        assert_eq!(spec.xhttp_mode, "auto");
        let xray = crate::config::config_from_spec(crate::CoreKind::Xray, &spec);
        let stream = &xray["outbounds"][0]["streamSettings"];
        assert_eq!(stream["network"], json!("xhttp"));
        assert_eq!(stream["xhttpSettings"]["path"], json!("/xhttp"));
        assert_eq!(stream["xhttpSettings"]["mode"], json!("auto"));
        assert_eq!(stream["xhttpSettings"]["host"], json!("example.com"));
    }

    #[test]
    fn renders_ws_early_data_and_grpc_multi() {
        let ws = parse(
            None,
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?type=ws&security=tls&path=%2Fws&host=example.com&ed=2048&eh=Sec-WebSocket-Protocol#WS",
        )
        .unwrap();
        let ws_config = crate::config::config_from_spec(crate::CoreKind::Xray, &ws);
        let ws_settings = &ws_config["outbounds"][0]["streamSettings"]["wsSettings"];
        assert_eq!(ws_settings["maxEarlyData"], json!(2048));
        assert_eq!(
            ws_settings["earlyDataHeaderName"],
            json!("Sec-WebSocket-Protocol")
        );

        let grpc = parse(
            None,
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?type=grpc&security=tls&serviceName=grpc&mode=multi&authority=grpc.example.com#GRPC",
        )
        .unwrap();
        let grpc_config = crate::config::config_from_spec(crate::CoreKind::Xray, &grpc);
        let grpc_settings = &grpc_config["outbounds"][0]["streamSettings"]["grpcSettings"];
        assert_eq!(grpc_settings["multiMode"], json!(true));
        assert_eq!(grpc_settings["authority"], json!("grpc.example.com"));

        let raw = parse(
            None,
            "vless://11111111-2222-3333-4444-555555555555@example.com:443?type=raw&security=reality&sni=example.com&pbk=abc&sid=01#Raw",
        )
        .unwrap();
        assert_eq!(raw.network, "tcp");
    }

    #[test]
    fn parses_shadowsocks_sip002() {
        let userinfo =
            base64::engine::general_purpose::STANDARD_NO_PAD.encode("aes-128-gcm:secret");
        let link = format!("ss://{userinfo}@example.com:8388#SS");
        let spec = parse(None, &link).unwrap();
        assert_eq!(spec.name, "SS");
        assert_eq!(spec.protocol, Protocol::Shadowsocks);
        assert_eq!(spec.method, "aes-128-gcm");
        assert_eq!(singbox_outbound(&spec)["type"], "shadowsocks");
    }

    #[test]
    fn parses_vmess() {
        let payload = json!({
            "v": "2",
            "ps": "VM",
            "add": "example.com",
            "port": "443",
            "id": "11111111-2222-3333-4444-555555555555",
            "aid": "0",
            "net": "ws",
            "path": "/ws",
            "host": "example.com",
            "tls": "tls"
        });
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        let spec = parse(None, &format!("vmess://{encoded}")).unwrap();
        assert_eq!(spec.name, "VM");
        assert_eq!(spec.network, "ws");
        assert_eq!(spec.security, Security::Tls);
        assert_eq!(singbox_outbound(&spec)["transport"]["type"], "ws");
    }

    #[test]
    fn parses_trojan_defaults_to_tls() {
        let spec = parse(None, "trojan://password@example.com:443#T").unwrap();
        assert_eq!(spec.protocol, Protocol::Trojan);
        assert_eq!(spec.security, Security::Tls);
        assert_eq!(xray_outbound(&spec)["protocol"], "trojan");
    }
}
