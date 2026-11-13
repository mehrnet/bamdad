use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use bamdad_core::config::{self, load_store, save_store, Store};
use bamdad_core::engine::{EngineEvent, EngineHandle, EngineState, LaunchSpec, LogStream};
use bamdad_core::system_proxy::{self, ProxySettings};
use bamdad_core::{binary, link, paths, probe, CoreKind};

use crate::{AppWindow, GroupRow, NamePart, ProfileRow};

pub(crate) type LogBuffer = Arc<Mutex<Vec<String>>>;
type PingState = Arc<Mutex<HashMap<String, (String, i32)>>>;

static VPN_SOCKS_PORT: AtomicU16 = AtomicU16::new(10808);
static SWITCHING: AtomicBool = AtomicBool::new(false);
static NATIVE_TUNNEL_ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_IPV4: Mutex<String> = Mutex::new(String::new());
static LAST_IPV6: Mutex<String> = Mutex::new(String::new());
static LAST_COUNTRY: Mutex<String> = Mutex::new(String::new());
static LOG_LAST_PUBLISH: Mutex<Option<Instant>> = Mutex::new(None);
static LOG_FLUSH_PENDING: AtomicBool = AtomicBool::new(false);
static ACTIVE_PROFILE: Mutex<String> = Mutex::new(String::new());
static ACTIVE_ACCENT: Mutex<String> = Mutex::new(String::new());

struct ImportContext {
    ui: Weak<AppWindow>,
    store: Arc<Mutex<Store>>,
    pings: PingState,
    logs: LogBuffer,
}

static IMPORT_CONTEXT: Mutex<Option<ImportContext>> = Mutex::new(None);

fn set_import_context(
    ui: Weak<AppWindow>,
    store: Arc<Mutex<Store>>,
    pings: PingState,
    logs: LogBuffer,
) {
    if let Ok(mut slot) = IMPORT_CONTEXT.lock() {
        *slot = Some(ImportContext {
            ui,
            store,
            pings,
            logs,
        });
    }
}

/// Import one or more config bodies from the Android file/ZIP picker.
/// Returns the number of profiles added.
pub(crate) fn import_native_files(group: &str, files_json: &str) -> usize {
    let Ok(files) = serde_json::from_str::<Vec<serde_json::Value>>(files_json) else {
        return 0;
    };
    let Some(context) = IMPORT_CONTEXT.lock().ok().and_then(|slot| {
        slot.as_ref().map(|context| ImportContext {
            ui: context.ui.clone(),
            store: context.store.clone(),
            pings: context.pings.clone(),
            logs: context.logs.clone(),
        })
    }) else {
        return 0;
    };

    let mut added = Vec::new();
    for file in files {
        let name = file
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("imported");
        let text = file
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        for mut profile in profiles_from_text(name, text) {
            if !group.trim().is_empty() {
                profile.group_id = Some(format!("import:{}", group.trim()));
                profile.group_name = Some(group.trim().to_string());
            }
            added.push(profile);
        }
    }

    let count = added.len();
    let group = group.trim().to_string();
    if count == 0 {
        let ui = context.ui.clone();
        let logs = context.logs.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui.upgrade() {
                push_log(&ui, &logs, "import found no supported configs".to_string());
                ui.set_status_text("Import failed".into());
            }
        });
        return 0;
    }

    {
        let mut store = context.store.lock().unwrap();
        for profile in added {
            store.add_profile(profile);
        }
        let _ = save_store(&store);
    }

    let ui = context.ui.clone();
    let store = context.store.clone();
    let pings = context.pings.clone();
    let logs = context.logs.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            let store = store.lock().unwrap();
            ui.set_groups(group_model(&store, &pings));
            ui.set_selected_id(store.selected_id.clone().unwrap_or_default().into());
            drop(store);
            let suffix = if group.is_empty() {
                String::new()
            } else {
                format!(" from {group}")
            };
            push_log(
                &ui,
                &logs,
                format!(
                    "imported {count} config{}{suffix}",
                    if count == 1 { "" } else { "s" }
                ),
            );
            ui.set_status_text("Imported".into());
        }
    });
    count
}

fn profiles_from_text(name: &str, text: &str) -> Vec<config::Profile> {
    if let Some(profile) = bamdad_core::native::profile_from_config(Some(name), text) {
        return vec![profile];
    }

    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if value.is_object()
                && (value.get("outbounds").is_some_and(|v| v.is_array())
                    || value.get("endpoints").is_some_and(|v| v.is_array()))
            {
                let core = if value.get("endpoints").is_some() {
                    CoreKind::SingBox
                } else {
                    detect_json_core(&value)
                };
                return vec![config::Profile {
                    id: config::new_id(),
                    name: name.to_string(),
                    core,
                    transport: bamdad_core::Transport::Proxy,
                    protocol: "custom".to_string(),
                    address: String::new(),
                    port: 0,
                    config: value,
                    raw_config: None,
                    source: None,
                    group_id: None,
                    group_name: None,
                    group_url: None,
                }];
            }
        }
        return Vec::new();
    }

    let mut profiles = Vec::new();
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Ok(spec) = link::parse(Some(name), line) {
            profiles.push(config::Profile::from_spec(&spec, CoreKind::Xray));
        }
    }
    profiles
}

pub(crate) fn set_active_accent(name: &str) {
    *ACTIVE_ACCENT.lock().unwrap() = name.to_string();
}

pub(crate) fn active_accent_name() -> String {
    ACTIVE_ACCENT.lock().unwrap().clone()
}

pub(crate) fn set_active_profile_name(name: &str) {
    *ACTIVE_PROFILE.lock().unwrap() = name.to_string();
}

pub(crate) fn active_profile_name() -> String {
    ACTIVE_PROFILE.lock().unwrap().clone()
}

