//! HTTP latency probing through a tunnel.
//!
//! One prober is used everywhere a tunnel exists: the active proxy (its SOCKS
//! listener), the active native tunnel (this process, straight through the
//! VPN), and every stored proxy config (a temporary or pooled core). Each
//! measurement warms the path with one request, takes three timed requests on
//! the warm connection, and reports the lowest TTFB.
//!
//! Batch probes (the ping button on a group or "ping all") merge many profiles
//! into one core instance with one SOCKS inbound/outbound pair per profile,
//! like v2rayNG and radar-node's pooled xray jobs, so hundreds of configs can
//! be measured without paying one process start each.
//!
//! Core startup is excluded from each measurement.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config;
use crate::error::{CoreError, Result};
use crate::{binary, paths, CoreKind};

/// The same cheap, globally available target Android uses for captive-portal
/// checks.
pub const TARGET: &str = "http://www.gstatic.com/generate_204";

/// Profiles merged into a single core instance per batch.
const CHUNK: usize = 128;
/// Concurrent HTTP requests within a batch.
const REQUESTS: usize = 64;
/// Timed samples taken after the warm-up request.
const SAMPLES: usize = 3;
/// Never let a single sample eat the whole budget.
const SAMPLE_CAP: Duration = Duration::from_millis(1_500);

pub struct BatchJob {
    pub id: String,
    pub core: CoreKind,
    pub config: Value,
}

pub struct BatchResult {
    pub id: String,
    pub latency_ms: Option<u64>,
}

/// Probe a whole batch. Jobs are grouped by core and chunked so one merged
/// instance never grows unbounded.
pub fn measure_batch(jobs: Vec<BatchJob>, timeout: Duration) -> Vec<BatchResult> {
    let mut results = Vec::with_capacity(jobs.len());
    for core in [CoreKind::Xray, CoreKind::SingBox] {
        let group: Vec<BatchJob> = jobs
            .iter()
            .filter(|job| job.core == core)
            .map(|job| BatchJob {
                id: job.id.clone(),
                core: job.core,
                config: job.config.clone(),
            })
            .collect();
        for chunk in group.chunks(CHUNK) {
            let chunk: Vec<BatchJob> = chunk
                .iter()
                .map(|job| BatchJob {
                    id: job.id.clone(),
                    core: job.core,
                    config: job.config.clone(),
                })
                .collect();
            results.extend(measure_chunk(core, chunk, timeout));
        }
    }
    results
}

fn measure_chunk(core: CoreKind, jobs: Vec<BatchJob>, timeout: Duration) -> Vec<BatchResult> {
    let failure = |jobs: &[BatchJob]| -> Vec<BatchResult> {
        jobs.iter()
            .map(|job| BatchResult {
                id: job.id.clone(),
                latency_ms: None,
            })
            .collect()
    };

    let Some(binary_path) = binary::resolve_kind(core) else {
        return failure(&jobs);
    };
    let ports = match reserve_ports(jobs.len()) {
        Ok(ports) => ports,
        Err(_) => return failure(&jobs),
    };
    let config = match merged_config(core, &jobs, &ports) {
        Ok(config) => config,
        Err(_) => return failure(&jobs),
    };

    let work = paths::run_dir().join(format!("probe-{}", config::new_id()));
    if std::fs::create_dir_all(&work).is_err() {
        return failure(&jobs);
    }
    let child = match spawn_raw(core, &binary_path, &config, &work) {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&work);
            return failure(&jobs);
        }
    };
    let _guard = KillOnDrop(child);

    let results = if wait_for_ports(&ports, timeout.min(Duration::from_secs(8))) {
        request_all(&ports, timeout)
    } else {
        vec![None; ports.len()]
    };

    let _ = std::fs::remove_dir_all(&work);
    jobs.iter()
        .zip(results)
        .map(|(job, latency_ms)| BatchResult {
            id: job.id.clone(),
            latency_ms,
        })
        .collect()
}

