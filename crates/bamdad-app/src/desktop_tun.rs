//! Desktop tunnel: the privileged helper plus the tun2socks sidecar.
//!
//! Only Linux is implemented. The helper is started through `pkexec` so the
//! desktop session asks for approval once per connection; set
//! `BAMDAD_TUN_NO_POLKIT=1` (or run as root) to start it directly, which the
//! integration test does.

#![cfg(not(target_os = "android"))]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bamdad_core::paths;

static ACTIVE: Mutex<Option<Child>> = Mutex::new(None);

/// Whether this build can tunnel desktop traffic at all.
pub fn supported() -> bool {
    cfg!(target_os = "linux")
}

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
pub fn start(socks_port: u16, bypass: &[String]) -> Result<String, String> {
    if !supported() {
        return Err("tunnel mode is only implemented on Linux".to_string());
    }
    stop();

    let helper = binary("BAMDAD_TUN_HELPER", "bamdad-tun-helper")
        .ok_or_else(|| "bamdad-tun-helper not found (set BAMDAD_TUN_HELPER)".to_string())?;
    let tun2socks = binary("BAMDAD_TUN2SOCKS", "bamdad-tun2socks")
        .ok_or_else(|| "bamdad-tun2socks not found (set BAMDAD_TUN2SOCKS)".to_string())?;

    let state = paths::run_dir().join("tun.state");
    let direct = std::env::var("BAMDAD_TUN_NO_POLKIT").is_ok() || unsafe { libc::geteuid() } == 0;
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
    for cidr in bypass {
        command.arg("--bypass").arg(cidr);
    }
    command.arg("--exec").arg(&tun2socks);
    command.args([
        "-device",
        "fd://3",
        "-proxy",
        &format!("socks5://127.0.0.1:{socks_port}"),
        "-loglevel",
        "warn",
    ]);
    command.stdout(Stdio::null()).stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|error| format!("start {}: {error}", helper.display()))?;

    // The helper configures routing before it prints "ready"; give it a moment
    // and fail loudly if it exits immediately (no polkit agent, not root, ...).
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

/// Host routes the helper must keep on the physical link: the profile's
/// endpoint addresses.
pub fn bypass_for(host: &str, port: u16) -> Vec<String> {
    use std::net::{IpAddr, ToSocketAddrs};
    let mut out = Vec::new();
    if let Ok(addresses) = (host, port).to_socket_addrs() {
        for address in addresses {
            match address.ip() {
                IpAddr::V4(ip)
                    if !out
                        .iter()
                        .any(|entry: &String| entry == &format!("{ip}/32")) =>
                {
                    out.push(format!("{ip}/32"))
                }
                IpAddr::V6(ip)
                    if !out
                        .iter()
                        .any(|entry: &String| entry == &format!("{ip}/128")) =>
                {
                    out.push(format!("{ip}/128"))
                }
                _ => {}
            }
        }
    }
    out
}
