use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc::Sender;
use std::time::Instant;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, Duration};

use crate::kind::CoreKind;
use crate::stats::{self, TrafficTotals};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
    App,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    State(EngineState),
    Log {
        stream: LogStream,
        line: String,
    },
    Traffic {
        up_bps: u64,
        down_bps: u64,
        total_up: u64,
        total_down: u64,
    },
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub core: CoreKind,
    pub binary: PathBuf,
    pub config: Value,
    pub assets_dir: PathBuf,
    pub work_dir: PathBuf,
    pub api_addr: String,
    pub api_port: u16,
}

#[derive(Debug)]
enum EngineCommand {
    Start(LaunchSpec),
    Stop,
}

/// Cheap, cloneable handle for the UI. Commands are queued and handled by the
/// engine task; nothing here blocks the Slint event loop.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<EngineCommand>,
}

impl EngineHandle {
    pub fn start(&self, spec: LaunchSpec) {
        let _ = self.tx.send(EngineCommand::Start(spec));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(EngineCommand::Stop);
    }
}

struct Session {
    shutdown: oneshot::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

/// Spawn the engine task. Must be called from inside a Tokio runtime.
pub fn spawn(events: Sender<EngineEvent>) -> EngineHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<EngineCommand>();

    tokio::spawn(async move {
        let mut session: Option<Session> = None;

        loop {
            let mut ended = false;
            let command = if let Some(session) = session.as_mut() {
                tokio::select! {
                    command = rx.recv() => command,
                    _ = &mut session.join => {
                        ended = true;
                        None
                    }
                }
            } else {
                rx.recv().await
            };

            if ended {
                session = None;
                continue;
            }

            match command {
                None => break,
                Some(EngineCommand::Start(spec)) => {
                    if let Some(mut previous) = session.take() {
                        let _ = previous.shutdown.send(());
                        let _ = (&mut previous.join).await;
                    }

                    let (shutdown_tx, shutdown_rx) = oneshot::channel();
                    let event_tx = events.clone();
                    let join = tokio::spawn(run_session(spec, shutdown_rx, event_tx));
                    session = Some(Session {
                        shutdown: shutdown_tx,
                        join,
                    });
                }
                Some(EngineCommand::Stop) => {
                    if let Some(mut current) = session.take() {
                        let _ = current.shutdown.send(());
                        let _ = (&mut current.join).await;
                    } else {
                        send_state(&events, EngineState::Stopped);
                    }
                }
            }
        }
    });

    EngineHandle { tx }
}

enum Exit {
    Shutdown,
    Exited(std::io::Result<std::process::ExitStatus>),
}

