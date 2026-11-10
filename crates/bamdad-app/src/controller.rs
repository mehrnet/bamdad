use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use bamdad_core::config::{self, load_store, save_store, Store};
use bamdad_core::engine::{EngineEvent, EngineHandle, EngineState, LaunchSpec, LogStream};
use bamdad_core::system_proxy::{self, ProxySettings};
use bamdad_core::{binary, link, paths, probe, CoreKind};

use crate::{AppWindow, ProfileRow};

type LogBuffer = Arc<Mutex<Vec<String>>>;
type PingState = Arc<Mutex<HashMap<String, (String, i32)>>>;

static VPN_SOCKS_PORT: AtomicU16 = AtomicU16::new(10808);

pub fn init(
    ui: &AppWindow,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
    events: Receiver<EngineEvent>,
) {
    let store = Arc::new(Mutex::new(load_store()));
    let logs: LogBuffer = Arc::new(Mutex::new(Vec::new()));
    let pings: PingState = Arc::new(Mutex::new(HashMap::new()));

    seed(ui, &store, &pings);
    setup_callbacks(
        ui,
        engine,
        runtime,
        store.clone(),
        logs.clone(),
        pings.clone(),
    );
    start_size_watch(ui);
    start_bridge(ui.as_weak(), events, logs);

    // Headless visual-test hooks (used by caged Wayland previews).
    if let Ok(page) = std::env::var("BAMDAD_PREVIEW_PAGE") {
        if let Ok(page) = page.parse::<i32>() {
            ui.set_page(page);
        }
    }
    if std::env::var("BAMDAD_PREVIEW_PING").is_ok() {
        ui.invoke_ping_all();
    }
    if std::env::var("BAMDAD_PREVIEW_CONNECT").is_ok() {
        let weak = ui.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(350),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_toggle_connect();
                }
            },
        );
        std::mem::forget(timer);
    }
}

fn seed(ui: &AppWindow, store: &Arc<Mutex<Store>>, pings: &PingState) {
    let store = store.lock().unwrap();

    ui.set_profiles(profile_model(&store, pings));
    ui.set_selected_id(store.selected_id.clone().unwrap_or_default().into());
    ui.set_mobile(cfg!(target_os = "android"));
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());
    ui.set_socks_port(store.settings.socks_port.to_string().into());
    ui.set_http_port(store.settings.http_port.to_string().into());
    ui.set_system_proxy(store.settings.system_proxy);
    ui.set_data_dir(paths::data_dir().display().to_string().into());
    ui.set_xray_version(core_version(CoreKind::Xray).into());
    ui.set_singbox_version(core_version(CoreKind::SingBox).into());
}

fn core_version(kind: CoreKind) -> String {
    match binary::resolve_kind(kind) {
        Some(path) => match binary::version(&path) {
            Ok(version) => format!("{version} · {}", path.display()),
            Err(error) => format!("{} ({error})", path.display()),
        },
        None => "not installed".to_string(),
    }
}

