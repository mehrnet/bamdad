//! Userspace proxy helpers.
//!
//! A helper runs a protocol session without a tun device or VpnService slot
//! and exposes the result as a local SOCKS5 proxy:
//!
//! - `bamdad-wg` (vendored wireproxy) for WireGuard.
//! - `bamdad-ovpn` (OpenVPN 3 + gVisor netstack) later, same CLI contract.
//!
//! The engine supervises a helper exactly like a proxy core and emits the
//! usual lifecycle events.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::binary;
use crate::error::{CoreError, Result};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperKind {
    WireGuard,
    OpenVpn,
}

#[derive(Debug, Clone)]
pub struct HelperSpec {
    pub kind: HelperKind,
    pub config: String,
    pub socks_port: u16,
}

pub struct RunningHelper {
    child: Child,
    socks_port: u16,
    work_dir: PathBuf,
}

impl RunningHelper {
    pub fn socks_port(&self) -> u16 {
        self.socks_port
    }

    /// False once the helper process has exited.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for RunningHelper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

pub fn start(spec: HelperSpec) -> Result<RunningHelper> {
    let binary_path = match spec.kind {
        HelperKind::WireGuard => {
            binary::resolve_helper("BAMDAD_WG_PROXY_BIN", "bamdad-wg", Some("libbamdad_wg.so"))
        }
        HelperKind::OpenVpn => {
            return Err(CoreError::Spawn(
                "the OpenVPN userspace helper is not available yet".to_string(),
            ))
        }
    }
    .ok_or_else(|| {
        CoreError::Spawn(format!(
            "{} helper not found (set BAMDAD_WG_PROXY_BIN)",
            match spec.kind {
                HelperKind::WireGuard => "bamdad-wg",
                HelperKind::OpenVpn => "bamdad-ovpn",
            }
        ))
    })?;

    let work_dir = paths::run_dir().join(format!("helper-{}", crate::config::new_id()));
    std::fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("helper.conf");
    let config = match spec.kind {
        HelperKind::WireGuard => format!(
            "{}\n\n[Socks5]\nBindAddress = 127.0.0.1:{}\n",
            spec.config.trim_end(),
            spec.socks_port
        ),
        HelperKind::OpenVpn => spec.config.clone(),
    };
    write_private(&config_path, &config)?;

    let mut child = Command::new(&binary_path)
        .arg("-c")
        .arg(&config_path)
        .arg("-s")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(CoreError::Io)?;

    let address = SocketAddr::from(([127, 0, 0, 1], spec.socks_port));
    let started = Instant::now();
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(120)).is_ok() {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = std::fs::remove_dir_all(&work_dir);
                return Err(CoreError::Spawn(format!(
                    "{} exited before opening its SOCKS port ({status})",
                    binary_path.display()
                )));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = std::fs::remove_dir_all(&work_dir);
                return Err(CoreError::Io(error));
            }
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&work_dir);
            return Err(CoreError::Spawn(format!(
                "{} did not open its SOCKS port within 10s",
                binary_path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(RunningHelper {
        child,
        socks_port: spec.socks_port,
        work_dir,
    })
}

fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(CoreError::Io)?;
    file.write_all(text.as_bytes()).map_err(CoreError::Io)?;
    Ok(())
}
