//! One-shot HTTP probe through a proxy core.
//!
//! Mirrors radar-node's approach: start the profile's own core on temporary
//! ports, fetch a tiny `generate_204` target through its SOCKS listener, and
//! report the request time. Core startup is excluded from the measurement.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::config::{self, AppSettings};
use crate::error::{CoreError, Result};
use crate::{binary, paths, CoreKind};

/// The same cheap, globally available target Android uses for captive-portal
/// checks.
pub const TARGET: &str = "https://www.gstatic.com/generate_204";

pub struct HttpProbe {
    pub core: CoreKind,
    pub binary: PathBuf,
    pub config: Value,
    pub settings: AppSettings,
}

pub fn measure(probe: HttpProbe, timeout: Duration) -> Result<u64> {
    let socks_port = config::free_port()?;
    let http_port = config::free_port()?;
    let api_port = config::free_port()?;

    let mut settings = probe.settings.clone();
    settings.socks_port = socks_port;
    settings.http_port = http_port;

    let prepared = config::prepare_config(probe.core, &probe.config, &settings, api_port)?;
    let work = paths::run_dir().join(format!("probe-{}", config::new_id()));
    std::fs::create_dir_all(&work)?;

    let child = match spawn(&probe, &prepared, &work) {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(error);
        }
    };
    let _guard = KillOnDrop(child);

    let result = if wait_for_port(socks_port, timeout / 2) {
        request(socks_port, timeout)
    } else {
        Err(CoreError::Spawn(
            "probe core did not open its SOCKS port".to_string(),
        ))
    };

    let _ = std::fs::remove_dir_all(&work);
    result
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(probe: &HttpProbe, config: &Value, work: &Path) -> Result<Child> {
    let mut command = Command::new(&probe.binary);
    command
        .env("XRAY_LOCATION_ASSET", binary::asset_dir())
        .current_dir(work)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match probe.core {
        CoreKind::Xray => {
            command.stdin(Stdio::piped());
        }
        CoreKind::SingBox => {
            let path = work.join("config.json");
            std::fs::write(&path, config.to_string())?;
            command.arg("run").arg("-c").arg(&path).stdin(Stdio::null());
        }
    }

    let mut child = command.spawn().map_err(CoreError::Io)?;
    if probe.core == CoreKind::Xray {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.to_string().as_bytes())
                .map_err(CoreError::Io)?;
        }
    }
    Ok(child)
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let start = Instant::now();
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Fetch the probe target through an already-running SOCKS listener.
pub fn measure_through(socks_port: u16, timeout: Duration) -> Result<u64> {
    request(socks_port, timeout)
}

/// Fetch the probe target directly from this process. Used to check the
/// active native tunnel end to end.
pub fn measure_direct(timeout: Duration) -> Result<u64> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(timeout)
        .build();
    let start = Instant::now();
    let response = agent
        .get(TARGET)
        .call()
        .map_err(|error| CoreError::Spawn(format!("probe request failed: {error}")))?;
    if response.status() != 204 {
        return Err(CoreError::Spawn(format!(
            "probe returned HTTP {}",
            response.status()
        )));
    }
    Ok(start.elapsed().as_millis() as u64)
}

fn request(socks_port: u16, timeout: Duration) -> Result<u64> {
    let proxy = ureq::Proxy::new(format!("socks5://127.0.0.1:{socks_port}"))
        .map_err(|error| CoreError::Spawn(format!("invalid probe proxy: {error}")))?;
    let agent = ureq::AgentBuilder::new()
        .proxy(proxy)
        .timeout_connect(Duration::from_secs(5))
        .timeout(timeout)
        .build();

    let start = Instant::now();
    let response = agent
        .get(TARGET)
        .call()
        .map_err(|error| CoreError::Spawn(format!("probe request failed: {error}")))?;
    let status = response.status();
    if status != 204 {
        return Err(CoreError::Spawn(format!("probe returned HTTP {status}")));
    }
    Ok(start.elapsed().as_millis() as u64)
}
