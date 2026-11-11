use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::net::TcpStream as TokioTcpStream;

use crate::config::{self, AppSettings};
use crate::error::{CoreError, Result};
use crate::{binary, paths, CoreKind};

/// Transport-level latency shared by every protocol.
///
/// All supported protocols (VLESS, VMess, Trojan, Shadowsocks, and their
/// TLS/REALITY/WS/gRPC variants) use a TCP endpoint, so a timed TCP handshake
/// is a meaningful, protocol-agnostic "ping". It intentionally does not send
/// credentials, so it is safe to run for every saved profile.
pub async fn tcp_latency(host: &str, port: u16, timeout: Duration) -> Result<u64> {
    if host.trim().is_empty() || port == 0 {
        return Err(CoreError::InvalidLink(
            "profile has no reachable endpoint".to_string(),
        ));
    }

    let start = Instant::now();
    let connect = TokioTcpStream::connect((host, port));
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(start.elapsed().as_millis() as u64)
        }
        Ok(Err(error)) => Err(CoreError::Io(error)),
        Err(_) => Err(CoreError::Spawn(format!(
            "timed out after {} ms",
            timeout.as_millis()
        ))),
    }
}

const PROBE_TARGETS: &[&str] = &[
    "https://www.google.com/generate_204",
    "https://www.gstatic.com/generate_204",
    "https://cp.cloudflare.com/generate_204",
];

struct CoreProcess {
    child: Child,
    config_file: Option<PathBuf>,
}

impl Drop for CoreProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(path) = self.config_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn launch_core(
    core: CoreKind,
    binary: &Path,
    config: &Value,
    assets: &Path,
    work_dir: &Path,
) -> Result<CoreProcess> {
    let _ = std::fs::create_dir_all(work_dir);
    let _ = std::fs::create_dir_all(assets);

    let mut command = Command::new(binary);
    command
        .env("XRAY_LOCATION_ASSET", assets)
        .current_dir(work_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Make sure a probe core cannot outlive the app if the process is killed
    // while a ping is still running.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }

    let (child, config_file) = match core {
        CoreKind::Xray => {
            command.stdin(Stdio::piped());
            let mut child = command.spawn().map_err(CoreError::Io)?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(config.to_string().as_bytes())
                    .map_err(CoreError::Io)?;
                let _ = stdin.flush();
            }
            (child, None)
        }
        CoreKind::SingBox => {
            let path = work_dir.join(format!(
                "probe-singbox-{}-{}.json",
                std::process::id(),
                config::new_id()
            ));
            std::fs::write(&path, config.to_string())
                .map_err(|error| CoreError::Spawn(format!("probe config: {error}")))?;
            command.arg("run").arg("-c").arg(&path).stdin(Stdio::null());
            let child = command.spawn().map_err(CoreError::Io)?;
            (child, Some(path))
        }
    };

    Ok(CoreProcess { child, config_file })
}

fn wait_for_port(port: u16, deadline: Instant) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(120)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Probe a profile's real end-to-end latency through a temporary core instance.
///
/// A raw TCP handshake only proves that the proxy server's port is reachable; it
/// does not prove that the protocol, credentials, or transport actually work.
/// This launches the profile's own core on disposable local ports, warms up a
/// pooled HTTPS connection through its HTTP inbound, then takes the best of
/// three requests, mirroring the radar-node xray probe. The temporary core is
/// killed before returning.
pub fn tunnel_latency(
    core: CoreKind,
    config: &Value,
    settings: &AppSettings,
    timeout: Duration,
) -> Result<u64> {
    let Some(binary) = binary::resolve_kind(core) else {
        return Err(CoreError::Spawn(format!(
            "{} is not installed",
            core.label()
        )));
    };

    let socks_port = config::free_port()?;
    let http_port = config::free_port()?;
    let api_port = config::free_port()?;
    let mut probe_settings = settings.clone();
    probe_settings.socks_port = socks_port;
    probe_settings.http_port = http_port;
    probe_settings.system_proxy = false;

    let prepared = config::prepare_config(core, config, &probe_settings, api_port)?;
    let _process = launch_core(
        core,
        &binary,
        &prepared,
        &binary::asset_dir(),
        &paths::run_dir(),
    )?;

    let started = Instant::now();
    let startup_deadline = started + timeout.min(Duration::from_millis(4_000));
    if !wait_for_port(http_port, startup_deadline) {
        return Err(CoreError::Spawn(format!(
            "{} did not open its HTTP inbound",
            core.label()
        )));
    }

    let proxy = ureq::Proxy::new(format!("http://127.0.0.1:{http_port}"))
        .map_err(|error| CoreError::Spawn(format!("probe proxy: {error}")))?;
    let agent = ureq::AgentBuilder::new()
        .proxy(proxy)
        .timeout(Duration::from_millis(2_000))
        .build();

    // Pre-warm: find a target that answers and prime the pooled TLS
    // connection. The warm-up request itself is not part of the result.
    let mut target: Option<&'static str> = None;
    for candidate in PROBE_TARGETS {
        if fetch_204(&agent, candidate) {
            target = Some(candidate);
            break;
        }
    }
    let Some(mut target) = target else {
        return Err(CoreError::Spawn("probe request failed".to_string()));
    };

    // Best of 3 on the warmed connection. Taking the minimum removes the
    // occasional slow packet without hiding a consistently slow tunnel.
    // Fire the three samples concurrently over the warmed connection pool and
    // keep the best one. This gives the same best-of-three result without
    // paying three sequential round trips.
    let mut best = measure_best_of_three(&agent, target);
    if best.is_none() {
        for candidate in PROBE_TARGETS {
            if *candidate == target {
                continue;
            }
            let started = Instant::now();
            if fetch_204(&agent, candidate) {
                target = candidate;
                let _ = started;
                best = measure_best_of_three(&agent, target);
                if best.is_some() {
                    break;
                }
            }
        }
    }

    best.map(Ok)
        .unwrap_or_else(|| Err(CoreError::Spawn("probe request failed".to_string())))
}

fn measure_best_of_three(agent: &ureq::Agent, target: &'static str) -> Option<u64> {
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(3);
        for _ in 0..3 {
            let agent = agent.clone();
            handles.push(scope.spawn(move || {
                let started = Instant::now();
                if fetch_204(&agent, target) {
                    Some(started.elapsed().as_millis() as u64)
                } else {
                    None
                }
            }));
        }
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .min()
    })
}

fn fetch_204(agent: &ureq::Agent, url: &str) -> bool {
    match agent.get(url).call() {
        Ok(response) => (200..400).contains(&response.status()),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_a_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let latency = tcp_latency("127.0.0.1", port, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(latency < 1_000);
    }

    #[tokio::test]
    async fn rejects_empty_endpoints() {
        assert!(tcp_latency("", 0, Duration::from_millis(50)).await.is_err());
    }
}