pub fn init(
    ui: &AppWindow,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
    events: Receiver<EngineEvent>,
) {
    let store = Arc::new(Mutex::new(load_store()));
    let logs: LogBuffer = Arc::new(Mutex::new(Vec::new()));
    let pings: PingState = Arc::new(Mutex::new(HashMap::new()));

    set_import_context(ui.as_weak(), store.clone(), pings.clone(), logs.clone());
    seed(ui, &store, &pings);
    prefetch_dns(&store, &runtime);
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
    if let Ok(tunnel) = std::env::var("BAMDAD_PREVIEW_TUNNEL") {
        ui.set_connected(true);
        ui.set_busy(false);
        ui.set_status_text("Connected".into());
        ui.set_tunnel_ip(tunnel.into());
        set_network_text(&ui, "203.0.113.7", "");
        ui.set_total_down_text("1.2 MB".into());
        ui.set_total_up_text("312 KB".into());
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

    set_import_context(ui.as_weak(), store.clone(), pings.clone(), logs.clone());
    seed(ui, &store, &pings);
    prefetch_dns(&store, &runtime);
    seed_last_network(ui);
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
                ui.set_tunnel_ip("".into());
                ui.set_status_text("Core error".into());
            }
            EngineState::Stopped => {
                if SWITCHING.load(Ordering::SeqCst) {
                    return;
                }
                NATIVE_TUNNEL_ACTIVE.store(false, Ordering::SeqCst);
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

fn set_last_network(ipv4: Option<&str>, ipv6: Option<&str>, country: Option<&str>) {
    *LAST_IPV4.lock().unwrap() = ipv4.unwrap_or("").to_string();
    *LAST_IPV6.lock().unwrap() = ipv6.unwrap_or("").to_string();
    *LAST_COUNTRY.lock().unwrap() = country.unwrap_or("").to_string();
}

fn clear_last_network() {
    LAST_IPV4.lock().unwrap().clear();
    LAST_IPV6.lock().unwrap().clear();
    LAST_COUNTRY.lock().unwrap().clear();
}

fn country_flag(code: &str) -> slint::Image {
    if code.len() != 2 || !code.is_ascii() {
        return Default::default();
    }
    let mut flag = String::new();
    for ch in code.chars() {
        let base = 0x1F1E6u32 + (ch.to_ascii_uppercase() as u32 - 'A' as u32);
        let Some(ch) = char::from_u32(base) else {
            return Default::default();
        };
        flag.push(ch);
    }
    emoji_image(&flag).unwrap_or_default()
}

fn seed_last_network(ui: &AppWindow) {
    let ipv4 = LAST_IPV4.lock().unwrap().clone();
    let ipv6 = LAST_IPV6.lock().unwrap().clone();
    let country = LAST_COUNTRY.lock().unwrap().clone();
    set_network_text(ui, &ipv4, &ipv6);
    ui.set_country_flag(if country.is_empty() {
        Default::default()
    } else {
        country_flag(&country)
    });
}

fn mask_ipv4(ip: &str) -> String {
    let mut parts = ip.split('.');
    let (Some(a), Some(b), Some(c), Some(_)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ip.to_string();
    };
    format!("{a}.{b}.{c}.***")
}

fn mask_ipv6(ip: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    if let Some((prefix, _)) = ip.split_once("::") {
        if prefix.is_empty() {
            return "::***".to_string();
        }
        return format!("{prefix}::***");
    }
    let visible: Vec<&str> = ip.split(':').take(3).collect();
    format!("{}:***", visible.join(":"))
}

fn set_network_text(ui: &AppWindow, ipv4: &str, ipv6: &str) {
    ui.set_ipv4_text(ipv4.into());
    ui.set_ipv6_text(ipv6.into());
    ui.set_ipv4_mask(mask_ipv4(ipv4).into());
    ui.set_ipv6_mask(mask_ipv6(ipv6).into());
}

fn normalize_theme(mode: &str) -> &'static str {
    match mode {
        "dark" => "dark",
        "light" => "light",
        "black" => "black",
        _ => "system",
    }
}

fn normalize_accent(accent: &str) -> &'static str {
    match accent {
        "blue" => "blue",
        "red" => "red",
        "green" => "green",
        "purple" => "purple",
        "orange" => "orange",
        "pink" => "pink",
        "gray" => "gray",
        _ => "cyan",
    }
}

fn apply_theme(ui: &AppWindow, mode: &str, accent: &str) {
    let mode = normalize_theme(mode);
    let dark = match mode {
        "dark" | "black" => true,
        "light" => false,
        _ => system_prefers_dark(),
    };
    ui.set_theme_mode(mode.into());
    ui.set_palette_dark(dark);
    ui.set_palette_black(mode == "black");
    let accent = normalize_accent(accent);
    set_active_accent(accent);
    ui.set_accent_mode(accent.into());
}

#[cfg(target_os = "android")]
fn system_prefers_dark() -> bool {
    crate::android::system_theme()
        .map(|theme| theme.eq_ignore_ascii_case("dark"))
        .unwrap_or(true)
}

#[cfg(target_os = "windows")]
fn system_prefers_dark() -> bool {
    let Ok(output) = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "AppsUseLightTheme",
        ])
        .output()
    else {
        return true;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    if text.contains("0x0") {
        true
    } else if text.contains("0x1") {
        false
    } else {
        true
    }
}

#[cfg(target_os = "macos")]
fn system_prefers_dark() -> bool {
    let Ok(output) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .eq_ignore_ascii_case("dark")
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn system_prefers_dark() -> bool {
    if let Ok(output) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if value.contains("dark") {
                return true;
            }
            if value.contains("light") || value.contains("default") {
                return false;
            }
        }
    }
    if let Ok(value) = std::env::var("GTK_THEME") {
        if value.to_ascii_lowercase().contains("dark") {
            return true;
        }
    }
    true
}

#[cfg(not(any(
    target_os = "android",
    target_os = "windows",
    target_os = "macos",
    unix
)))]
fn system_prefers_dark() -> bool {
    true
}

/// Cloudflare's trace endpoint answers on a fixed IP, so it works even when
/// DNS inside the tunnel is slow or broken.
fn fetch_trace_ip(url: &str, ipv6: bool, socks_port: Option<u16>) -> Option<std::net::IpAddr> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(4))
        .timeout(Duration::from_secs(7));
    if let Some(port) = socks_port {
        match ureq::Proxy::new(format!("socks5://127.0.0.1:{port}")) {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(error) => {
                #[cfg(target_os = "android")]
                crate::logcat(&format!("public exit: bad proxy: {error}"));
                return None;
            }
        }
    }
    let agent = builder.build();
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(error) => {
            #[cfg(target_os = "android")]
            crate::logcat(&format!("public exit: trace {url} failed: {error}"));
            return None;
        }
    };
    let body = match response.into_string() {
        Ok(body) => body,
        Err(error) => {
            #[cfg(target_os = "android")]
            crate::logcat(&format!("public exit: trace {url} body failed: {error}"));
            return None;
        }
    };
    for line in body.lines() {
        let Some(value) = line.strip_prefix("ip=") else {
            continue;
        };
        if let Ok(ip) = value.trim().parse::<std::net::IpAddr>() {
            if ip.is_ipv6() == ipv6 {
                return Some(ip);
            }
        }
    }
    #[cfg(target_os = "android")]
    crate::logcat(&format!("public exit: trace {url} had no address"));
    None
}

/// Fast exit-address lookup using Cloudflare's fixed-IP trace endpoint. When
/// `socks_port` is set the request goes through the proxy that is currently
/// carrying the connection; otherwise it goes through the tun directly.
fn fetch_exit_ips_fast(
    socks_port: Option<u16>,
) -> (Option<std::net::IpAddr>, Option<std::net::IpAddr>) {
    let ipv4 = fetch_trace_ip("https://1.1.1.1/cdn-cgi/trace", false, socks_port)
        .or_else(|| fetch_trace_ip("https://1.0.0.1/cdn-cgi/trace", false, socks_port));
    let ipv6 = fetch_trace_ip(
        "https://[2606:4700:4700::1111]/cdn-cgi/trace",
        true,
        socks_port,
    );
    (ipv4, ipv6)
}

