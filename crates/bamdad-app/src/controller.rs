use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use bamdad_core::config::{self, load_store, save_store, Store};
use bamdad_core::engine::{EngineEvent, EngineHandle, EngineState, LaunchSpec, LogStream};
use bamdad_core::system_proxy::{self, ProxySettings};
use bamdad_core::{binary, link, paths, probe, CoreKind};

use crate::{AppWindow, NamePart, ProfileRow};

pub(crate) type LogBuffer = Arc<Mutex<Vec<String>>>;
type PingState = Arc<Mutex<HashMap<String, (String, i32)>>>;

static VPN_SOCKS_PORT: AtomicU16 = AtomicU16::new(10808);
static SWITCHING: AtomicBool = AtomicBool::new(false);

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

/// Shared state that survives activity destruction on Android.
#[derive(Clone)]
pub(crate) struct SessionSnapshot {
    pub state: EngineState,
    pub total_up: u64,
    pub total_down: u64,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            state: EngineState::Stopped,
            total_up: 0,
            total_down: 0,
        }
    }
}

/// Attach a (new) UI to an already running Android engine session.
pub(crate) fn init_attached(
    ui: &AppWindow,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
    logs: LogBuffer,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    events: Receiver<EngineEvent>,
) {
    let store = Arc::new(Mutex::new(load_store()));
    let pings: PingState = Arc::new(Mutex::new(HashMap::new()));

    seed(ui, &store, &pings);
    {
        let items: Vec<SharedString> = logs
            .lock()
            .unwrap()
            .iter()
            .map(|line| SharedString::from(line.as_str()))
            .collect();
        ui.set_logs(ModelRc::new(VecModel::from(items)));
    }
    {
        let snapshot = snapshot.lock().unwrap();
        ui.set_total_up_text(format_bytes(snapshot.total_up).into());
        ui.set_total_down_text(format_bytes(snapshot.total_down).into());
        match &snapshot.state {
            EngineState::Running => {
                SWITCHING.store(false, Ordering::SeqCst);
                ui.set_connected(true);
                ui.set_busy(false);
                ui.set_status_text("Connected".into());
            }
            EngineState::Starting => {
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Starting…".into());
            }
            EngineState::Stopping => {
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Stopping…".into());
            }
            EngineState::Error(_) => {
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Core error".into());
            }
            EngineState::Stopped => {
                if SWITCHING.load(Ordering::SeqCst) {
                    return;
                }
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Disconnected".into());
            }
        }
    }

    setup_callbacks(ui, engine, runtime, store, logs.clone(), pings.clone());
    start_size_watch(ui);
    start_bridge(ui.as_weak(), events, logs);
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

static EMOJI_ZIP: &[u8] = include_bytes!("../assets/twemoji.zip");

thread_local! {
    static EMOJI_CACHE: RefCell<HashMap<String, Option<slint::Image>>> =
        RefCell::new(HashMap::new());
}

fn emoji_candidates(grapheme: &str) -> Vec<String> {
    let codepoints: Vec<u32> = grapheme.chars().map(|c| c as u32).collect();
    let full = codepoints
        .iter()
        .map(|c| format!("{c:x}"))
        .collect::<Vec<_>>()
        .join("-");
    let no_variation = codepoints
        .iter()
        .filter(|&&c| c != 0xFE0F)
        .map(|c| format!("{c:x}"))
        .collect::<Vec<_>>()
        .join("-");
    let first = codepoints
        .first()
        .map(|c| format!("{c:x}"))
        .unwrap_or_default();

    let mut candidates = vec![full];
    if no_variation != candidates[0] {
        candidates.push(no_variation);
    }
    if first != candidates[0] {
        candidates.push(first);
    }
    candidates
}

fn emoji_image(grapheme: &str) -> Option<slint::Image> {
    EMOJI_CACHE.with(|cache| {
        if let Some(cached) = cache.borrow().get(grapheme) {
            return cached.clone();
        }

        let mut result = None;
        for candidate in emoji_candidates(grapheme) {
            let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(EMOJI_ZIP)) else {
                break;
            };
            let Ok(mut file) = archive.by_name(&format!("{candidate}.svg")) else {
                continue;
            };
            let mut data = Vec::new();
            if std::io::Read::read_to_end(&mut file, &mut data).is_err() {
                continue;
            }
            if let Ok(image) = slint::Image::load_from_svg_data(&data) {
                result = Some(image);
                break;
            }
        }

        cache
            .borrow_mut()
            .insert(grapheme.to_string(), result.clone());
        result
    })
}