fn request_all(ports: &[u16], timeout: Duration) -> Vec<Option<u64>> {
    let results: Vec<Mutex<Option<u64>>> = (0..ports.len()).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let workers = ports.len().min(REQUESTS);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= ports.len() {
                    break;
                }
                let latency = request(ports[index], timeout).ok();
                if let Ok(mut slot) = results[index].lock() {
                    *slot = latency;
                }
            });
        }
    });

    results
        .into_iter()
        .map(|slot| slot.into_inner().unwrap_or(None))
        .collect()
}

fn reserve_ports(count: usize) -> Result<Vec<u16>> {
    let mut listeners = Vec::with_capacity(count);
    for _ in 0..count {
        listeners.push(TcpListener::bind("127.0.0.1:0").map_err(CoreError::Io)?);
    }
    let ports = listeners
        .iter()
        .map(|listener| {
            listener
                .local_addr()
                .map(|addr| addr.port())
                .map_err(CoreError::Io)
        })
        .collect::<Result<Vec<u16>>>()?;
    drop(listeners);
    Ok(ports)
}

fn primary_outbound(core: CoreKind, config: &Value) -> Option<Value> {
    let outbounds = config.get("outbounds")?.as_array()?;
    let bypass = |outbound: &&Value| {
        let tag = outbound
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let protocol = outbound
            .get("protocol")
            .or_else(|| outbound.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            protocol.as_str(),
            "freedom" | "blackhole" | "dns" | "direct" | "block"
        ) || matches!(tag.as_str(), "direct" | "block" | "dns")
    };
    let _ = core;
    outbounds
        .iter()
        .find(|outbound| !bypass(outbound))
        .or_else(|| outbounds.first())
        .cloned()
}