/// Look up the public exit address while a native tunnel owns the default
/// route. The app's own sockets go through the tun, so a direct request
/// reports the address the server assigns to the tunnel.
fn refresh_native_exit_ips(ui: &AppWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Give Android a moment to install the routes.
        std::thread::sleep(Duration::from_millis(900));
        #[cfg(target_os = "android")]
        crate::logcat("public exit: looking up");
        let (ipv4, ipv6) = fetch_exit_ips_fast(None);
        if ipv4.is_none() && ipv6.is_none() {
            return;
        }
        let country = ipv4
            .or(ipv6)
            .and_then(|ip| crate::country::lookup(ip).map(str::to_string));
        let v4 = ipv4.map(|ip| ip.to_string());
        let v6 = ipv6.map(|ip| ip.to_string());
        #[cfg(target_os = "android")]
        crate::logcat(&format!(
            "public exit: {} {}",
            v4.as_deref().unwrap_or("-"),
            v6.as_deref().unwrap_or("-")
        ));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            // A disconnect or transport switch may have happened while the
            // lookup was in flight; never apply stale results.
            if !ui.get_connected() || !NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst) {
                return;
            }
            set_last_network(v4.as_deref(), v6.as_deref(), country.as_deref());
            set_network_text(
                &ui,
                v4.as_deref().unwrap_or_default(),
                v6.as_deref().unwrap_or_default(),
            );
            ui.set_country_flag(country.as_deref().map(country_flag).unwrap_or_default());
        });
    });
}

fn fetch_exit_ips(port: u16) -> Option<(Option<std::net::IpAddr>, Option<std::net::IpAddr>)> {
    const V4_ECHOES: &[(&str, &str)] = &[
        ("api.ipify.org", "/"),
        ("ipv4.icanhazip.com", "/"),
        ("ifconfig.me", "/ip"),
        ("checkip.amazonaws.com", "/"),
    ];
    const V6_ECHOES: &[(&str, &str)] = &[("api6.ipify.org", "/"), ("ipv6.icanhazip.com", "/")];

    let mut ipv4 = None;
    let mut ipv6 = None;

    for (host, path) in V4_ECHOES {
        let Ok(body) = bamdad_core::socks::http_get(port, host, path, Duration::from_secs(6))
        else {
            continue;
        };
        let candidate = body.lines().next().unwrap_or("").trim();
        if let Ok(ip) = candidate.parse::<std::net::IpAddr>() {
            if ip.is_ipv4() {
                ipv4 = Some(ip);
                break;
            }
        }
    }

    for (host, path) in V6_ECHOES {
        let Ok(body) = bamdad_core::socks::http_get(port, host, path, Duration::from_secs(6))
        else {
            continue;
        };
        let candidate = body.lines().next().unwrap_or("").trim();
        if let Ok(ip) = candidate.parse::<std::net::IpAddr>() {
            if ip.is_ipv6() {
                ipv6 = Some(ip);
                break;
            }
        }
    }

    if ipv4.is_none() && ipv6.is_none() {
        None
    } else {
        Some((ipv4, ipv6))
    }
}

fn seed(ui: &AppWindow, store: &Arc<Mutex<Store>>, pings: &PingState) {
    let store = store.lock().unwrap();

    ui.set_groups(group_model(&store, pings));
    ui.set_selected_id(store.selected_id.clone().unwrap_or_default().into());
    ui.set_mobile(cfg!(target_os = "android"));
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());
    ui.set_socks_port(store.settings.socks_port.to_string().into());
    ui.set_http_port(store.settings.http_port.to_string().into());
    ui.set_system_proxy(store.settings.system_proxy);
    ui.set_theme_mode(store.settings.theme.clone().into());
    apply_theme(ui, &store.settings.theme, &store.settings.accent);
    #[cfg(target_os = "android")]
    crate::android::apply_launcher_accent(&store.settings.accent);
    let active_profile = store
        .selected_profile()
        .map(|profile| profile.name.clone())
        .unwrap_or_default();
    ui.set_active_profile(active_profile.clone().into());
    set_active_profile_name(&active_profile);
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
    static GROUP_STATE: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
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
    // Keep the Iran flag recognizable without the emblem: a plain
    // green/white/red tricolour instead of Twemoji's detailed version.
    if grapheme == "🇮🇷" {
        return slint::Image::load_from_svg_data(include_bytes!("../ui/icons/flag-ir.svg")).ok();
    }
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

/// Fold decorative Unicode (mathematical alphanumerics and fullwidth forms) to
/// ASCII for display. System fonts often lack these glyphs and show boxes.
/// Superscripts such as `²` are kept as-is. Other scripts are left untouched.
fn display_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;

    let mut out = String::new();
    for ch in name.chars() {
        let code = ch as u32;
        if (0x1D400..=0x1D7FF).contains(&code)
            || (0x2100..=0x214F).contains(&code)
            || (0xFF00..=0xFFEF).contains(&code)
        {
            // NFKD maps math/fullwidth letters to ASCII.
            out.extend(ch.to_string().nfkd());
        } else {
            out.push(ch);
        }
    }
    out
}

fn is_rtl_name(name: &str) -> bool {
    for ch in name.chars() {
        if ch.is_alphabetic() {
            let code = ch as u32;
            return matches!(
                code,
                0x0590..=0x05FF
                    | 0x0600..=0x06FF
                    | 0x0750..=0x077F
                    | 0x08A0..=0x08FF
                    | 0xFB50..=0xFDFF
                    | 0xFE70..=0xFEFF
            );
        }
    }
    false
}

fn name_parts(name: &str) -> Vec<NamePart> {
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

fn node_row(
    profile: &config::Profile,
    pings: &HashMap<String, (String, i32)>,
    selected_id: Option<&str>,
) -> ProfileRow {
    let (ping, ping_state) = pings
        .get(&profile.id)
        .cloned()
        .unwrap_or_else(|| (initial_ping(profile), 0));
    let rtl = is_rtl_name(&profile.name);
    let plain_name = display_name(&profile.name);
    let mut parts = name_parts(&plain_name);
    if rtl {
        parts.reverse();
    }
    ProfileRow {
        id: profile.id.as_str().into(),
        name: profile.name.as_str().into(),
        plain_name: plain_name.as_str().into(),
        long_name: plain_name.chars().count() > 36,
        name_parts: ModelRc::new(VecModel::from(parts)),
        detail: format!(
            "{} · {}",
            protocol_label(&profile.protocol),
            profile.endpoint()
        )
        .into(),
        protocol: protocol_label(&profile.protocol).into(),
        core: profile.transport_label().into(),
        ping: ping.into(),
        ping_state,
        selected: selected_id == Some(profile.id.as_str()),
        rtl,
    }
}

fn group_ping(nodes: &[&config::Profile], pings: &HashMap<String, (String, i32)>) -> (String, i32) {
    if nodes.is_empty() {
        return ("—".to_string(), 0);
    }
    let mut testing = false;
    let mut timeout = false;
    let mut best: Option<(u64, i32)> = None;

    for node in nodes {
        match pings.get(&node.id) {
            Some((text, state)) => {
                if *state == 1 {
                    testing = true;
                }
                if *state == 4 {
                    timeout = true;
                }
                if let Some(ms) = text
                    .strip_suffix(" ms")
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    if best.is_none_or(|(current, _)| ms < current) {
                        best = Some((ms, *state));
                    }
                }
            }
            None => {}
        }
    }

    if testing {
        ("…".to_string(), 1)
    } else if let Some((ms, state)) = best {
        (format!("{ms} ms"), state)
    } else if timeout {
        ("timeout".to_string(), 4)
    } else {
        ("Ping".to_string(), 0)
    }
}

