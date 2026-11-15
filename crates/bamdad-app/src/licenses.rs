//! Static license inventory for the in-app open-source licenses screen.
//!
//! Mirrors `THIRD_PARTY_NOTICES.md`; keep both in sync when a component is
//! added, removed, or relicensed.

use crate::LicenseRow;

/// One row for the licenses screen.
struct Entry {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    note: &'static str,
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: "Bamdad",
        version: "",
        license: "GPL-3.0-only",
        note: "This application: Rust/Slint shell, profiles, and VPN controller.",
    },
    Entry {
        name: "Slint",
        version: "",
        license: "GPL-3.0-only",
        note: "Declarative UI toolkit used for every screen.",
    },
    Entry {
        name: "Xray-core",
        version: "",
        license: "MPL-2.0",
        note: "Bundled sidecar proxy core for VMess/VLESS/Trojan/Shadowsocks links.",
    },
    Entry {
        name: "sing-box",
        version: "",
        license: "GPL-3.0",
        note: "Bundled sidecar proxy core for sing-box profiles and subscriptions.",
    },
    Entry {
        name: "tun2socks",
        version: "v2.7.0",
        license: "MIT",
        note: "Forwards the Android proxy VpnService TUN to the core SOCKS listener.",
    },
    Entry {
        name: "OpenVPN 3 Core",
        version: "release/3.11.8",
        license: "MPL-2.0",
        note: "Android OpenVPN client, used under its MPL-2.0 option.",
    },
    Entry {
        name: "amneziawg-android",
        version: "",
        license: "Apache-2.0",
        note: "Userspace WireGuard and AmneziaWG backend with its Go tunnelling core.",
    },
    Entry {
        name: "amneziawg-go",
        version: "",
        license: "MIT",
        note: "Userspace WireGuard-compatible tunnelling core embedded in the Android backend.",
    },
    Entry {
        name: "mbedTLS",
        version: "2.28.4",
        license: "Apache-2.0",
        note: "TLS library linked into the OpenVPN 3 core.",
    },
    Entry {
        name: "asio",
        version: "1.24.0",
        license: "BSL-1.0",
        note: "Networking layer linked into the OpenVPN 3 core.",
    },
    Entry {
        name: "LZ4",
        version: "1.8.3",
        license: "BSD-2-Clause",
        note: "Compression for OpenVPN 3 payloads.",
    },
    Entry {
        name: "zip4j",
        version: "",
        license: "Apache-2.0",
        note: "Password-protected ZIP import on Android.",
    },
    Entry {
        name: "Twemoji",
        version: "",
        license: "CC-BY 4.0",
        note: "Emoji artwork used in profile names.",
    },
    Entry {
        name: "Noto Emoji",
        version: "",
        license: "OFL-1.1",
        note: "Monochrome emoji fallback font.",
    },
];

/// Builds the rows shown in the licenses screen.
pub fn rows(app_version: &str, xray: &str, singbox: &str) -> Vec<LicenseRow> {
    ENTRIES
        .iter()
        .map(|entry| {
            let version = match entry.name {
                "Bamdad" => app_version,
                "Xray-core" => xray,
                "sing-box" => singbox,
                _ => entry.version,
            };
            LicenseRow {
                name: entry.name.into(),
                version: version.into(),
                license: entry.license.into(),
                note: entry.note.into(),
            }
        })
        .collect()
}