fn profile_model(store: &Store, pings: &PingState) -> ModelRc<ProfileRow> {
    let pings = pings.lock().unwrap();
    let rows: Vec<ProfileRow> = store
        .profiles
        .iter()
        .map(|profile| {
            let (ping, ping_state) = pings
                .get(&profile.id)
                .cloned()
                .unwrap_or_else(|| (initial_ping(profile), 0));
            ProfileRow {
                id: profile.id.as_str().into(),
                name: profile.name.as_str().into(),
                detail: format!(
                    "{} · {}",
                    protocol_label(&profile.protocol),
                    profile.endpoint()
                )
                .into(),
                protocol: protocol_label(&profile.protocol).into(),
                core: profile.core.label().into(),
                ping: ping.into(),
                ping_state,
                selected: store.selected_id.as_deref() == Some(profile.id.as_str()),
            }
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn initial_ping(profile: &config::Profile) -> String {
    if profile.address.is_empty() || profile.address == "local" || profile.port == 0 {
        "—".to_string()
    } else {
        "Ping".to_string()
    }
}

fn protocol_label(protocol: &str) -> String {
    match protocol.to_ascii_lowercase().as_str() {
        "vless" => "VLESS",
        "vmess" => "VMESS",
        "trojan" => "TROJAN",
        "shadowsocks" | "ss" => "SS",
        "direct" => "DIRECT",
        _ => "CUSTOM",
    }
    .to_string()
}

fn setup_callbacks(
    ui: &AppWindow,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
    store: Arc<Mutex<Store>>,
    logs: LogBuffer,
    pings: PingState,
) {
    {
        let weak = ui.as_weak();
        let engine = engine.clone();
        let store = store.clone();
        let logs = logs.clone();
        ui.on_toggle_connect(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_connected() || ui.get_busy() {
                stop_connection(&ui, &store, &engine, &logs);
            } else {
                start_connection(&ui, &store, &engine, &logs);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_select_profile(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let selected = {
                let mut guard = store.lock().unwrap();
                let Some(profile) = guard.profile_by_id(id.as_str()).cloned() else {
                    return;
                };
                guard.selected_id = Some(profile.id.clone());
                guard.selected = Some(profile.name.clone());
                let _ = save_store(&guard);
                profile
            };

            let guard = store.lock().unwrap();
            ui.set_profiles(profile_model(&guard, &pings));
            drop(guard);

            ui.set_selected_id(selected.id.as_str().into());
            push_log(&ui, &logs, format!("selected profile: {}", selected.name));
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_import_link(move |raw, core_choice| {
            let Some(ui) = weak.upgrade() else { return };
            let core = if core_choice == 1 {
                CoreKind::SingBox
            } else {
                CoreKind::Xray
            };

            match link::parse(None, raw.as_str()) {
                Ok(spec) => {
                    let mut core = core;
                    // sing-box has no XHTTP transport; emitting its config would
                    // produce a silent upload-only connection.
                    if matches!(spec.network.as_str(), "xhttp" | "splithttp")
                        && core == CoreKind::SingBox
                    {
                        core = CoreKind::Xray;
                        push_log(
                            &ui,
                            &logs,
                            "XHTTP is only supported by Xray; imported with Xray".to_string(),
                        );
                    }
                    let name = spec.name.clone();
                    let profile = config::Profile::from_spec(&spec, core);
                    {
                        let mut guard = store.lock().unwrap();
                        guard.add_profile(profile);
                        let _ = save_store(&guard);
                    }

                    let guard = store.lock().unwrap();
                    ui.set_profiles(profile_model(&guard, &pings));
                    ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
                    drop(guard);

                    push_log(&ui, &logs, format!("imported {name} for {}", core.label()));
                }
                Err(error) => {
                    push_log(&ui, &logs, format!("import failed: {error}"));
                    ui.set_status_text("Import failed".into());
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let logs = logs.clone();
        ui.on_save_settings(move |socks, http, system_proxy| {
            let Some(ui) = weak.upgrade() else { return };
            let socks_port: u16 = match socks.as_str().parse() {
                Ok(port) => port,
                Err(_) => {
                    push_log(&ui, &logs, "invalid SOCKS port".to_string());
                    return;
                }
            };
            let http_port: u16 = match http.as_str().parse() {
                Ok(port) => port,
                Err(_) => {
                    push_log(&ui, &logs, "invalid HTTP port".to_string());
                    return;
                }
            };

            {
                let mut guard = store.lock().unwrap();
                guard.settings.socks_port = socks_port;
                guard.settings.http_port = http_port;
                guard.settings.system_proxy = system_proxy;
                let _ = save_store(&guard);
            }

            let proxy = ProxySettings {
                socks_port,
                http_port,
            };
            if system_proxy && ui.get_connected() {
                if let Err(error) = system_proxy::enable(proxy) {
                    push_log(&ui, &logs, format!("system proxy: {error}"));
                }
            } else if !system_proxy {
                let _ = system_proxy::disable();
            }

            ui.set_socks_port(socks_port.to_string().into());
            ui.set_http_port(http_port.to_string().into());
            push_log(
                &ui,
                &logs,
                format!("settings saved (socks {socks_port}, http {http_port})"),
            );
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_reload_profiles(move || {
            let Some(ui) = weak.upgrade() else { return };
            let fresh = load_store();
            *store.lock().unwrap() = fresh;
            let guard = store.lock().unwrap();
            ui.set_profiles(profile_model(&guard, &pings));
            ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
            drop(guard);
            push_log(&ui, &logs, "profiles reloaded".to_string());
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_clear_logs(move || {
            let Some(ui) = weak.upgrade() else { return };
            logs.lock().unwrap().clear();
            ui.set_logs(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_test_proxy(move || {
            let weak = weak.clone();
            let logs = logs.clone();
            std::thread::spawn(move || {
                use std::net::{SocketAddr, TcpStream};
                let port = VPN_SOCKS_PORT.load(Ordering::Relaxed);
                let addr = SocketAddr::from(([127, 0, 0, 1], port));
                let message =
                    if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
                        format!("test: SOCKS 127.0.0.1:{port} is not running")
                    } else {
                        match proxy_probe(port) {
                            ProxyProbe::Ok(message)
                            | ProxyProbe::Dns(message)
                            | ProxyProbe::Failed(message) => message,
                        }
                    };
                #[cfg(target_os = "android")]
                crate::logcat(&message);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        push_log(&ui, &logs, message);
                    }
                });
            });
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_copy_logs(move || {
            if let Some(ui) = weak.upgrade() {
                let text = logs.lock().unwrap().join("\n");
                crate::copy_logs(&text);
                push_log(&ui, &logs, "logs copied to the clipboard".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_share_logs(move || {
            if let Some(ui) = weak.upgrade() {
                let text = logs.lock().unwrap().join("\n");
                crate::share_logs(&text);
                push_log(&ui, &logs, "logs ready to share".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let runtime = runtime.clone();
        let store = store.clone();
        let pings = pings.clone();
        ui.on_ping_profile(move |id| {
            begin_ping(&weak, &runtime, &store, &pings, id.to_string());
        });
    }

    {
        let weak = ui.as_weak();
        let runtime = runtime.clone();
        let store = store.clone();
        let pings = pings.clone();
        ui.on_ping_all(move || {
            let ids: Vec<String> = {
                let guard = store.lock().unwrap();
                guard
                    .profiles
                    .iter()
                    .map(|profile| profile.id.clone())
                    .collect()
            };
            for id in ids {
                begin_ping(&weak, &runtime, &store, &pings, id);
            }
        });
    }
}

fn begin_ping(
    weak: &Weak<AppWindow>,
    runtime: &tokio::runtime::Handle,
    store: &Arc<Mutex<Store>>,
    pings: &PingState,
    id: String,
) {
    let Some(ui) = weak.upgrade() else { return };
    let profile = store.lock().unwrap().profile_by_id(&id).cloned();
    let Some(profile) = profile else { return };

    if profile.address.is_empty() || profile.address == "local" || profile.port == 0 {
        set_ping(&ui, pings, &id, "—".to_string(), 0);
        return;
    }

    set_ping(&ui, pings, &id, "…".to_string(), 1);

    let weak = weak.clone();
    let pings = pings.clone();
    let runtime = runtime.clone();
    runtime.spawn(async move {
        let result =
            probe::tcp_latency(&profile.address, profile.port, Duration::from_secs(4)).await;

        let (text, state) = match result {
            Ok(ms) => (format!("{ms} ms"), latency_state(ms)),
            Err(_) => ("timeout".to_string(), 4),
        };

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                set_ping(&ui, &pings, &id, text, state);
            }
        });
    });
}

fn latency_state(ms: u64) -> i32 {
    if ms < 200 {
        2
    } else if ms < 600 {
        3
    } else {
        4
    }
}

fn set_ping(ui: &AppWindow, pings: &PingState, id: &str, text: String, state: i32) {
    pings
        .lock()
        .unwrap()
        .insert(id.to_string(), (text.clone(), state));

    let model = ui.get_profiles();
    for index in 0..model.row_count() {
        if let Some(mut row) = model.row_data(index) {
            if row.id.as_str() == id {
                row.ping = text.into();
                row.ping_state = state;
                model.set_row_data(index, row);
                break;
            }
        }
    }
}

fn start_connection(
    ui: &AppWindow,
    store: &Arc<Mutex<Store>>,
    engine: &EngineHandle,
    logs: &LogBuffer,
) {
    let (settings, profile) = {
        let guard = store.lock().unwrap();
        (guard.settings.clone(), guard.selected_profile().cloned())
    };

    let Some(profile) = profile else {
        push_log(ui, logs, "no profile selected".to_string());
        return;
    };

    let Some(binary_path) = binary::resolve_kind(profile.core) else {
        push_log(
            ui,
            logs,
            format!(
                "{} not found. Run scripts/fetch-{} or place it at {}",
                profile.core.label(),
                if profile.core == CoreKind::Xray {
                    "xray.sh"
                } else {
                    "singbox.sh"
                },
                profile.core.managed_path().display()
            ),
        );
        ui.set_status_text(format!("{} missing", profile.core.label()).into());
        return;
    };

    let api_port = match config::free_port() {
        Ok(port) => port,
        Err(error) => {
            push_log(ui, logs, format!("could not reserve API port: {error}"));
            return;
        }
    };

    let prepared = match config::prepare_config(profile.core, &profile.config, &settings, api_port)
    {
        Ok(config) => config,
        Err(error) => {
            push_log(ui, logs, format!("invalid config: {error}"));
            return;
        }
    };

    if let Err(error) = paths::ensure_layout() {
        push_log(
            ui,
            logs,
            format!("could not create data directory: {error}"),
        );
        return;
    }

    VPN_SOCKS_PORT.store(settings.socks_port, Ordering::Relaxed);

    let spec = LaunchSpec {
        core: profile.core,
        binary: binary_path,
        config: prepared,
        assets_dir: binary::asset_dir(),
        work_dir: paths::run_dir(),
        api_addr: settings.api_addr.clone(),
        api_port,
    };

    engine.start(spec);
    ui.set_busy(true);
    ui.set_status_text("Starting…".into());
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());

    if settings.system_proxy && !cfg!(target_os = "android") {
        let proxy = ProxySettings {
            socks_port: settings.socks_port,
            http_port: settings.http_port,
        };
        if let Err(error) = system_proxy::enable(proxy) {
            push_log(ui, logs, format!("system proxy: {error}"));
        }
    }
}

fn stop_connection(
    ui: &AppWindow,
    store: &Arc<Mutex<Store>>,
    engine: &EngineHandle,
    logs: &LogBuffer,
) {
    engine.stop();
    crate::stop_android_vpn();
    ui.set_busy(true);
    ui.set_status_text("Stopping…".into());

    let system_proxy_enabled = store.lock().unwrap().settings.system_proxy;
    if system_proxy_enabled {
        if let Err(error) = system_proxy::disable() {
            push_log(ui, logs, format!("could not restore system proxy: {error}"));
        }
    }
}

fn start_bridge(weak: Weak<AppWindow>, events: Receiver<EngineEvent>, logs: LogBuffer) {
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            let weak = weak.clone();
            let logs = logs.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    apply_event(&ui, &logs, event);
                }
            });
        }
    });
}

enum ProxyProbe {
    Ok(String),
    Dns(String),
    Failed(String),
}

/// Verify the running core end to end. A SOCKS `CONNECT` to port 443 is used
/// because many providers block plain HTTP; the IP fallback separates a remote
/// DNS problem from a dead proxy.
fn proxy_probe(port: u16) -> ProxyProbe {
    use bamdad_core::socks::open_through;

    let mut last_error = String::new();
    for attempt in 0..2 {
        match open_through(port, "example.com", 443, Duration::from_secs(15)) {
            Ok(_) => {
                return ProxyProbe::Ok(format!(
                    "proxy check: SOCKS CONNECT example.com:443 ok via 127.0.0.1:{port}"
                ));
            }
            Err(error) => {
                last_error = error.to_string();
                if attempt == 0 {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    match open_through(port, "1.1.1.1", 443, Duration::from_secs(10)) {
        Ok(_) => ProxyProbe::Dns(format!(
            "proxy check: example.com:443 failed ({last_error}), but 1.1.1.1:443 works — remote DNS/domain issue"
        )),
        Err(ip_error) => ProxyProbe::Failed(format!(
            "proxy check FAILED via SOCKS 127.0.0.1:{port}: {last_error}; 1.1.1.1 fallback: {ip_error}"
        )),
    }
}

fn apply_event(ui: &AppWindow, logs: &LogBuffer, event: EngineEvent) {
    match event {
        EngineEvent::State(state) => match state {
            EngineState::Stopped => {
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Disconnected".into());
                ui.set_total_down_text("0 B".into());
                ui.set_total_up_text("0 B".into());
                crate::stop_android_vpn();
                let _ = system_proxy::disable();
            }
            EngineState::Starting => {
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Starting…".into());
                ui.set_total_down_text("0 B".into());
                ui.set_total_up_text("0 B".into());
            }
            EngineState::Running => {
                ui.set_connected(true);
                ui.set_busy(false);
                ui.set_status_text("Connected".into());
                let weak = ui.as_weak();
                let logs = logs.clone();
                std::thread::spawn(move || {
                    use std::net::{SocketAddr, TcpStream};
                    let port = VPN_SOCKS_PORT.load(Ordering::Relaxed);
                    let addr = SocketAddr::from(([127, 0, 0, 1], port));
                    let mut ready = false;
                    for _ in 0..50 {
                        if TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok() {
                            ready = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }

                    #[cfg(target_os = "android")]
                    if ready {
                        crate::start_android_vpn(port);
                    }

                    let (message, status) = if !ready {
                        (
                            format!("proxy check: SOCKS 127.0.0.1:{port} never opened"),
                            Some("Proxy unreachable"),
                        )
                    } else {
                        match proxy_probe(port) {
                            ProxyProbe::Ok(message) => (message, None),
                            ProxyProbe::Dns(message) => (message, Some("Proxy DNS issue")),
                            ProxyProbe::Failed(message) => (message, Some("Proxy unreachable")),
                        }
                    };

                    #[cfg(target_os = "android")]
                    crate::logcat(&message);
                    let weak_for_log = weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_for_log.upgrade() {
                            push_log(&ui, &logs, message);
                            if let Some(status) = status {
                                if ui.get_connected() {
                                    ui.set_status_text(status.into());
                                }
                            }
                        }
                    });
                });
            }
            EngineState::Stopping => {
                ui.set_busy(true);
                ui.set_status_text("Stopping…".into());
                crate::stop_android_vpn();
            }
            EngineState::Error(message) => {
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Core error".into());
                crate::stop_android_vpn();
                let _ = system_proxy::disable();
                push_log(ui, logs, message);
            }
        },
        EngineEvent::Log { stream, line } => {
            let prefix = match stream {
                LogStream::Stdout => "[core] ",
                LogStream::Stderr => "[err ] ",
                LogStream::App => "[app ] ",
            };
            push_log(ui, logs, format!("{prefix}{line}"));
        }
        EngineEvent::Traffic {
            total_up,
            total_down,
            ..
        } => {
            ui.set_total_down_text(format_bytes(total_down).into());
            ui.set_total_up_text(format_bytes(total_up).into());
        }
    }
}

fn update_breakpoints(ui: &AppWindow) {
    let window = ui.window();
    let scale = window.scale_factor().max(0.1);
    let width = window.size().width as f32 / scale;
    // Before the first frame the window can report 0; keep the defaults.
    if width < 100.0 {
        return;
    }
    let phone = width < 700.0;
    let tablet = width >= 700.0 && width < 1024.0;
    if ui.get_phone() != phone {
        ui.set_phone(phone);
    }
    if ui.get_tablet() != tablet {
        ui.set_tablet(tablet);
    }
}

fn start_size_watch(ui: &AppWindow) {
    update_breakpoints(ui);
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(80),
        move || {
            if let Some(ui) = weak.upgrade() {
                update_breakpoints(&ui);
            }
        },
    );
    std::mem::forget(timer);
}

fn push_log(ui: &AppWindow, logs: &LogBuffer, line: impl Into<String>) {
    let mut buffer = logs.lock().unwrap();
    buffer.push(line.into());
    if buffer.len() > 600 {
        let excess = buffer.len() - 600;
        buffer.drain(0..excess);
    }
    let items: Vec<SharedString> = buffer
        .iter()
        .map(|line| SharedString::from(line.as_str()))
        .collect();
    ui.set_logs(ModelRc::new(VecModel::from(items)));
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