async fn run_session(
    spec: LaunchSpec,
    mut shutdown: oneshot::Receiver<()>,
    events: Sender<EngineEvent>,
) {
    send_state(&events, EngineState::Starting);
    log(
        &events,
        LogStream::App,
        format!("starting {}: {}", spec.core.label(), spec.binary.display()),
    );

    // A missing working directory makes `spawn` fail with a confusing ENOENT.
    let _ = std::fs::create_dir_all(&spec.work_dir);
    let _ = std::fs::create_dir_all(&spec.assets_dir);

    let mut command = Command::new(&spec.binary);
    command
        .env("XRAY_LOCATION_ASSET", &spec.assets_dir)
        .current_dir(&spec.work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut config_file: Option<PathBuf> = None;

    match spec.core {
        CoreKind::Xray => {
            command.stdin(Stdio::piped());
        }
        CoreKind::SingBox => {
            let path = spec.work_dir.join(format!(
                "singbox-{}-{}.json",
                std::process::id(),
                crate::config::new_id()
            ));
            if let Err(error) = write_private(&path, &spec.config.to_string()) {
                let message = format!("failed to write sing-box config: {error}");
                log(&events, LogStream::App, message.clone());
                send_state(&events, EngineState::Error(message));
                return;
            }
            command.arg("run").arg("-c").arg(&path).stdin(Stdio::null());
            config_file = Some(path);
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            remove_config(&config_file);
            let message = format!("failed to spawn {}: {error}", spec.core.label());
            log(&events, LogStream::App, message.clone());
            send_state(&events, EngineState::Error(message));
            return;
        }
    };

    // Xray reads its configuration from stdin when no `-c` argument is given.
    if spec.core == CoreKind::Xray {
        if let Some(mut stdin) = child.stdin.take() {
            let config = spec.config.to_string();
            if let Err(error) = stdin.write_all(config.as_bytes()).await {
                log(
                    &events,
                    LogStream::App,
                    format!("failed to write config to core: {error}"),
                );
            }
            let _ = stdin.shutdown().await;
        }
    }

    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(stdout, LogStream::Stdout, events.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(stderr, LogStream::Stderr, events.clone());
    }

    let pid = child.id().unwrap_or_default();
    log(
        &events,
        LogStream::App,
        format!("{} running (pid {pid})", spec.core.label()),
    );
    send_state(&events, EngineState::Running);

    let mut ticker = interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut telemetry = Telemetry::new(&spec);

    let exit = loop {
        tokio::select! {
            _ = &mut shutdown => break Exit::Shutdown,
            status = child.wait() => break Exit::Exited(status),
            _ = ticker.tick() => {
                telemetry.sample(&events).await;
            }
        }
    };

    remove_config(&config_file);

    match exit {
        Exit::Shutdown => {
            send_state(&events, EngineState::Stopping);
            terminate(&mut child).await;
            log(&events, LogStream::App, "core stopped".to_string());
            send_state(&events, EngineState::Stopped);
        }
        Exit::Exited(Ok(status)) => {
            let message = format!("core exited unexpectedly ({status})");
            log(&events, LogStream::App, message.clone());
            send_state(&events, EngineState::Error(message));
        }
        Exit::Exited(Err(error)) => {
            let message = format!("failed while waiting for core: {error}");
            log(&events, LogStream::App, message.clone());
            send_state(&events, EngineState::Error(message));
        }
    }
}

enum Telemetry {
    Xray {
        addr: String,
        port: u16,
        channel: Option<tonic::transport::Channel>,
        last_totals: TrafficTotals,
        last_sample: Instant,
    },
    SingBox {
        addr: String,
        port: u16,
        total_up: u64,
        total_down: u64,
    },
}

impl Telemetry {
    fn new(spec: &LaunchSpec) -> Self {
        match spec.core {
            CoreKind::Xray => Self::Xray {
                addr: spec.api_addr.clone(),
                port: spec.api_port,
                channel: None,
                last_totals: TrafficTotals::default(),
                last_sample: Instant::now(),
            },
            CoreKind::SingBox => Self::SingBox {
                addr: spec.api_addr.clone(),
                port: spec.api_port,
                total_up: 0,
                total_down: 0,
            },
        }
    }

    async fn sample(&mut self, events: &Sender<EngineEvent>) {
        match self {
            Self::Xray {
                addr,
                port,
                channel,
                last_totals,
                last_sample,
            } => {
                if channel.is_none() {
                    *channel = connect_stats(addr, *port).await;
                }
                let Some(api_channel) = channel.clone() else {
                    return;
                };
                let Ok(totals) = stats::query_traffic(api_channel).await else {
                    // The API may not be listening yet, or the core is gone.
                    *channel = None;
                    return;
                };

                let elapsed = last_sample.elapsed().as_secs_f64().max(0.001);
                let up_bps =
                    (totals.uplink.saturating_sub(last_totals.uplink) as f64 / elapsed) as u64;
                let down_bps =
                    (totals.downlink.saturating_sub(last_totals.downlink) as f64 / elapsed) as u64;
                *last_totals = totals;
                *last_sample = Instant::now();

                let _ = events.send(EngineEvent::Traffic {
                    up_bps,
                    down_bps,
                    total_up: totals.uplink,
                    total_down: totals.downlink,
                });
            }
            Self::SingBox {
                addr,
                port,
                total_up,
                total_down,
            } => {
                let Ok((up, down)) = stats::query_clash_traffic(addr, *port).await else {
                    return;
                };
                *total_up = total_up.saturating_add(up);
                *total_down = total_down.saturating_add(down);
                let _ = events.send(EngineEvent::Traffic {
                    up_bps: up,
                    down_bps: down,
                    total_up: *total_up,
                    total_down: *total_down,
                });
            }
        }
    }
}

async fn connect_stats(addr: &str, port: u16) -> Option<tonic::transport::Channel> {
    tokio::time::timeout(Duration::from_millis(400), stats::connect(addr, port))
        .await
        .ok()
        .and_then(Result::ok)
}

async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SIGTERM first so the core can close listeners cleanly.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_ok()
            {
                return;
            }
        }
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn spawn_log_reader<R>(reader: R, stream: LogStream, events: Sender<EngineEvent>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = events.send(EngineEvent::Log { stream, line });
        }
    });
}

fn write_private(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn remove_config(path: &Option<PathBuf>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
    }
}

fn send_state(events: &Sender<EngineEvent>, state: EngineState) {
    let _ = events.send(EngineEvent::State(state));
}

fn log(events: &Sender<EngineEvent>, stream: LogStream, line: String) {
    let _ = events.send(EngineEvent::Log { stream, line });
}
