//! Desktop tunnel: the privileged helper plus the tun2socks sidecar.
//!
//! Implemented for Linux. Other desktop platforms keep the system-proxy path;
//! `start()` returns a clear error there so the UI can explain it.
//!
//! The helper is started through `pkexec` so the desktop session asks for
//! approval once per connection; set `BAMDAD_TUN_NO_POLKIT=1` (or run as root)
//! to start it directly, which the integration test does.

#![cfg(not(target_os = "android"))]

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use bamdad_core::paths;

/// Whether this build can tunnel desktop traffic at all.
pub fn supported() -> bool {
    cfg!(target_os = "linux")
}

/// Host routes the helper must keep on the physical link: the profile's
/// endpoint addresses.
pub fn bypass_for(host: &str, port: u16) -> Vec<String> {
    use std::net::{IpAddr, ToSocketAddrs};
    let mut out: Vec<String> = Vec::new();
    if let Ok(addresses) = (host, port).to_socket_addrs() {
        for address in addresses {
            let cidr = match address.ip() {
                IpAddr::V4(ip) => format!("{ip}/32"),
                IpAddr::V6(ip) => format!("{ip}/128"),
            };
            if !out.contains(&cidr) {
                out.push(cidr);
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    static ACTIVE: Mutex<Option<Child>> = Mutex::new(None);

    fn binary(env_var: &str, name: &str) -> Option<PathBuf> {
        if let Ok(path) = std::env::var(env_var) {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
        })
    }

    /// Starts the tunnel and returns a human readable description for the log.
    pub fn start(
        socks_port: u16,
        bypass: &[String],
        dns: &[String],
        routes: &[String],
    ) -> Result<String, String> {
        stop();

        let helper = binary("BAMDAD_TUN_HELPER", "bamdad-tun-helper")
            .ok_or_else(|| "bamdad-tun-helper not found (set BAMDAD_TUN_HELPER)".to_string())?;
        let tun2socks = binary("BAMDAD_TUN2SOCKS", "bamdad-tun2socks")
            .ok_or_else(|| "bamdad-tun2socks not found (set BAMDAD_TUN2SOCKS)".to_string())?;

        let state = paths::run_dir().join("tun.state");
        let direct =
            std::env::var("BAMDAD_TUN_NO_POLKIT").is_ok() || unsafe { libc::geteuid() } == 0;
        let mut command = if direct {
            Command::new(&helper)
        } else {
            let mut command = Command::new("pkexec");
            command.arg(&helper);
            command
        };
        command.args([
            "up",
            "--name",
            "bamdad0",
            "--address",
            "10.255.0.1/24",
            "--mtu",
            "1400",
            "--state",
        ]);
        command.arg(&state);
        command
            .arg("--parent-pid")
            .arg(std::process::id().to_string());
        for cidr in routes {
            command.arg("--route").arg(cidr);
        }
        if routes.is_empty() {
            for cidr in bypass {
                command.arg("--bypass").arg(cidr);
            }
        }
        for server in dns {
            command.arg("--dns").arg(server);
        }
        command.arg("--exec").arg(&tun2socks);
        command.args([
            "-device",
            "fd://3",
            "-proxy",
            &format!("socks5://127.0.0.1:{socks_port}"),
            "--loglevel",
            "warn",
        ]);
        command.stdout(Stdio::null()).stderr(Stdio::null());

        let mut child = command
            .spawn()
            .map_err(|error| format!("start {}: {error}", helper.display()))?;

        // The helper configures routing before it reports ready; fail loudly if
        // it exits immediately (no polkit agent, not root, ...).
        let deadline = Instant::now() + Duration::from_millis(2500);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(format!(
                        "tunnel helper exited with {status}; is a polkit agent available?"
                    ))
                }
                Ok(None) if Instant::now() >= deadline => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(error) => return Err(format!("tunnel helper: {error}")),
            }
        }

        *ACTIVE.lock().unwrap() = Some(child);
        Ok(format!(
            "desktop tunnel up: 10.255.0.1/24 via tun2socks on socks 127.0.0.1:{socks_port}"
        ))
    }

    /// Stops the helper (which restores routing) and the sidecar.
    pub fn stop() {
        let child = ACTIVE.lock().unwrap().take();
        let Some(mut child) = child else { return };
        let pid = child.id() as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::{start, stop};

#[cfg(not(target_os = "linux"))]
pub fn start(
    _socks_port: u16,
    _bypass: &[String],
    _dns: &[String],
    _routes: &[String],
) -> Result<String, String> {
    Err("tunnel mode is only implemented on Linux so far".to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn stop() {}