fn group_model(store: &Store, pings: &PingState) -> ModelRc<GroupRow> {
    let pings = pings.lock().unwrap();
    let mut groups: Vec<(String, String, Vec<&config::Profile>)> = vec![(
        "default".to_string(),
        store.default_group_name.clone(),
        Vec::new(),
    )];
    let mut index: HashMap<String, usize> = HashMap::new();
    index.insert("default".to_string(), 0);

    for profile in &store.profiles {
        let key = profile.group_key();
        if let Some(&position) = index.get(&key) {
            groups[position].2.push(profile);
        } else {
            index.insert(key.clone(), groups.len());
            groups.push((key, profile.group_title(), vec![profile]));
        }
    }

    let rows: Vec<GroupRow> = groups
        .into_iter()
        .map(|(key, title, nodes)| {
            let expanded = GROUP_STATE.with(|state| {
                *state
                    .borrow_mut()
                    .entry(key.clone())
                    .or_insert(key == "default")
            });
            let mut nodes = nodes;
            if let Some(order) = store.group_order.get(&key) {
                let positions: HashMap<&str, usize> = order
                    .iter()
                    .enumerate()
                    .map(|(position, id)| (id.as_str(), position))
                    .collect();
                nodes.sort_by_key(|profile| {
                    positions
                        .get(profile.id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX)
                });
            }
            let node_rows: Vec<ProfileRow> = nodes
                .iter()
                .map(|profile| node_row(profile, &pings, store.selected_id.as_deref()))
                .collect();
            let (ping, ping_state) = group_ping(&nodes, &pings);
            let rtl = is_rtl_name(&title);
            let display_title = display_name(&title);
            GroupRow {
                id: key.as_str().into(),
                name: display_title.as_str().into(),
                kind: if key == "default" { "DEFAULT" } else { "SUB" }.into(),
                expanded,
                deletable: key != "default",
                ping: ping.into(),
                ping_state,
                nodes: ModelRc::new(VecModel::from(node_rows)),
                rtl,
            }
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn initial_ping(profile: &config::Profile) -> String {
    if profile.protocol.eq_ignore_ascii_case("direct")
        || profile.address == "local"
        || profile.address.is_empty()
        || profile.port == 0
    {
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
        "wireguard" | "wg" => "WIREGUARD",
        "amneziawg" | "awg" => "AMNEZIAWG",
        "ipsec" | "ikev2" => "IPSEC",
        "openvpn" | "ovpn" => "OPENVPN",
        "direct" => "DIRECT",
        _ => "CUSTOM",
    }
    .to_string()
}

fn detect_json_core(config: &serde_json::Value) -> CoreKind {
    let looks_singbox = config
        .get("outbounds")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|outbounds| {
            outbounds
                .iter()
                .any(|outbound| outbound.get("type").is_some())
        });
    if looks_singbox {
        CoreKind::SingBox
    } else {
        CoreKind::Xray
    }
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
            ui.set_groups(group_model(&guard, &pings));
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
        ui.on_import_link(move |raw| {
            let Some(ui) = weak.upgrade() else { return };
            let trimmed = raw.trim();
            if let Some(profile) = bamdad_core::native::profile_from_config(None, trimmed) {
                let name = profile.name.clone();
                let label = profile.transport_label();
                {
                    let mut guard = store.lock().unwrap();
                    guard.add_profile(profile);
                    let _ = save_store(&guard);
                }
                let guard = store.lock().unwrap();
                ui.set_groups(group_model(&guard, &pings));
                ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
                drop(guard);
                push_log(&ui, &logs, format!("imported {name} ({label})"));
                ui.set_status_text("Imported".into());
                return;
            }

            if trimmed.starts_with('{') {
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(config)
                        if config.is_object()
                            && config
                                .get("outbounds")
                                .is_some_and(|value| value.is_array()) =>
                    {
                        let core = detect_json_core(&config);
                        let name = format!("Custom · {}", core.label());
                        let profile = config::Profile {
                            id: config::new_id(),
                            name: name.clone(),
                            core,
                            transport: bamdad_core::Transport::Proxy,
                            protocol: "custom".to_string(),
                            address: String::new(),
                            port: 0,
                            config,
                            raw_config: None,
                            source: None,
                            group_id: None,
                            group_name: None,
                            group_url: None,
                        };
                        {
                            let mut guard = store.lock().unwrap();
                            guard.add_profile(profile);
                            let _ = save_store(&guard);
                        }
                        let guard = store.lock().unwrap();
                        ui.set_groups(group_model(&guard, &pings));
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
                    // Links are core-agnostic; use Xray by default and keep
                    // XHTTP on Xray. JSON imports are detected from their shape.
                    let mut core = CoreKind::Xray;
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
                    ui.set_groups(group_model(&guard, &pings));
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
        ui.on_file_import(move || {
            let Some(_ui) = weak.upgrade() else { return };
            #[cfg(target_os = "android")]
            crate::open_import_file();
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
            ui.set_groups(group_model(&guard, &pings));
            ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
            drop(guard);
            push_log(&ui, &logs, "profiles reloaded".to_string());
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_theme_mode(move |mode| {
            let Some(ui) = weak.upgrade() else { return };
            let mode = normalize_theme(mode.as_str());
            let accent = {
                let mut guard = store.lock().unwrap();
                guard.settings.theme = mode.to_string();
                let _ = save_store(&guard);
                guard.settings.accent.clone()
            };
            ui.set_theme_mode(mode.into());
            apply_theme(&ui, mode, &accent);
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_accent_mode(move |accent| {
            let Some(ui) = weak.upgrade() else { return };
            let accent = normalize_accent(accent.as_str());
            let theme = {
                let mut guard = store.lock().unwrap();
                guard.settings.accent = accent.to_string();
                let _ = save_store(&guard);
                guard.settings.theme.clone()
            };
            ui.set_accent_mode(accent.into());
            apply_theme(&ui, &theme, accent);
            #[cfg(target_os = "android")]
            crate::android::apply_launcher_accent(accent);
            #[cfg(target_os = "android")]
            crate::android::update_vpn_notification(
                &ui.get_active_profile(),
                &ui.get_total_down_text(),
                &ui.get_total_up_text(),
                accent,
            );
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
        ui.on_add_subscription(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_menu_open(false);
                ui.set_subscription_url("".into());
                ui.set_subscription_open(true);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_subscription(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_subscription_open(false);
                ui.set_subscription_url("".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_add_subscription_url(move |url| {
            let Some(ui) = weak.upgrade() else { return };
            let url = url.trim().to_string();
            if url.is_empty() {
                return;
            }
            ui.set_subscription_open(false);
            ui.set_subscription_url("".into());
            push_log(&ui, &logs, "fetching subscription…".to_string());

            let weak = weak.clone();
            let store = store.clone();
            let pings = pings.clone();
            let logs = logs.clone();
            std::thread::spawn(move || {
                let result = fetch_subscription(&url);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok((group_name, nodes)) => {
                            if nodes.is_empty() {
                                push_log(
                                    &ui,
                                    &logs,
                                    "subscription contains no supported configs".to_string(),
                                );
                                return;
                            }
                            let group_id = config::new_id();
                            let group_name = group_name.unwrap_or_else(|| "Subscription".into());
                            let first_id = nodes[0].id.clone();
                            let mut guard = store.lock().unwrap();
                            for mut node in nodes {
                                node.group_id = Some(group_id.clone());
                                node.group_name = Some(group_name.clone());
                                node.group_url = Some(url.clone());
                                guard.profiles.push(node);
                            }
                            guard.selected_id = Some(first_id.clone());
                            guard.selected = guard
                                .profiles
                                .iter()
                                .find(|profile| profile.id == first_id)
                                .map(|profile| profile.name.clone());
                            let _ = save_store(&guard);
                            ui.set_groups(group_model(&guard, &pings));
                            ui.set_selected_id(first_id.into());
                            let count = guard
                                .profiles
                                .iter()
                                .filter(|profile| profile.group_id == Some(group_id.clone()))
                                .count();
                            drop(guard);
                            push_log(&ui, &logs, format!("subscription added: {count} configs"));
                        }
                        Err(error) => {
                            push_log(&ui, &logs, format!("subscription failed: {error}"));
                        }
                    }
                });
            });
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
        let store = store.clone();
        let pings = pings.clone();
        ui.on_toggle_group(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            GROUP_STATE.with(|state| {
                let mut state = state.borrow_mut();
                let value = state.entry(id.to_string()).or_insert(true);
                *value = !*value;
            });
            let guard = store.lock().unwrap();
            ui.set_groups(group_model(&guard, &pings));
        });
    }

    {
        let weak = ui.as_weak();
        let runtime = runtime.clone();
        let store = store.clone();
        let pings = pings.clone();
        ui.on_ping_group(move |id| {
            let ids: Vec<String> = {
                let guard = store.lock().unwrap();
                guard
                    .profiles
                    .iter()
                    .filter(|profile| profile.group_key() == id.as_str())
                    .map(|profile| profile.id.clone())
                    .collect()
            };
            for id in ids {
                begin_ping(&weak, &runtime, &store, &pings, id);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_delete_group(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_group_menu_open(false);
            let removed = {
                let mut guard = store.lock().unwrap();
                let removed = guard.remove_group(id.as_str());
                if removed {
                    let _ = save_store(&guard);
                }
                removed
            };
            if removed {
                let guard = store.lock().unwrap();
                ui.set_groups(group_model(&guard, &pings));
                ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
                drop(guard);
                push_log(&ui, &logs, "subscription deleted".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_open_group_menu(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let (name, is_default) = {
                let guard = store.lock().unwrap();
                if id.as_str() == "default" {
                    (guard.default_group_name.clone(), true)
                } else {
                    let Some(node) = guard
                        .profiles
                        .iter()
                        .find(|profile| profile.group_key() == id.as_str())
                    else {
                        return;
                    };
                    (node.group_title(), false)
                }
            };
            ui.set_group_menu_id(id.clone());
            ui.set_group_menu_name(name.into());
            ui.set_group_menu_default(is_default);
            ui.set_group_menu_open(true);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_group_menu(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_group_menu_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_clear_group(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let mut guard = store.lock().unwrap();
            let before = guard.profiles.len();
            if id.as_str() == "default" {
                guard.profiles.retain(|profile| profile.group_id.is_some());
            } else {
                guard
                    .profiles
                    .retain(|profile| profile.group_key() != id.as_str());
            }
            let removed = before - guard.profiles.len();
            if removed > 0 {
                if guard
                    .selected_id
                    .as_ref()
                    .is_some_and(|selected| guard.profile_by_id(selected).is_none())
                {
                    guard.selected_id = guard.profiles.first().map(|profile| profile.id.clone());
                }
                guard.selected = guard
                    .selected_id
                    .as_ref()
                    .and_then(|selected| guard.profile_by_id(selected))
                    .map(|profile| profile.name.clone());
                let _ = save_store(&guard);
                ui.set_groups(group_model(&guard, &pings));
                ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
                drop(guard);
                push_log(&ui, &logs, format!("{removed} configs deleted"));
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        ui.on_sort_group_latency(move |group_id| {
            let Some(ui) = weak.upgrade() else { return };
            let mut guard = store.lock().unwrap();
            let ping_map = pings.lock().unwrap();
            let mut nodes: Vec<(usize, String)> = guard
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, profile)| profile.group_key() == group_id.as_str())
                .map(|(index, profile)| (index, profile.id.clone()))
                .collect();
            nodes.sort_by_key(|(index, id)| {
                let (rank, ms) = match ping_map.get(id) {
                    Some((text, state)) if *state == 2 || *state == 3 => (
                        0u8,
                        text.strip_suffix(" ms")
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(u64::MAX),
                    ),
                    Some((_, 1)) => (1u8, u64::MAX),
                    Some((_, 4)) => (2u8, u64::MAX),
                    _ => (3u8, u64::MAX),
                };
                (rank, ms, *index)
            });
            let ids: Vec<String> = nodes.into_iter().map(|(_, id)| id).collect();
            guard.group_order.insert(group_id.to_string(), ids);
            let _ = save_store(&guard);
            drop(ping_map);
            ui.set_groups(group_model(&guard, &pings));
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_reset_config_order(move |group_id| {
            let Some(ui) = weak.upgrade() else { return };
            let mut guard = store.lock().unwrap();
            if guard.group_order.remove(group_id.as_str()).is_some() {
                let _ = save_store(&guard);
                ui.set_groups(group_model(&guard, &pings));
                drop(guard);
                push_log(&ui, &logs, "custom ordering reset".to_string());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_open_config_menu(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let name = store
                .lock()
                .unwrap()
                .profile_by_id(id.as_str())
                .map(|profile| profile.name.clone());
            let Some(name) = name else { return };
            ui.set_config_menu_id(id.clone());
            ui.set_config_menu_name(name.into());
            ui.set_config_menu_open(true);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_config_menu(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_config_menu_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_request_rename(move |target, title, name| {
            if let Some(ui) = weak.upgrade() {
                ui.set_rename_target(target.clone());
                ui.set_rename_title(title.clone());
                ui.set_rename_name(name.clone());
                ui.set_rename_open(true);
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_rename(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_rename_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        ui.on_save_rename(move |target, name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let mut guard = store.lock().unwrap();
            if let Some(id) = target.as_str().strip_prefix("config:") {
                if let Some(profile) = guard.profiles.iter_mut().find(|profile| profile.id == id) {
                    profile.name = name.to_string();
                }
            } else if target.as_str() == "group:default" {
                guard.default_group_name = name.to_string();
            }
            guard.selected = guard
                .selected_id
                .as_ref()
                .and_then(|selected| guard.profile_by_id(selected))
                .map(|profile| profile.name.clone());
            let _ = save_store(&guard);
            ui.set_groups(group_model(&guard, &pings));
            ui.set_rename_open(false);
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_delete_config(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let mut guard = store.lock().unwrap();
            if !guard.remove_profile(id.as_str()) {
                return;
            }
            if guard.selected_id.is_none() {
                guard.selected_id = guard.profiles.first().map(|profile| profile.id.clone());
            }
            guard.selected = guard
                .selected_id
                .as_ref()
                .and_then(|selected| guard.profile_by_id(selected))
                .map(|profile| profile.name.clone());
            let _ = save_store(&guard);
            ui.set_groups(group_model(&guard, &pings));
            ui.set_selected_id(guard.selected_id.clone().unwrap_or_default().into());
            drop(guard);
            push_log(&ui, &logs, "config deleted".to_string());
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_edit_group(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            if id.as_str() == "default" {
                return;
            }
            let (name, url) = {
                let guard = store.lock().unwrap();
                let Some(node) = guard
                    .profiles
                    .iter()
                    .find(|profile| profile.group_key() == id.as_str())
                else {
                    return;
                };
                (
                    node.group_title(),
                    node.group_url.clone().unwrap_or_default(),
                )
            };
            ui.set_edit_group_id(id.clone());
            ui.set_edit_name(name.into());
            ui.set_edit_url(url.into());
            ui.set_group_menu_open(false);
            ui.set_edit_open(true);
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_edit(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_edit_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_save_group(move |id, name, url| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            let url = url.trim();
            let mut guard = store.lock().unwrap();
            let mut changed = false;
            for profile in &mut guard.profiles {
                if profile.group_key() == id.as_str() {
                    profile.group_name = Some(name.to_string());
                    if !url.is_empty() {
                        profile.group_url = Some(url.to_string());
                    }
                    changed = true;
                }
            }
            if changed {
                let _ = save_store(&guard);
                ui.set_groups(group_model(&guard, &pings));
            }
            drop(guard);
            ui.set_edit_open(false);
            push_log(&ui, &logs, "profile updated".to_string());
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let logs = logs.clone();
        ui.on_refresh_group(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_group_menu_open(false);
            let id = id.to_string();
            let info = {
                let guard = store.lock().unwrap();
                guard
                    .profiles
                    .iter()
                    .find(|profile| profile.group_key() == id)
                    .map(|profile| (profile.group_url.clone(), profile.group_title()))
            };
            let Some((Some(url), old_name)) = info else {
                push_log(
                    &ui,
                    &logs,
                    "this profile has no subscription URL".to_string(),
                );
                return;
            };
            push_log(&ui, &logs, "refreshing subscription…".to_string());

            let weak = weak.clone();
            let store = store.clone();
            let pings = pings.clone();
            let logs = logs.clone();
            std::thread::spawn(move || {
                let result = fetch_subscription(&url);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match result {
                        Ok((new_name, nodes)) if !nodes.is_empty() => {
                            let group_name = new_name.unwrap_or(old_name);
                            let previous = store.lock().unwrap().selected_profile().cloned();
                            let selected_in_group = previous
                                .as_ref()
                                .is_some_and(|profile| profile.group_key() == id);
                            let selected_source = previous
                                .as_ref()
                                .filter(|profile| profile.group_key() == id)
                                .and_then(|profile| profile.source.clone());

                            let mut guard = store.lock().unwrap();
                            let old_sources: HashMap<String, String> = guard
                                .profiles
                                .iter()
                                .filter(|profile| profile.group_key() == id)
                                .filter_map(|profile| {
                                    profile
                                        .source
                                        .clone()
                                        .map(|source| (profile.id.clone(), source))
                                })
                                .collect();
                            let old_order = guard.group_order.get(&id).cloned().unwrap_or_default();
                            guard.profiles.retain(|profile| profile.group_key() != id);
                            let mut first_id = None;
                            let mut keep_id = None;
                            for mut node in nodes {
                                node.group_id = Some(id.clone());
                                node.group_name = Some(group_name.clone());
                                node.group_url = Some(url.clone());
                                if first_id.is_none() {
                                    first_id = Some(node.id.clone());
                                }
                                if selected_source.as_deref() == node.source.as_deref() {
                                    keep_id = Some(node.id.clone());
                                }
                                guard.profiles.push(node);
                            }
                            // Keep the user's custom order across subscription
                            // refreshes by matching stable link sources.
                            let new_by_source: HashMap<String, String> = guard
                                .profiles
                                .iter()
                                .filter(|profile| profile.group_key() == id)
                                .filter_map(|profile| {
                                    profile
                                        .source
                                        .clone()
                                        .map(|source| (source, profile.id.clone()))
                                })
                                .collect();
                            let mut order: Vec<String> = old_order
                                .iter()
                                .filter_map(|old_id| {
                                    old_sources
                                        .get(old_id)
                                        .and_then(|source| new_by_source.get(source).cloned())
                                })
                                .collect();
                            for profile in guard
                                .profiles
                                .iter()
                                .filter(|profile| profile.group_key() == id)
                            {
                                if !order.contains(&profile.id) {
                                    order.push(profile.id.clone());
                                }
                            }
                            guard.group_order.insert(id.clone(), order);
                            if selected_in_group {
                                if let Some(selected) = keep_id.or(first_id) {
                                    guard.selected_id = Some(selected.clone());
                                    guard.selected = guard
                                        .profiles
                                        .iter()
                                        .find(|profile| profile.id == selected)
                                        .map(|profile| profile.name.clone());
                                    ui.set_selected_id(selected.into());
                                }
                            }
                            let _ = save_store(&guard);
                            ui.set_groups(group_model(&guard, &pings));
                            let count = guard
                                .profiles
                                .iter()
                                .filter(|profile| profile.group_key() == id)
                                .count();
                            drop(guard);
                            push_log(&ui, &logs, format!("subscription updated: {count} configs"));
                        }
                        Ok(_) => {
                            push_log(&ui, &logs, "subscription is empty".to_string());
                        }
                        Err(error) => {
                            push_log(&ui, &logs, format!("subscription failed: {error}"));
                        }
                    }
                });
            });
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

fn fetch_subscription(url: &str) -> anyhow::Result<(Option<String>, Vec<config::Profile>)> {
    let response = ureq::get(url)
        .timeout(Duration::from_secs(25))
        .call()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let title = response
        .header("profile-title")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let body = response
        .into_string()
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let mut links: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.starts_with("vless://")
                || line.starts_with("vmess://")
                || line.starts_with("trojan://")
                || line.starts_with("ss://")
        })
        .map(ToOwned::to_owned)
        .collect();

    if links.is_empty() {
        let cleaned: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        for engine in [
            base64::engine::general_purpose::STANDARD,
            base64::engine::general_purpose::STANDARD_NO_PAD,
            base64::engine::general_purpose::URL_SAFE,
            base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            if let Ok(decoded) = base64::Engine::decode(&engine, cleaned.as_bytes()) {
                if let Ok(text) = String::from_utf8(decoded) {
                    links = text
                        .lines()
                        .map(str::trim)
                        .filter(|line| {
                            line.starts_with("vless://")
                                || line.starts_with("vmess://")
                                || line.starts_with("trojan://")
                                || line.starts_with("ss://")
                        })
                        .map(ToOwned::to_owned)
                        .collect();
                    if !links.is_empty() {
                        break;
                    }
                }
            }
        }
    }

    let mut nodes = Vec::new();
    for line in links {
        let Ok(spec) = link::parse(None, &line) else {
            continue;
        };
        let core = if matches!(spec.network.as_str(), "xhttp" | "splithttp") {
            CoreKind::Xray
        } else {
            CoreKind::Xray
        };
        nodes.push(config::Profile::from_spec(&spec, core));
    }

    let title = title.or_else(|| {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(|host| format!("Subscription · {host}")))
    });
    Ok((title, nodes))
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

    let native = !profile.is_proxy();
    if profile.protocol.eq_ignore_ascii_case("direct")
        || profile.address == "local"
        || profile.address.is_empty()
        || profile.port == 0
    {
        set_ping(&ui, store, pings, &id, "—".to_string(), 0);
        return;
    }

    set_ping(&ui, store, pings, &id, "…".to_string(), 1);

    let weak = weak.clone();
    let pings = pings.clone();
    let store = store.clone();
    let runtime = runtime.clone();
    runtime.spawn(async move {
        // Proxies expose a TCP listener, so a warm TCP handshake is the right
        // probe. WireGuard/AmneziaWG and UDP OpenVPN do not; use an ICMP echo
        // to the configured endpoint instead so those profiles also show a
        // latency instead of a dash.
        let result = if native {
            probe::best_native_latency(&profile.address, profile.port, Duration::from_millis(2_000))
                .await
        } else {
            probe::best_tcp_latency(&profile.address, profile.port, Duration::from_millis(2_000))
                .await
        };

        let (text, state) = match result {
            Ok(ms) => {
                #[cfg(target_os = "android")]
                crate::logcat(&format!(
                    "ping {} ({}:{}): {ms} ms",
                    profile.name, profile.address, profile.port
                ));
                (format!("{ms} ms"), latency_state(ms))
            }
            Err(error) => {
                #[cfg(target_os = "android")]
                crate::logcat(&format!(
                    "ping {} ({}:{}): {error}",
                    profile.name, profile.address, profile.port
                ));
                ("timeout".to_string(), 4)
            }
        };

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                set_ping(&ui, &store, &pings, &id, text, state);
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

fn set_ping(
    ui: &AppWindow,
    store: &Arc<Mutex<Store>>,
    pings: &PingState,
    id: &str,
    text: String,
    state: i32,
) {
    pings.lock().unwrap().insert(id.to_string(), (text, state));

    let guard = store.lock().unwrap();
    ui.set_groups(group_model(&guard, pings));
}

/// Enter the yellow "busy" state immediately when a connection starts or the
/// user switches profiles, and clear the previous session's counters and
/// addresses so no stale values stay on screen while the new core comes up.
fn enter_starting_state(ui: &AppWindow, switching: bool) {
    ui.set_connected(false);
    ui.set_busy(true);
    ui.set_status_text(if switching {
        "Switching…".into()
    } else {
        "Starting…".into()
    });
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());
    ui.set_tunnel_ip("".into());
    set_network_text(ui, "", "");
    ui.set_country_flag(Default::default());
    clear_last_network();
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
    ui.set_active_profile(profile.name.clone().into());
    set_active_profile_name(&profile.name);

    if !profile.is_proxy() {
        #[cfg(target_os = "android")]
        {
            let Some(mut request) = profile.tunnel_profile() else {
                push_log(ui, logs, "native profile has no raw config".to_string());
                return;
            };
            if request.config.trim().is_empty() {
                push_log(ui, logs, "native profile has an empty config".to_string());
                return;
            }
            request.accent = settings.accent.clone();
            let json = match serde_json::to_string(&request) {
                Ok(json) => json,
                Err(error) => {
                    push_log(ui, logs, format!("could not encode native tunnel: {error}"));
                    return;
                }
            };
            if switching {
                SWITCHING.store(true, Ordering::SeqCst);
            }
            NATIVE_TUNNEL_ACTIVE.store(true, Ordering::SeqCst);
            engine.stop();
            crate::stop_android_vpn();
            push_log(
                ui,
                logs,
                format!(
                    "starting {} via {} (direct VpnService)",
                    profile.name,
                    profile.transport_label()
                ),
            );
            enter_starting_state(ui, switching);
            crate::start_native_tunnel(&json);
            return;
        }
        #[cfg(not(target_os = "android"))]
        {
            push_log(
                ui,
                logs,
                format!(
                    "{} profiles are Android-only for now",
                    profile.transport_label()
                ),
            );
            return;
        }
    }

    if NATIVE_TUNNEL_ACTIVE.swap(false, Ordering::SeqCst) {
        crate::stop_native_tunnel();
    }

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
    enter_starting_state(ui, switching);
    engine.start(spec);

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
    if NATIVE_TUNNEL_ACTIVE.swap(false, Ordering::SeqCst) {
        crate::stop_native_tunnel();
    }
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
                clear_last_network();
                ui.set_status_text("Disconnected".into());
                ui.set_total_down_text("0 B".into());
                ui.set_total_up_text("0 B".into());
                ui.set_tunnel_ip("".into());
                set_network_text(ui, "", "");
                ui.set_country_flag(Default::default());
                crate::stop_android_vpn();
                let _ = system_proxy::disable();
            }
            EngineState::Starting => {
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Starting…".into());
                ui.set_total_down_text("0 B".into());
                ui.set_total_up_text("0 B".into());
                ui.set_tunnel_ip("".into());
                set_network_text(ui, "", "");
                ui.set_country_flag(Default::default());
            }
            EngineState::Running => {
                SWITCHING.store(false, Ordering::SeqCst);
                ui.set_connected(true);
                ui.set_status_text("Connected".into());
                set_traffic_totals(ui, 0, 0);
                // Keep the yellow "connecting" state visible for a moment even
                // when the core starts very quickly.
                let weak_busy = ui.as_weak();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(450));
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_busy.upgrade() {
                            if ui.get_connected() {
                                ui.set_busy(false);
                            }
                        }
                    });
                });
                if NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst) {
                    refresh_native_exit_ips(ui);
                }
                if !NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst) {
                    let weak = ui.as_weak();
                    let logs = logs.clone();
                    std::thread::spawn(move || {
                        use std::net::{SocketAddr, TcpStream};
                        let port = VPN_SOCKS_PORT.load(Ordering::Relaxed);
                        let addr = SocketAddr::from(([127, 0, 0, 1], port));
                        let mut ready = false;
                        for _ in 0..50 {
                            if TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
                            {
                                ready = true;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }

                        #[cfg(target_os = "android")]
                        if ready {
                            crate::start_android_vpn(port);
                        }

                        // Resolve the exit address before the slower
                        // connectivity probe so the UI can show it as soon
                        // as the proxy answers.
                        if ready {
                            let (fast_v4, fast_v6) = fetch_exit_ips_fast(Some(port));
                            let (ipv4, ipv6) = if fast_v4.is_none() && fast_v6.is_none() {
                                match fetch_exit_ips(port) {
                                    Some((v4, v6)) => (v4, v6),
                                    None => (None, None),
                                }
                            } else {
                                (fast_v4, fast_v6)
                            };
                            if ipv4.is_some() || ipv6.is_some() {
                                let country = ipv4
                                    .or(ipv6)
                                    .and_then(|ip| crate::country::lookup(ip).map(str::to_string));
                                let v4 = ipv4.map(|ip| ip.to_string());
                                let v6 = ipv6.map(|ip| ip.to_string());
                                #[cfg(target_os = "android")]
                                crate::logcat(&format!(
                                    "public exit: {} {} (via socks)",
                                    v4.as_deref().unwrap_or("-"),
                                    v6.as_deref().unwrap_or("-")
                                ));
                                let weak_net = weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    let Some(ui) = weak_net.upgrade() else {
                                        return;
                                    };
                                    if !ui.get_connected()
                                        || NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst)
                                    {
                                        return;
                                    }
                                    set_last_network(
                                        v4.as_deref(),
                                        v6.as_deref(),
                                        country.as_deref(),
                                    );
                                    set_network_text(
                                        &ui,
                                        v4.as_deref().unwrap_or_default(),
                                        v6.as_deref().unwrap_or_default(),
                                    );
                                    ui.set_country_flag(
                                        country.as_deref().map(country_flag).unwrap_or_default(),
                                    );
                                });
                            }
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
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
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
                NATIVE_TUNNEL_ACTIVE.store(false, Ordering::SeqCst);
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_status_text("Core error".into());
                set_network_text(ui, "", "");
                ui.set_country_flag(Default::default());
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
            set_traffic_totals(ui, total_up, total_down);
        }
        EngineEvent::TunnelInfo { local_address } => {
            ui.set_tunnel_ip(local_address.into());
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

fn publish_logs(ui: &AppWindow, logs: &LogBuffer) {
    let buffer = logs.lock().unwrap();
    let items: Vec<SharedString> = buffer
        .iter()
        .map(|line| SharedString::from(line.as_str()))
        .collect();
    ui.set_logs(ModelRc::new(VecModel::from(items)));
}

/// Publish the log model at a bounded rate. Rebuilding a large model on every
/// single line makes the settings ScrollView jump while the user scrolls it.
fn push_log(ui: &AppWindow, logs: &LogBuffer, line: impl Into<String>) {
    {
        let mut buffer = logs.lock().unwrap();
        buffer.push(line.into());
        if buffer.len() > 600 {
            let excess = buffer.len() - 600;
            buffer.drain(0..excess);
        }
    }

    let now = Instant::now();
    let due = {
        let mut last = LOG_LAST_PUBLISH.lock().unwrap();
        let due = last.is_none_or(|at| now.duration_since(at) >= Duration::from_millis(200));
        if due {
            *last = Some(now);
        }
        due
    };
    if due {
        publish_logs(ui, logs);
        return;
    }

    if LOG_FLUSH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let weak = ui.as_weak();
    let logs = logs.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(220));
        LOG_FLUSH_PENDING.store(false, Ordering::SeqCst);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                publish_logs(&ui, &logs);
            }
        });
    });
}

fn prefetch_dns(store: &Arc<Mutex<Store>>, runtime: &tokio::runtime::Handle) {
    let endpoints: Vec<(String, u16)> = {
        let guard = store.lock().unwrap();
        guard
            .profiles
            .iter()
            .filter(|profile| {
                !profile.address.is_empty()
                    && profile.address != "local"
                    && profile.port != 0
                    && !profile.protocol.eq_ignore_ascii_case("direct")
            })
            .map(|profile| (profile.address.clone(), profile.port))
            .collect()
    };
    for (host, port) in endpoints {
        runtime.spawn(async move {
            probe::prefetch_dns(&host, port).await;
        });
    }
}

fn set_traffic_totals(ui: &AppWindow, up: u64, down: u64) {
    ui.set_total_down_text(format_bytes(down).into());
    ui.set_total_up_text(format_bytes(up).into());
}

pub(crate) fn format_bytes(bytes: u64) -> String {
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
    use super::{display_name, group_model, mask_ipv4, mask_ipv6};
    use bamdad_core::config::{self, Store};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use slint::Model;

    #[test]
    fn folds_decorative_unicode_but_keeps_superscripts() {
        assert_eq!(display_name("🇫🇮 | 𝔽𝕀ℕ𝕃𝔸ℕ𝔻²"), "🇫🇮 | FINLAND²");
        assert_eq!(display_name("🇹🇷 | TR > 𝗧𝘂𝗻 [1]"), "🇹🇷 | TR > Tun [1]");
    }

    #[test]
    fn applies_custom_group_order() {
        let mut first = config::default_profile();
        first.id = "first".to_string();
        first.name = "First".to_string();
        let mut second = config::default_profile();
        second.id = "second".to_string();
        second.name = "Second".to_string();
        let mut store = Store {
            profiles: vec![first, second],
            ..Default::default()
        };
        store
            .group_order
            .insert("default".to_string(), vec!["second".into(), "first".into()]);
        let pings = Arc::new(Mutex::new(HashMap::new()));
        let groups = group_model(&store, &pings);
        let nodes = groups.row_data(0).unwrap().nodes;
        assert_eq!(nodes.row_data(0).unwrap().id.as_str(), "second");
        assert_eq!(nodes.row_data(1).unwrap().id.as_str(), "first");
    }

    #[test]
    fn masks_exit_addresses() {
        assert_eq!(mask_ipv4("1.2.3.4"), "1.2.3.***");
        assert_eq!(mask_ipv4(""), "");
        assert_eq!(
            mask_ipv6("2a01:5ec0:1003:8a2e:3c1d:7f2a:9b4e:1f07"),
            "2a01:5ec0:1003:***"
        );
        assert_eq!(mask_ipv6("2001:db8::1"), "2001:db8::***");
        assert_eq!(mask_ipv6(""), "");
    }
}