fn merged_config(core: CoreKind, jobs: &[BatchJob], ports: &[u16]) -> Result<Value> {
    let mut inbounds = Vec::with_capacity(jobs.len());
    let mut outbounds = Vec::with_capacity(jobs.len());
    let mut rules = Vec::with_capacity(jobs.len());

    for (index, job) in jobs.iter().enumerate() {
        let mut outbound = primary_outbound(core, &job.config)
            .ok_or_else(|| CoreError::Spawn(format!("probe {} has no outbound", job.id)))?;
        let out_tag = format!("probe-out-{index}");
        outbound["tag"] = json!(out_tag);
        // Mux keeps a warm connection to the proxy server between samples and
        // across configs, which is what makes warm TTFB meaningful. Not used
        // with XHTTP (own multiplexing) or REALITY (known failures).
        let stream = outbound.get("streamSettings");
        let network = stream
            .and_then(|stream| stream.get("network"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let security = stream
            .and_then(|stream| stream.get("security"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let protocol = outbound
            .get("protocol")
            .or_else(|| outbound.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let bypass = matches!(
            protocol,
            "freedom" | "blackhole" | "direct" | "block" | "dns"
        );
        if core == CoreKind::Xray
            && !bypass
            && !network.eq_ignore_ascii_case("xhttp")
            && !security.eq_ignore_ascii_case("reality")
        {
            outbound["mux"] = json!({ "enabled": true, "concurrency": 8 });
        }
        outbounds.push(outbound);

        let in_tag = format!("probe-in-{index}");
        let port = ports[index];
        match core {
            CoreKind::Xray => {
                inbounds.push(json!({
                    "tag": in_tag,
                    "listen": "127.0.0.1",
                    "port": port,
                    "protocol": "socks",
                    "settings": { "auth": "noauth", "udp": false }
                }));
                rules.push(json!({
                    "type": "field",
                    "inboundTag": [in_tag],
                    "outboundTag": out_tag
                }));
            }
            CoreKind::SingBox => {
                inbounds.push(json!({
                    "type": "socks",
                    "tag": in_tag,
                    "listen": "127.0.0.1",
                    "listen_port": port
                }));
                rules.push(json!({
                    "inbound": [in_tag],
                    "outbound": out_tag
                }));
            }
        }
    }

    match core {
        CoreKind::Xray => Ok(json!({
            "log": { "loglevel": "warning" },
            "inbounds": inbounds,
            "outbounds": outbounds,
            "routing": {
                "domainStrategy": "IPIfNonMatch",
                "rules": rules
            }
        })),
        CoreKind::SingBox => Ok(json!({
            "log": { "level": "warn", "timestamp": false },
            "inbounds": inbounds,
            "outbounds": outbounds,
            "route": {
                "rules": rules,
                "final": "probe-out-0"
            }
        })),
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_raw(core: CoreKind, binary_path: &Path, config: &Value, work: &Path) -> Result<Child> {
    let mut command = Command::new(binary_path);
    command
        .env("XRAY_LOCATION_ASSET", binary::asset_dir())
        .current_dir(work)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match core {
        CoreKind::Xray => {
            command.stdin(Stdio::piped());
        }
        CoreKind::SingBox => {
            let path = work.join("config.json");
            std::fs::write(&path, config.to_string()).map_err(CoreError::Io)?;
            command.arg("run").arg("-c").arg(&path).stdin(Stdio::null());
        }
    }

    let mut child = command.spawn().map_err(CoreError::Io)?;
    if core == CoreKind::Xray {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.to_string().as_bytes())
                .map_err(CoreError::Io)?;
        }
    }
    Ok(child)
}

fn wait_for_ports(ports: &[u16], timeout: Duration) -> bool {
    let start = Instant::now();
    let mut pending: Vec<u16> = ports.to_vec();
    loop {
        pending.retain(|port| {
            TcpStream::connect_timeout(
                &SocketAddr::from(([127, 0, 0, 1], *port)),
                Duration::from_millis(80),
            )
            .is_err()
        });
        if pending.is_empty() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Where a probe has to travel to reach the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Through a local SOCKS listener: a temporary core or the active proxy.
    Socks(u16),
    /// Straight from this process: the active native tunnel.
    Direct,
}

/// The unified latency prober.
///
/// Warms the path with one request (connection/DNS/TLS setup, excluded from
/// the result), then takes [SAMPLES] timed requests on the warm connection and
/// returns the lowest TTFB. The whole call is bounded by `timeout`.
pub fn measure(route: Route, timeout: Duration) -> Result<u64> {
    let proxy = match route {
        Route::Socks(port) => Some(port),
        Route::Direct => None,
    };
    best_ttfb(&agent(proxy)?, timeout)
}

fn request(socks_port: u16, timeout: Duration) -> Result<u64> {
    measure(Route::Socks(socks_port), timeout)
}

fn agent(proxy: Option<u16>) -> Result<ureq::Agent> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_secs(5));
    if let Some(port) = proxy {
        let proxy = ureq::Proxy::new(format!("socks5://127.0.0.1:{port}"))
            .map_err(|error| CoreError::Spawn(format!("invalid probe proxy: {error}")))?;
        builder = builder.proxy(proxy);
    }
    Ok(builder.build())
}

/// One warm-up request, then [SAMPLES] timed requests on the same connection.
/// The whole loop is bounded by `timeout`, so a dead profile costs one budget,
/// not one per sample.
fn best_ttfb(agent: &ureq::Agent, timeout: Duration) -> Result<u64> {
    let deadline = Instant::now() + timeout;
    // The first request pays connection, DNS and TLS setup. If it fails,
    // retrying only burns the budget, so report a failure immediately.
    let warm = match sample(agent, remaining(&deadline)) {
        Some(ms) => ms,
        None => {
            return Err(CoreError::Spawn("probe request failed".to_string()));
        }
    };

    let mut best = None;
    for _ in 0..SAMPLES {
        let left = remaining(&deadline).min(SAMPLE_CAP);
        if left < Duration::from_millis(50) {
            break;
        }
        if let Some(ms) = sample(agent, left) {
            best = Some(best.map_or(ms, |best: u64| best.min(ms)));
        }
    }
    // A tunnel that answered the warm-up once but not any sample still has a
    // number: report the warm-up instead of a timeout.
    Ok(best.unwrap_or(warm).max(1))
}

fn remaining(deadline: &Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn sample(agent: &ureq::Agent, timeout: Duration) -> Option<u64> {
    let started = Instant::now();
    let request = agent.get(TARGET).timeout(timeout);
    let response = request.call().ok()?;
    let status = response.status();
    // Consume the (empty) body so ureq can return the connection to its pool
    // and the next sample reuses it.
    let _ = response.into_string();
    if status >= 400 {
        return None;
    }
    Some(started.elapsed().as_millis() as u64)
}
