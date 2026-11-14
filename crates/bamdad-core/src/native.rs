//! Detection and profile construction for native tunnel configs.
//!
//! Phase 1 covers WireGuard and AmneziaWG `.conf` files. The raw config is
//! kept intact and handed to the Android `amneziawg-android` backend.

use serde_json::Value;

use crate::config::{new_id, Profile};
use crate::transport::Transport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeConfig {
    pub transport: Transport,
    pub address: String,
    pub port: u16,
}

/// Detect whether `text` is a WireGuard/AmneziaWG config.
pub fn detect(text: &str) -> Option<NativeConfig> {
    let trimmed = text
        .trim_start_matches(['\u{feff}', '\u{200b}'])
        .trim_start();
    if !trimmed.starts_with("[Interface]") {
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("client")
            && (lower.contains("remote ")
                || lower.contains("<ca>")
                || lower.contains("<tls-crypt>"))
        {
            let (address, port) = parse_openvpn_remote(trimmed);
            return Some(NativeConfig {
                transport: Transport::OpenVpn,
                address,
                port,
            });
        }
        return None;
    }
    if !trimmed.contains("PrivateKey") {
        return None;
    }

    let mut transport = Transport::WireGuard;
    for line in trimmed.lines() {
        let key = line
            .split_once('=')
            .map(|(key, _)| key.trim())
            .unwrap_or("");
        if matches!(
            key,
            "Jc" | "Jmin"
                | "Jmax"
                | "S1"
                | "S2"
                | "S3"
                | "S4"
                | "H1"
                | "H2"
                | "H3"
                | "H4"
                | "I1"
                | "I2"
                | "I3"
                | "I4"
                | "I5"
        ) {
            transport = Transport::AmneziaWg;
            break;
        }
    }

    let (address, port) = parse_endpoint(trimmed);
    Some(NativeConfig {
        transport,
        address,
        port,
    })
}

fn parse_openvpn_remote(text: &str) -> (String, u16) {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if !parts
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case("remote"))
        {
            continue;
        }
        let Some(host) = parts.next() else { continue };
        let port = parts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1194);
        return (host.to_string(), port);
    }
    (String::new(), 0)
}

fn parse_endpoint(text: &str) -> (String, u16) {
    let mut in_peer = false;
    for line in text.lines() {
        let line = line.trim();
        if line.eq_ignore_ascii_case("[Peer]") {
            in_peer = true;
            continue;
        }
        if line.eq_ignore_ascii_case("[Interface]") {
            in_peer = false;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if in_peer && key.eq_ignore_ascii_case("Endpoint") {
            if let Some((host, port)) = value.rsplit_once(':') {
                if let Ok(port) = port.parse() {
                    return (host.trim_matches(['[', ']']).to_string(), port);
                }
            }
        }
    }
    (String::new(), 0)
}

/// Build a native profile from a pasted/imported config body.
pub fn profile_from_config(name_hint: Option<&str>, text: &str) -> Option<Profile> {
    let detected = detect(text)?;
    let name = name_hint
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if detected.address.is_empty() {
                format!("{} config", detected.transport.label())
            } else {
                format!(
                    "{} · {}:{}",
                    detected.transport.label(),
                    detected.address,
                    detected.port
                )
            }
        });
    Some(Profile {
        id: new_id(),
        name,
        core: crate::kind::CoreKind::SingBox,
        transport: detected.transport,
        protocol: detected.transport.label().to_string(),
        address: detected.address,
        port: detected.port,
        config: Value::Null,
        raw_config: Some(text.to_string()),
        source: None,
        group_id: None,
        group_name: None,
        group_url: None,
        chain: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WG: &str = r#"[Interface]
PrivateKey = aGVsbG8=
Address = 10.0.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = d29ybGQ=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = engage.example.com:51820
"#;

    #[test]
    fn detects_wireguard() {
        let config = detect(WG).unwrap();
        assert_eq!(config.transport, Transport::WireGuard);
        assert_eq!(config.address, "engage.example.com");
        assert_eq!(config.port, 51820);
    }

    #[test]
    fn detects_amneziawg() {
        let text = WG.replace("Address = 10.0.0.2/32", "Address = 10.0.0.2/32\nJc = 3");
        let config = detect(&text).unwrap();
        assert_eq!(config.transport, Transport::AmneziaWg);
    }

    #[test]
    fn handles_bom_and_quick_style_options() {
        let text = format!(
            "\u{feff}{}",
            WG.replace(
                "DNS = 1.1.1.1",
                "MTU = 1300\nDNS = 208.67.222.222,208.67.220.220"
            )
        );
        let config = detect(&text).unwrap();
        assert_eq!(config.transport, Transport::WireGuard);
        assert_eq!(config.port, 51820);
    }

    #[test]
    fn detects_openvpn() {
        let text = r#"client
dev tun
proto udp
remote vpn.example.com 1194
<ca>
-----BEGIN CERTIFICATE-----
...
-----END CERTIFICATE-----
</ca>
"#;
        let config = detect(text).unwrap();
        assert_eq!(config.transport, Transport::OpenVpn);
        assert_eq!(config.address, "vpn.example.com");
        assert_eq!(config.port, 1194);
    }

    #[test]
    fn rejects_proxy_json() {
        assert!(detect("{\"outbounds\": []}").is_none());
    }

    #[test]
    fn builds_native_profile() {
        let profile = profile_from_config(Some("Test WG"), WG).unwrap();
        assert_eq!(profile.name, "Test WG");
        assert_eq!(profile.transport, Transport::WireGuard);
        assert_eq!(profile.raw_config.as_deref(), Some(WG));
        assert!(profile.is_proxy() == false);
    }
}
