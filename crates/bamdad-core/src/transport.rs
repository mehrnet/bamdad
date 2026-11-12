//! Transport selection shared by desktop and Android.
//!
//! Proxy transports use the existing Xray/sing-box cores plus the platform
//! tunnel shim. Native transports are implemented by platform backends; the
//! UI only sees the normalized profile and status types below.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Xray or sing-box through the platform proxy tunnel.
    #[default]
    Proxy,
    WireGuard,
    AmneziaWg,
    IpSec,
    OpenVpn,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::WireGuard => "wireguard",
            Self::AmneziaWg => "amneziawg",
            Self::IpSec => "ipsec",
            Self::OpenVpn => "openvpn",
        }
    }

    pub fn is_proxy(self) -> bool {
        matches!(self, Self::Proxy)
    }

    pub fn from_protocol(protocol: &str) -> Self {
        match protocol.to_ascii_lowercase().as_str() {
            "wireguard" | "wg" => Self::WireGuard,
            "amneziawg" | "awg" => Self::AmneziaWg,
            "ipsec" | "ikev2" | "strongswan" => Self::IpSec,
            "openvpn" | "ovpn" => Self::OpenVpn,
            _ => Self::Proxy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TunnelState {
    #[default]
    Down,
    Connecting,
    Up,
    Disconnecting,
    Error,
}

/// Transport-neutral request handed to an Android native backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelProfile {
    pub id: String,
    pub name: String,
    pub transport: Transport,
    /// Original `.conf`, `.ovpn`, `.sswan`, or proxy JSON.
    pub config: String,
    #[serde(default)]
    pub dns: Vec<String>,
    #[serde(default)]
    pub allowed_apps: Vec<String>,
    #[serde(default)]
    pub excluded_apps: Vec<String>,
    #[serde(default)]
    pub bypass_cidrs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u16>,
    #[serde(default = "default_accent")]
    pub accent: String,
}

fn default_accent() -> String {
    "cyan".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TunnelStatus {
    pub state: TunnelState,
    #[serde(default)]
    pub rx_bytes: u64,
    #[serde(default)]
    pub tx_bytes: u64,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub local_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