/// Fold decorative Unicode (mathematical alphanumerics, fullwidth, super- and
/// subscripts) to ASCII for display. System fonts often lack these glyphs and
/// show boxes. Other scripts are left untouched.
fn display_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let mut out = String::new();
    for ch in name.chars() {
        let code = ch as u32;
        if (0x1D400..=0x1D7FF).contains(&code)
            || (0x2070..=0x209F).contains(&code)
            || (0x2100..=0x214F).contains(&code)
            || (0x00B2..=0x00B3).contains(&code)
            || code == 0x00B9
            || (0xFF00..=0xFFEF).contains(&code)
        {
            // NFKD maps math/fullwidth letters and super/subscripts to ASCII.
            out.extend(ch.to_string().nfkd());
        } else {
            out.push(ch);
        }
    }
    out
}

fn name_parts(name: &str) -> Vec<NamePart> {
    let name = display_name(name);
    let name = name.as_str();
    use unicode_segmentation::UnicodeSegmentation;

    let mut parts = Vec::new();
    let mut text = String::new();

    let push_text = |parts: &mut Vec<NamePart>, text: &mut String| {
        if text.is_empty() {
            return;
        }
        parts.push(NamePart {
            text: text.as_str().into(),
            image: Default::default(),
            is_emoji: false,
        });
        text.clear();
    };

    for grapheme in name.graphemes(true) {
        if emojis::get(grapheme).is_some() {
            push_text(&mut parts, &mut text);
            if let Some(image) = emoji_image(grapheme) {
                parts.push(NamePart {
                    text: Default::default(),
                    image,
                    is_emoji: true,
                });
            } else {
                text.push_str(grapheme);
            }
        } else {
            text.push_str(grapheme);
        }
    }
    push_text(&mut parts, &mut text);
    parts
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
                name_parts: ModelRc::new(VecModel::from(name_parts(&profile.name))),
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
                start_connection(&ui, &store, &engine, &logs, false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        let engine = engine.clone();
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

            if ui.get_connected() && !ui.get_busy() {
                let message = format!(
                    "switching to {} ({}) without dropping the tunnel",
                    selected.name,
                    selected.core.label()
                );
                #[cfg(target_os = "android")]
                crate::logcat(&message);
                push_log(&ui, &logs, message);
                start_connection(&ui, &store, &engine, &logs, true);
            } else {
                push_log(&ui, &logs, format!("selected profile: {}", selected.name));
            }
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

            let trimmed = raw.trim();
            if trimmed.starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(config)
                        if config.is_object()
                            && config
                                .get("outbounds")
                                .is_some_and(|value| value.is_array()) =>
                    {
                        let name = format!("Custom · {}", core.label());
                        let profile = config::Profile {
                            id: config::new_id(),
                            name: name.clone(),
                            core,
                            protocol: "custom".to_string(),
                            address: String::new(),
                            port: 0,
                            config,
                            source: None,
                        };
                        {
                            let mut guard = store.lock().unwrap();
                            guard.add_profile(profile);
                            let _ = save_store(&guard);
                        }
                        let guard = store.lock().unwrap();
                        ui.set_profiles(profile_model(&guard, &pings));
                        ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
                        drop(guard);
                        push_log(&ui, &logs, format!("imported {name}"));
                    }
                    Ok(_) => {
                        push_log(
                            &ui,
                            &logs,
                            "import failed: JSON config needs an \"outbounds\" array".to_string(),
                        );
                        ui.set_status_text("Import failed".into());
                    }
                    Err(error) => {
                        push_log(&ui, &logs, format!("import failed: {error}"));
                        ui.set_status_text("Import failed".into());
                    }
                }
                return;
            }

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
        let logs = logs.clone();
        ui.on_open_menu(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_menu_open(true);
                ui.set_import_open(false);
                push_log(&ui, &logs, "choose how to add a profile".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_menu(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_menu_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_open_import(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_menu_open(false);
                ui.set_import_open(true);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_import(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_import_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_paste_import(move || {
            if let Some(ui) = weak.upgrade() {
                let text = crate::read_clipboard();
                let text = text.trim();
                if text.is_empty() {
                    push_log(&ui, &logs, "clipboard is empty".to_string());
                    return;
                }
                ui.set_link_input(text.into());
                ui.set_menu_open(false);
                ui.set_import_open(true);
                push_log(&ui, &logs, "clipboard loaded".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_add_subscription(move || {
            if let Some(ui) = weak.upgrade() {
                push_log(
                    &ui,
                    &logs,
                    "subscription profiles arrive in the next update".to_string(),
                );
                ui.set_menu_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_add_manual(move || {
            if let Some(ui) = weak.upgrade() {
                push_log(
                    &ui,
                    &logs,
                    "manual protocol setup arrives in the next update".to_string(),
                );
                ui.set_menu_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toggle_ip(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_show_ip(!ui.get_show_ip());
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
    switching: bool,
) {
    let (settings, profile) = {
        let guard = store.lock().unwrap();
        (guard.settings.clone(), guard.selected_profile().cloned())
    };

    let Some(profile) = profile else {
        push_log(ui, logs, "no profile selected".to_string());
        return;
    };

    // Log the transport shape (never credentials) so reports can identify
    // slow XHTTP modes, missing ALPN or a fallback transport.
    if let Some(outbound) = profile
        .config
        .get("outbounds")
        .and_then(serde_json::Value::as_array)
        .and_then(|outbounds| outbounds.first())
    {
        if let Some(stream) = outbound.get("streamSettings") {
            let network = stream
                .get("network")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            let security = stream
                .get("security")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            let mode = stream
                .get("xhttpSettings")
                .and_then(|settings| settings.get("mode"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            let alpn = stream
                .get("tlsSettings")
                .and_then(|tls| tls.get("alpn"))
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let extra = stream
                .get("xhttpSettings")
                .and_then(|settings| settings.get("extra"))
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            push_log(
                ui,
                logs,
                format!("transport: {network}/{security} mode={mode} alpn={alpn} extra={extra}"),
            );
        } else {
            let kind = outbound
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            let transport = outbound
                .get("transport")
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-");
            push_log(ui, logs, format!("transport: {kind}/{transport}"));
        }
    }

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

    if switching {
        SWITCHING.store(true, Ordering::SeqCst);
    }
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
    SWITCHING.store(false, Ordering::SeqCst);
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
                if SWITCHING.load(Ordering::SeqCst) {
                    return;
                }
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
                SWITCHING.store(false, Ordering::SeqCst);
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
                if SWITCHING.load(Ordering::SeqCst) {
                    ui.set_busy(true);
                    ui.set_status_text("Switching…".into());
                    return;
                }
                ui.set_busy(true);
                ui.set_status_text("Stopping…".into());
                crate::stop_android_vpn();
            }
            EngineState::Error(message) => {
                SWITCHING.store(false, Ordering::SeqCst);
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Core error".into());
                #[cfg(not(target_os = "android"))]
                let _ = system_proxy::disable();
                // Android: keep the TUN up as a blackhole so apps cannot fall
                // back to the physical network. The user disconnects explicitly.
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

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn folds_decorative_unicode_to_ascii() {
        assert_eq!(display_name("🇫🇮 | 𝔽𝕀ℕ𝕃𝔸ℕ𝔻²"), "🇫🇮 | FINLAND2");
        assert_eq!(display_name("🇹🇷 | TR > 𝗧𝘂𝗻 [1]"), "🇹🇷 | TR > Tun [1]");
    }
}
