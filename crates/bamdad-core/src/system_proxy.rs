//! Best-effort OS-level proxy toggles.
//!
//! These are deliberately conservative: applications that honor the system
//! proxy will route through Bamdad's local SOCKS/HTTP listeners after
//! `enable()`. TUN mode (which captures traffic regardless of app support) is
//! a separate, privileged path planned on top of the same engine.

use std::process::Command;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, Copy)]
pub struct ProxySettings {
    pub socks_port: u16,
    pub http_port: u16,
}

pub fn enable(settings: ProxySettings) -> Result<()> {
    platform_enable(settings)
}

pub fn disable() -> Result<()> {
    platform_disable()
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| CoreError::Spawn(format!("{program}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CoreError::Spawn(format!("{program} exited with {status}")))
    }
}

#[cfg(target_os = "linux")]
fn platform_enable(settings: ProxySettings) -> Result<()> {
    if command_exists("gsettings") {
        let socks_port = settings.socks_port.to_string();
        let http_port = settings.http_port.to_string();
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "mode", "manual"],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.socks", "host", "127.0.0.1"],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.socks", "port", &socks_port],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.http", "host", "127.0.0.1"],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.http", "port", &http_port],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.https", "host", "127.0.0.1"],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.https", "port", &http_port],
        )?;
        return Ok(());
    }

    let kwrite = if command_exists("kwriteconfig6") {
        Some("kwriteconfig6")
    } else if command_exists("kwriteconfig5") {
        Some("kwriteconfig5")
    } else {
        None
    };

    if let Some(kwrite) = kwrite {
        let http = format!("http://127.0.0.1:{}", settings.http_port);
        let socks = format!("socks://127.0.0.1:{}", settings.socks_port);
        run(
            kwrite,
            &[
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "ProxyType",
                "1",
            ],
        )?;
        run(
            kwrite,
            &[
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "httpProxy",
                &http,
            ],
        )?;
        run(
            kwrite,
            &[
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "socksProxy",
                &socks,
            ],
        )?;
        return Ok(());
    }

    Err(CoreError::Spawn(
        "no supported desktop proxy backend found (GNOME gsettings or KDE kwriteconfig). \
         Configure 127.0.0.1 SOCKS/HTTP manually."
            .to_string(),
    ))
}

#[cfg(target_os = "linux")]
fn platform_disable() -> Result<()> {
    if command_exists("gsettings") {
        return run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "mode", "none"],
        );
    }
    let kwrite = if command_exists("kwriteconfig6") {
        Some("kwriteconfig6")
    } else if command_exists("kwriteconfig5") {
        Some("kwriteconfig5")
    } else {
        None
    };
    if let Some(kwrite) = kwrite {
        return run(
            kwrite,
            &[
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                "ProxyType",
                "0",
            ],
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_enable(settings: ProxySettings) -> Result<()> {
    let mut any = false;
    for service in ["Wi-Fi", "Ethernet"] {
        let socks = settings.socks_port.to_string();
        let http = settings.http_port.to_string();
        let commands: [&[&str]; 6] = [
            &["-setsocksfirewallproxy", service, "127.0.0.1", &socks],
            &["-setsocksfirewallproxystate", service, "on"],
            &["-setwebproxy", service, "127.0.0.1", &http],
            &["-setwebproxystate", service, "on"],
            &["-setsecurewebproxy", service, "127.0.0.1", &http],
            &["-setsecurewebproxystate", service, "on"],
        ];
        for args in commands {
            if run("networksetup", args).is_ok() {
                any = true;
            }
        }
    }
    if any {
        Ok(())
    } else {
        Err(CoreError::Spawn(
            "could not configure any network service with networksetup".to_string(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn platform_disable() -> Result<()> {
    for service in ["Wi-Fi", "Ethernet"] {
        let _ = run(
            "networksetup",
            &["-setsocksfirewallproxystate", service, "off"],
        );
        let _ = run("networksetup", &["-setwebproxystate", service, "off"]);
        let _ = run("networksetup", &["-setsecurewebproxystate", service, "off"]);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn platform_enable(settings: ProxySettings) -> Result<()> {
    let server = format!(
        "socks=127.0.0.1:{};http=127.0.0.1:{}",
        settings.socks_port, settings.http_port
    );
    run(
        "reg",
        &[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyEnable",
            "/t",
            "REG_DWORD",
            "/d",
            "1",
            "/f",
        ],
    )?;
    run(
        "reg",
        &[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
            "/t",
            "REG_SZ",
            "/d",
            &server,
            "/f",
        ],
    )?;
    // Best-effort: refresh WinINET settings for already-running apps.
    let _ = run("rundll32", &["wininet.dll,InternetSetOption"]);
    Ok(())
}

#[cfg(target_os = "windows")]
fn platform_disable() -> Result<()> {
    run(
        "reg",
        &[
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyEnable",
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ],
    )?;
    let _ = run("rundll32", &["wininet.dll,InternetSetOption"]);
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_enable(_settings: ProxySettings) -> Result<()> {
    Err(CoreError::Spawn(
        "system proxy is not implemented on this platform".to_string(),
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_disable() -> Result<()> {
    Ok(())
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .any(|candidate| candidate.is_file())
        })
        .unwrap_or(false)
}
