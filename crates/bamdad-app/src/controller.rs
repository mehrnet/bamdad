use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};

use bamdad_core::config::{self, load_store, Store};
use bamdad_core::engine::{EngineEvent, EngineHandle, EngineState, LaunchSpec, LogStream};
use bamdad_core::error::CoreError;
use bamdad_core::httpprobe;
use bamdad_core::system_proxy::{self, ProxySettings};
use bamdad_core::{binary, link, paths, probe, CoreKind};

use crate::{AppWindow, GroupRow, NamePart, ProfileRow};

pub(crate) type LogBuffer = Arc<Mutex<Vec<String>>>;
type PingState = Arc<Mutex<HashMap<String, (String, i32)>>>;

static VPN_SOCKS_PORT: AtomicU16 = AtomicU16::new(10808);
static SWITCHING: AtomicBool = AtomicBool::new(false);
static STARTING: AtomicBool = AtomicBool::new(false);
static NATIVE_TUNNEL_ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_IPV4: Mutex<String> = Mutex::new(String::new());
static LAST_IPV6: Mutex<String> = Mutex::new(String::new());
static LAST_COUNTRY: Mutex<String> = Mutex::new(String::new());
static LOG_LAST_PUBLISH: Mutex<Option<Instant>> = Mutex::new(None);
static LOG_FLUSH_PENDING: AtomicBool = AtomicBool::new(false);
static ACTIVE_PROFILE: Mutex<String> = Mutex::new(String::new());
/// Id of the profile the current/last attempt is for, so a deferred switch can
/// tell "picked a different row" from "picked the same row".
static ACTIVE_PROFILE_ID: Mutex<String> = Mutex::new(String::new());
static ACTIVE_ACCENT: Mutex<String> = Mutex::new(String::new());
static CREDENTIALS_TARGET: Mutex<String> = Mutex::new(String::new());
static KILL_SWITCH: AtomicBool = AtomicBool::new(false);
static PENDING_CONNECT: Mutex<Option<String>> = Mutex::new(None);
/// Bumped for every connection attempt. Async work (proxied probes, exit-IP
/// lookups, busy timers, watchdogs) captures it and drops its result when it
/// no longer matches, so a previous session can never paint over the current
/// one while switching.
static SESSION: AtomicU64 = AtomicU64::new(0);
/// When the current busy phase started, for double-click protection.
static BUSY_SINCE: Mutex<Option<Instant>> = Mutex::new(None);
/// Ping results arrive one by one; rebuilding the whole row model for each of
/// them is O(n²) on ping-all. Coalesce into at most one rebuild per 120 ms,
/// with a trailing refresh so the last result is never lost.
static GROUP_REFRESH_AT: Mutex<Option<Instant>> = Mutex::new(None);
static GROUP_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// Profile ids whose latency changed since the last row update.
static PING_DIRTY: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Set by the Kotlin proxy VpnService when it cannot take the VPN slot. With
/// the kill switch on this must arm the sinkhole, because this TUN never
/// existed.
static PROXY_FAILURE: AtomicBool = AtomicBool::new(false);

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
        crate::persist::save(&store);
    }

    let ui = context.ui.clone();
    let store = context.store.clone();
    let pings = context.pings.clone();
    let logs = context.logs.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            let store = store.lock().unwrap();
            publish_groups(&ui, &store, &pings);
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
                    auth_user: None,
                    auth_pass: None,
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

pub(crate) fn set_active_profile_id(id: &str) {
    *ACTIVE_PROFILE_ID.lock().unwrap() = id.to_string();
}

pub(crate) fn active_profile_id() -> String {
    ACTIVE_PROFILE_ID.lock().unwrap().clone()
}

/// Whether a native backend is supposed to be reporting right now. Backends
/// from a previous session can deliver one last DOWN/ERROR after a switch or
/// disconnect; those events must not touch the UI.
pub(crate) fn native_events_expected() -> bool {
    NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst)
}

/// Records a proxy VpnService failure so the error event that follows arms
/// the kill switch (no tunnel exists to hold the slot).
pub(crate) fn proxy_vpn_failed() {
    PROXY_FAILURE.store(true, Ordering::SeqCst);
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
        engine.clone(),
        runtime,
        store.clone(),
        logs.clone(),
        pings.clone(),
    );
    start_size_watch(ui);
    start_bridge(ui.as_weak(), events, logs, store.clone());
    if KILL_SWITCH.load(Ordering::SeqCst) {
        crate::start_kill_switch();
    }

    // Headless visual-test hooks (used by caged Wayland previews).
    if let Ok(page) = std::env::var("BAMDAD_PREVIEW_PAGE") {
        if let Ok(page) = page.parse::<i32>() {
            ui.set_page(page);
        }
    }
    if std::env::var("BAMDAD_PREVIEW_LOGS").is_ok() {
        ui.set_logs_full(true);
    }
    if std::env::var("BAMDAD_PREVIEW_LICENSES").is_ok() {
        ui.set_licenses_open(true);
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
    if std::env::var("BAMDAD_PREVIEW_CONNECTING").is_ok() {
        ui.set_connected(false);
        ui.set_busy(true);
        ui.set_status_text("Connecting…".into());
        ui.set_total_down_text("0 B".into());
        ui.set_total_up_text("0 B".into());
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
    {
        let items: Vec<SharedString> = logs
            .lock()
            .unwrap()
            .iter()
            .map(|line| SharedString::from(line.as_str()))
            .collect();
        ui.set_logs(ModelRc::new(VecModel::from(items)));
    }
    let idle = {
        let snapshot = snapshot.lock().unwrap();
        ui.set_total_up_text(format_bytes(snapshot.total_up).into());
        ui.set_total_down_text(format_bytes(snapshot.total_down).into());
        let mut idle = false;
        match &snapshot.state {
            EngineState::Running => {
                STARTING.store(false, Ordering::SeqCst);
                SWITCHING.store(false, Ordering::SeqCst);
                ui.set_connected(true);
                ui.set_busy(false);
                ui.set_status_text("Connected".into());
                // Only a running session has a public exit to show. Seeding
                // this unconditionally would show the previous session's exit
                // IP and flag after a disconnect happened while the activity
                // was gone.
                seed_last_network(ui);
            }
            EngineState::Starting => {
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Connecting…".into());
            }
            EngineState::Stopping => {
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text("Stopping…".into());
            }
            EngineState::Error(_) => {
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(false);
                ui.set_tunnel_ip("".into());
                set_network_text(ui, "", "");
                ui.set_country_flag(Default::default());
                ui.set_status_text("Core error".into());
                idle = true;
            }
            EngineState::Stopped => {
                if SWITCHING.load(Ordering::SeqCst) || STARTING.load(Ordering::SeqCst) {
                    // A new attempt is already coming up (the snapshot still
                    // says Stopped because the proxy core is between runs).
                    // Paint the transition, but never return early: the
                    // callbacks and event bridge below are what revive this
                    // UI.
                    ui.set_connected(false);
                    ui.set_busy(true);
                    ui.set_status_text(
                        if SWITCHING.load(Ordering::SeqCst) {
                            "Switching…"
                        } else {
                            "Connecting…"
                        }
                        .into(),
                    );
                } else {
                    NATIVE_TUNNEL_ACTIVE.store(false, Ordering::SeqCst);
                    clear_last_network();
                    ui.set_connected(false);
                    ui.set_busy(false);
                    ui.set_status_text("Disconnected".into());
                    set_network_text(ui, "", "");
                    ui.set_country_flag(Default::default());
                    idle = true;
                }
            }
        }
        idle
    };
    if idle && KILL_SWITCH.load(Ordering::SeqCst) {
        // No tunnel owns the VPN slot; hold it with the blackhole so apps
        // cannot fall back to the physical network.
        crate::start_kill_switch();
    }

    setup_callbacks(
        ui,
        engine,
        runtime,
        store.clone(),
        logs.clone(),
        pings.clone(),
    );
    start_size_watch(ui);
    start_bridge(ui.as_weak(), events, logs, store);
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
    let lookup_session = session();
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
            if session() != lookup_session
                || !ui.get_connected()
                || !NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst)
            {
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

/// Called from the Android split-tunneling app picker.
pub(crate) fn split_apps_selected(csv: String) {
    let context = IMPORT_CONTEXT.lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .map(|context| (context.ui.clone(), context.store.clone()))
    });
    let Some((ui, store)) = context else { return };
    let apps: Vec<String> = csv
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    {
        let mut guard = store.lock().unwrap();
        guard.settings.split_apps = apps.clone();
        apply_tunnel_options(&guard.settings);
        crate::persist::save(&guard);
    }
    let text = apps.join(", ");
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_split_apps_text(text.into());
        }
    });
}

fn apply_tunnel_options(settings: &config::AppSettings) {
    let dns: Vec<&str> = settings
        .custom_dns
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    let bypass: Vec<&str> = settings
        .bypass_cidrs
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
    let json = serde_json::json!({
        "dns": dns,
        "split_mode": settings.split_mode,
        "split_apps": settings.split_apps,
        "bypass": bypass,
    })
    .to_string();
    crate::configure_tunnel(&json);
}

fn seed(ui: &AppWindow, store: &Arc<Mutex<Store>>, pings: &PingState) {
    let store = store.lock().unwrap();

    publish_groups(&ui, &store, pings);
    ui.set_selected_id(store.selected_id.clone().unwrap_or_default().into());
    ui.set_mobile(cfg!(target_os = "android"));
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());
    ui.set_socks_port(store.settings.socks_port.to_string().into());
    ui.set_http_port(store.settings.http_port.to_string().into());
    ui.set_system_proxy(store.settings.system_proxy);
    ui.set_kill_switch(store.settings.kill_switch);
    ui.set_custom_dns(store.settings.custom_dns.clone().into());
    ui.set_bypass_cidrs(store.settings.bypass_cidrs.clone().into());
    ui.set_split_mode(store.settings.split_mode.clone().into());
    ui.set_split_apps_text(store.settings.split_apps.join(", ").into());
    KILL_SWITCH.store(store.settings.kill_switch, Ordering::SeqCst);
    apply_tunnel_options(&store.settings);
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
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_xray_version(core_version_name(CoreKind::Xray).into());
    {
        let xray = core_version_name(CoreKind::Xray);
        let singbox = core_version_name(CoreKind::SingBox);
        ui.set_licenses(ModelRc::new(VecModel::from(crate::licenses::rows(
            env!("CARGO_PKG_VERSION"),
            &xray,
            &singbox,
        ))));
    }
    ui.set_singbox_version(core_version_name(CoreKind::SingBox).into());
    #[cfg(target_os = "android")]
    {
        ui.set_wireguard_version(crate::android_wireguard_version().into());
        ui.set_openvpn_version(crate::android_openvpn_version().into());
    }
    #[cfg(not(target_os = "android"))]
    {
        ui.set_wireguard_version("Android only".into());
        ui.set_openvpn_version("Android only".into());
    }
}

fn short_version(text: &str) -> String {
    text.split_whitespace()
        .find(|part| part.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
        .map(|part| {
            part.trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '-'))
                .to_string()
        })
        .unwrap_or_else(|| text.to_string())
}

fn core_version_name(kind: CoreKind) -> String {
    match binary::resolve_kind(kind) {
        Some(path) => match binary::version(&path) {
            Ok(version) => short_version(&version),
            Err(error) => format!("error ({error})"),
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
                if let Some(ms) = latency_ms(text) {
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

/// The group list currently shown in the UI, kept so a latency result can
/// update one row instead of rebuilding every row of every group.
struct GroupsModel {
    model: ModelRc<GroupRow>,
    rows: Vec<ModelRc<ProfileRow>>,
    /// Profile id -> (group index, row index) in [model].
    positions: HashMap<String, (usize, usize)>,
}

thread_local! {
    static GROUP_MODEL: RefCell<Option<GroupsModel>> = RefCell::new(None);
}

/// Builds the full model, publishes it, and keeps the row index for targeted
/// ping updates.
fn publish_groups(ui: &AppWindow, store: &Store, pings: &PingState) {
    let built = group_model(store, pings);
    ui.set_groups(built.model.clone());
    GROUP_MODEL.with(|slot| *slot.borrow_mut() = Some(built));
}

fn group_model(store: &Store, pings: &PingState) -> GroupsModel {
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

    let mut rows = Vec::with_capacity(groups.len());
    let mut row_models = Vec::with_capacity(groups.len());
    let mut positions: HashMap<String, (usize, usize)> = HashMap::new();

    for (group_index, (key, title, nodes)) in groups.into_iter().enumerate() {
        let expanded = GROUP_STATE.with(|state| {
            *state
                .borrow_mut()
                .entry(key.clone())
                .or_insert(key == "default")
        });
        let mut nodes = nodes;
        if let Some(order) = store.group_order.get(&key) {
            let order_positions: HashMap<&str, usize> = order
                .iter()
                .enumerate()
                .map(|(position, id)| (id.as_str(), position))
                .collect();
            nodes.sort_by_key(|profile| {
                order_positions
                    .get(profile.id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
        let node_rows: Vec<ProfileRow> = nodes
            .iter()
            .enumerate()
            .map(|(row_index, profile)| {
                positions.insert(profile.id.clone(), (group_index, row_index));
                node_row(profile, &pings, store.selected_id.as_deref())
            })
            .collect();
        let (ping, ping_state) = group_ping(&nodes, &pings);
        let rtl = is_rtl_name(&title);
        let display_title = display_name(&title);
        let row_model: ModelRc<ProfileRow> = ModelRc::new(VecModel::from(node_rows));
        row_models.push(row_model.clone());
        rows.push(GroupRow {
            id: key.as_str().into(),
            name: display_title.as_str().into(),
            kind: if key == "default" { "DEFAULT" } else { "SUB" }.into(),
            expanded,
            deletable: key != "default",
            ping: ping.into(),
            ping_state,
            nodes: row_model,
            rtl,
        });
    }

    GroupsModel {
        model: ModelRc::new(VecModel::from(rows)),
        rows: row_models,
        positions,
    }
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
                // Ignore an accidental second tap right after the first one:
                // before, a slow acknowledgement meant the second tap silently
                // cancelled the connection the user just started.
                let elapsed = BUSY_SINCE
                    .lock()
                    .ok()
                    .and_then(|stamp| *stamp)
                    .map(|started| started.elapsed());
                if should_ignore_toggle(ui.get_busy(), ui.get_connected(), elapsed) {
                    return;
                }
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
                crate::persist::save(&guard);
                profile
            };

            let guard = store.lock().unwrap();
            publish_groups(&ui, &guard, &pings);
            drop(guard);

            ui.set_selected_id(selected.id.as_str().into());

            if ui.get_connected() {
                let message = format!(
                    "switching to {} ({}) without dropping the tunnel",
                    selected.name,
                    selected.core.label()
                );
                #[cfg(target_os = "android")]
                crate::logcat(&message);
                push_log(&ui, &logs, message);
                start_connection(&ui, &store, &engine, &logs, true);
            } else if ui.get_busy() {
                // Mid-connect. A pick must never look ignored: when the
                // current server is not answering, waiting for the timeout
                // (or for a tunnel that may never come up) is exactly when
                // the user tries another profile. Cancel that attempt and
                // start the chosen one instead.
                if active_profile_id() == selected.id {
                    push_log(
                        &ui,
                        &logs,
                        format!("already connecting to {}", selected.name),
                    );
                } else {
                    push_log(
                        &ui,
                        &logs,
                        format!("switching to {} while connecting", selected.name),
                    );
                    start_connection(&ui, &store, &engine, &logs, true);
                }
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
                    crate::persist::save(&guard);
                }
                let guard = store.lock().unwrap();
                publish_groups(&ui, &guard, &pings);
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
                            auth_user: None,
                            auth_pass: None,
                        };
                        {
                            let mut guard = store.lock().unwrap();
                            guard.add_profile(profile);
                            crate::persist::save(&guard);
                        }
                        let guard = store.lock().unwrap();
                        publish_groups(&ui, &guard, &pings);
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
                        crate::persist::save(&guard);
                    }

                    let guard = store.lock().unwrap();
                    publish_groups(&ui, &guard, &pings);
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
        ui.on_save_settings(
            move |socks,
                  http,
                  system_proxy,
                  custom_dns,
                  split_mode,
                  split_apps,
                  bypass_cidrs,
                  tunnel_routes| {
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
                    guard.settings.custom_dns = custom_dns.trim().to_string();
                    guard.settings.split_mode = match split_mode.as_str() {
                        "allow" | "exclude" => split_mode.to_string(),
                        _ => "off".to_string(),
                    };
                    guard.settings.split_apps = split_apps
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .collect();
                    guard.settings.bypass_cidrs = bypass_cidrs.trim().to_string();
                    guard.settings.tunnel_routes = tunnel_routes.trim().to_string();
                    apply_tunnel_options(&guard.settings);
                    crate::persist::save(&guard);
                }

                let proxy = ProxySettings {
                    socks_port,
                    http_port,
                };
                // `gsettings`/`netsh`/`networksetup` are separate processes;
                // saving settings must not wait for them on the UI thread.
                let connected = ui.get_connected();
                let weak = ui.as_weak();
                let thread_logs = logs.clone();
                std::thread::spawn(move || {
                    if system_proxy && connected {
                        if let Err(error) = system_proxy::enable(proxy) {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    push_log(&ui, &thread_logs, format!("system proxy: {error}"));
                                }
                            });
                        }
                    } else if !system_proxy {
                        let _ = system_proxy::disable();
                    }
                });

                ui.set_socks_port(socks_port.to_string().into());
                ui.set_http_port(http_port.to_string().into());
                push_log(
                    &ui,
                    &logs,
                    format!("settings saved (socks {socks_port}, http {http_port})"),
                );
            },
        );
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
            publish_groups(&ui, &guard, &pings);
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
                crate::persist::save(&guard);
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
                crate::persist::save(&guard);
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
        ui.on_pick_split_apps(move |current| {
            if weak.upgrade().is_some() {
                crate::pick_split_apps(current.as_str());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_set_kill_switch(move |enabled| {
            {
                let mut guard = store.lock().unwrap();
                guard.settings.kill_switch = enabled;
                crate::persist::save(&guard);
            }
            KILL_SWITCH.store(enabled, Ordering::SeqCst);
            if let Some(ui) = weak.upgrade() {
                ui.set_kill_switch(enabled);
                if !enabled {
                    crate::stop_kill_switch();
                } else if !ui.get_connected() && !ui.get_busy() {
                    crate::start_kill_switch();
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let logs = logs.clone();
        ui.on_set_tunnel_mode(move |enabled| {
            {
                let mut guard = store.lock().unwrap();
                guard.settings.tunnel_mode = enabled;
                crate::persist::save(&guard);
            }
            #[cfg(not(target_os = "android"))]
            if !enabled {
                stop_desktop_tunnel_async();
            }
            let probe: Result<(), String> = if enabled {
                #[cfg(target_os = "android")]
                {
                    Ok(())
                }
                #[cfg(not(target_os = "android"))]
                {
                    crate::desktop_tun::probe()
                }
            } else {
                Ok(())
            };
            if let Some(ui) = weak.upgrade() {
                ui.set_tunnel_mode(enabled);
                if let Err(error) = &probe {
                    push_log(&ui, &logs, format!("tunnel mode unavailable: {error}"));
                    ui.set_status_text("Tunnel mode unavailable".into());
                }
                push_log(
                    &ui,
                    &logs,
                    if enabled {
                        "tunnel mode enabled; the desktop tunnel starts on the next connect"
                            .to_string()
                    } else {
                        "tunnel mode disabled".to_string()
                    },
                );
            }
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
                            crate::persist::save(&guard);
                            publish_groups(&ui, &guard, &pings);
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
            begin_ping_batch(&weak, &runtime, &store, &pings, vec![id.to_string()]);
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
            publish_groups(&ui, &guard, &pings);
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
            begin_ping_batch(&weak, &runtime, &store, &pings, ids);
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
                    crate::persist::save(&guard);
                }
                removed
            };
            if removed {
                let guard = store.lock().unwrap();
                publish_groups(&ui, &guard, &pings);
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
                crate::persist::save(&guard);
                publish_groups(&ui, &guard, &pings);
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
                    Some((text, state)) if *state == 2 || *state == 3 => {
                        (0u8, latency_ms(text).unwrap_or(u64::MAX))
                    }
                    Some((_, 1)) => (1u8, u64::MAX),
                    Some((_, 4)) => (2u8, u64::MAX),
                    _ => (3u8, u64::MAX),
                };
                (rank, ms, *index)
            });
            let ids: Vec<String> = nodes.into_iter().map(|(_, id)| id).collect();
            guard.group_order.insert(group_id.to_string(), ids);
            crate::persist::save(&guard);
            drop(ping_map);
            publish_groups(&ui, &guard, &pings);
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
                crate::persist::save(&guard);
                publish_groups(&ui, &guard, &pings);
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
            let prepared = {
                let guard = store.lock().unwrap();
                let Some(profile) = guard.profile_by_id(id.as_str()) else {
                    return;
                };
                (
                    profile.name.clone(),
                    profile.transport == bamdad_core::Transport::OpenVpn,
                )
            };
            ui.set_config_menu_id(id.clone());
            ui.set_config_menu_name(prepared.0.into());
            ui.set_config_menu_credentials(prepared.1);
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
        let store = store.clone();
        ui.on_open_credentials(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            open_credentials_dialog(&ui, &store, id.as_str());
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_close_credentials(move || {
            *PENDING_CONNECT.lock().unwrap() = None;
            if let Some(ui) = weak.upgrade() {
                ui.set_credentials_open(false);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        let pings = pings.clone();
        let engine = engine.clone();
        let logs = logs.clone();
        ui.on_save_credentials(move |user, pass| {
            let Some(ui) = weak.upgrade() else { return };
            let target = CREDENTIALS_TARGET.lock().unwrap().clone();
            {
                let mut guard = store.lock().unwrap();
                let user = user.trim().to_string();
                let pass = pass.to_string();
                if let Some(profile) = guard.profiles.iter_mut().find(|p| p.id == target) {
                    profile.auth_user = if user.is_empty() { None } else { Some(user) };
                    profile.auth_pass = if pass.is_empty() { None } else { Some(pass) };
                }
                crate::persist::save(&guard);
                publish_groups(&ui, &guard, &pings);
            }
            ui.set_credentials_open(false);
            if PENDING_CONNECT.lock().unwrap().take().as_deref() == Some(target.as_str()) {
                let ready = store
                    .lock()
                    .unwrap()
                    .profile_by_id(&target)
                    .map(|profile| !openvpn_needs_auth(profile))
                    .unwrap_or(false);
                if ready {
                    start_connection(&ui, &store, &engine, &logs, false);
                }
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
            crate::persist::save(&guard);
            publish_groups(&ui, &guard, &pings);
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
            crate::persist::save(&guard);
            publish_groups(&ui, &guard, &pings);
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
                crate::persist::save(&guard);
                publish_groups(&ui, &guard, &pings);
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
                            let mut first_id = None;
                            let mut keep_id = None;
                            let mut replacement = Vec::with_capacity(nodes.len());
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
                                replacement.push(node);
                            }
                            // Replace in place: appending would move the whole
                            // group to the end of the list on every refresh.
                            guard.replace_group(&id, replacement);
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
                            crate::persist::save(&guard);
                            publish_groups(&ui, &guard, &pings);
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
            begin_ping_batch(&weak, &runtime, &store, &pings, ids);
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
        // Links are rendered for Xray; XHTTP/splithttp require it anyway.
        nodes.push(config::Profile::from_spec(&spec, CoreKind::Xray));
    }

    let title = title.or_else(|| {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(|host| format!("Subscription · {host}")))
    });
    Ok((title, nodes))
}

#[derive(Clone, Copy)]
enum PingMode {
    Skip,
    /// Endpoint round trip only: an inactive native config has no tunnel to
    /// send an HTTP probe through.
    Endpoint,
    Proxy,
    /// HTTP through the active proxy tunnel.
    ActiveProxy,
    /// HTTP through the active native tunnel.
    ActiveNative,
}

fn ping_entry(id: String, result: Result<u64, CoreError>) -> (String, String, i32) {
    match result {
        Ok(ms) => {
            let ms = ms.max(1);
            (id, format!("{ms} ms"), latency_state(ms))
        }
        Err(_) => (id, "timeout".to_string(), 4),
    }
}

/// Endpoint latency (ICMP/TCP) for configs whose tunnel is not active. A
/// web-port fallback is a path estimate rather than the transport's own round
/// trip, so it is marked as such.
fn endpoint_ping_entry(
    id: String,
    result: Result<probe::Latency, CoreError>,
) -> (String, String, i32) {
    match result {
        Ok(latency) => {
            let ms = latency.ms.max(1);
            let text = if latency.approximate() {
                format!("≈ {ms} ms")
            } else {
                format!("{ms} ms")
            };
            (id, text, latency_state(ms))
        }
        Err(_) => (id, "timeout".to_string(), 4),
    }
}

/// Probe one or more profiles. Proxy profiles are merged into shared core
/// instances (v2rayNG/radar-node style) so a subscription with hundreds of
/// configs is measured with a handful of process starts instead of one per
/// config.
fn begin_ping_batch(
    weak: &Weak<AppWindow>,
    runtime: &tokio::runtime::Handle,
    store: &Arc<Mutex<Store>>,
    pings: &PingState,
    ids: Vec<String>,
) {
    let Some(ui) = weak.upgrade() else { return };
    let (profiles, settings, selected_id) = {
        let guard = store.lock().unwrap();
        let profiles: Vec<config::Profile> = ids
            .iter()
            .filter_map(|id| guard.profile_by_id(id).cloned())
            .collect();
        (profiles, guard.settings.clone(), guard.selected_id.clone())
    };
    if profiles.is_empty() {
        return;
    }

    let connected = ui.get_connected();
    let native_active = NATIVE_TUNNEL_ACTIVE.load(Ordering::SeqCst);
    let mut plan: Vec<(config::Profile, PingMode)> = Vec::new();
    let mut waiting = Vec::new();
    let mut dashes = Vec::new();

    for profile in profiles {
        let mode = if profile.protocol.eq_ignore_ascii_case("direct")
            || profile.address == "local"
            || profile.address.is_empty()
            || profile.port == 0
        {
            PingMode::Skip
        } else if profile.is_proxy() {
            if connected && selected_id.as_deref() == Some(profile.id.as_str()) && !native_active {
                PingMode::ActiveProxy
            } else if native_active {
                // A probe core started now would be routed through the native
                // tunnel; keep the measurement on the physical path.
                PingMode::Endpoint
            } else {
                PingMode::Proxy
            }
        } else if connected && selected_id.as_deref() == Some(profile.id.as_str()) && native_active
        {
            PingMode::ActiveNative
        } else {
            // Native configs cannot be HTTP-probed until their tunnel owns the
            // VPN slot, and taking it would drop the current connection.
            PingMode::Endpoint
        };

        match mode {
            PingMode::Skip => dashes.push(profile.id),
            _ => {
                waiting.push(profile.id.clone());
                plan.push((profile, mode));
            }
        }
    }

    {
        let mut map = pings.lock().unwrap();
        for id in &dashes {
            map.insert(id.clone(), ("—".to_string(), 0));
        }
        for id in &waiting {
            map.insert(id.clone(), ("…".to_string(), 1));
        }
        drop(map);
        let guard = store.lock().unwrap();
        publish_groups(&ui, &guard, pings);
    }

    let weak = weak.clone();
    let pings = pings.clone();
    let store = store.clone();
    runtime.spawn(async move {
        let mut results: Vec<(String, String, i32)> = Vec::new();
        let limit = Arc::new(tokio::sync::Semaphore::new(16));
        let mut handles = Vec::new();
        let mut batch = Vec::new();

        for (profile, mode) in plan {
            match mode {
                PingMode::Proxy => batch.push(httpprobe::BatchJob {
                    id: profile.id.clone(),
                    core: profile.core,
                    config: profile.config.clone(),
                }),
                PingMode::ActiveProxy => {
                    let permit = limit.clone();
                    let port = settings.socks_port;
                    handles.push(tokio::spawn(async move {
                        let _permit = permit.acquire().await.ok();
                        let result = tokio::task::spawn_blocking(move || {
                            httpprobe::measure(
                                httpprobe::Route::Socks(port),
                                Duration::from_secs(5),
                            )
                        })
                        .await
                        .unwrap_or_else(|error| {
                            Err(CoreError::Spawn(format!("probe task failed: {error}")))
                        });
                        ping_entry(profile.id, result)
                    }));
                }
                PingMode::ActiveNative => {
                    let permit = limit.clone();
                    handles.push(tokio::spawn(async move {
                        let _permit = permit.acquire().await.ok();
                        let result = tokio::task::spawn_blocking(|| {
                            httpprobe::measure(httpprobe::Route::Direct, Duration::from_secs(5))
                        })
                        .await
                        .unwrap_or_else(|error| {
                            Err(CoreError::Spawn(format!("probe task failed: {error}")))
                        });
                        ping_entry(profile.id, result)
                    }));
                }
                PingMode::Endpoint => {
                    let permit = limit.clone();
                    handles.push(tokio::spawn(async move {
                        let _permit = permit.acquire().await.ok();
                        let result = probe::best_native_latency(
                            &profile.address,
                            profile.port,
                            Duration::from_millis(2_000),
                        )
                        .await;
                        endpoint_ping_entry(profile.id, result)
                    }));
                }
                PingMode::Skip => {}
            }
        }

        if !batch.is_empty() {
            // Paint each row as its probe finishes instead of holding the
            // whole subscription until the slowest config answers.
            let stream_weak = weak.clone();
            let stream_store = store.clone();
            let stream_pings = pings.clone();
            let on_result = move |item: httpprobe::BatchResult| {
                let weak = stream_weak.clone();
                let store = stream_store.clone();
                let pings = stream_pings.clone();
                let entry = match item.latency_ms {
                    Some(ms) => ping_entry(item.id, Ok(ms)),
                    None => (item.id, "timeout".to_string(), 4),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    set_ping(&ui, &store, &pings, &entry.0, entry.1, entry.2);
                });
            };
            let _ = tokio::task::spawn_blocking(move || {
                httpprobe::measure_batch_stream(batch, Duration::from_millis(4_000), on_result);
            })
            .await;
        }

        for handle in handles {
            if let Ok(entry) = handle.await {
                results.push(entry);
            }
        }

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                for (id, text, state) in results {
                    pings.lock().unwrap().insert(id.clone(), (text, state));
                    mark_ping_dirty(&id);
                }
                refresh_groups(&ui, &store, &pings);
            }
        });
    });
}

fn mark_ping_dirty(id: &str) {
    if let Ok(mut dirty) = PING_DIRTY.lock() {
        if !dirty.iter().any(|entry| entry == id) {
            dirty.push(id.to_string());
        }
    }
}

/// Applies latency changes to the rows that changed. A full model rebuild is
/// O(all profiles) and ran up to eight times a second while Ping all streamed
/// results; `set_row_data` touches one row and its group header instead.
fn apply_ping_updates(ui: &AppWindow, store: &Arc<Mutex<Store>>, pings: &PingState) {
    let dirty: Vec<String> = {
        let mut dirty = PING_DIRTY.lock().unwrap();
        std::mem::take(&mut *dirty)
    };
    if dirty.is_empty() {
        return;
    }

    let cached = GROUP_MODEL.with(|slot| slot.borrow().is_some());
    let Ok(guard) = store.lock() else {
        return;
    };
    if !cached {
        publish_groups(ui, &guard, pings);
        return;
    }

    let Ok(map) = pings.lock() else {
        return;
    };
    GROUP_MODEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(cache) = slot.as_mut() else {
            return;
        };

        let mut by_group: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
        for id in dirty {
            if let Some(&(group, row)) = cache.positions.get(&id) {
                by_group.entry(group).or_default().push((row, id));
            }
        }

        for (group_index, rows) in by_group {
            for (row_index, id) in &rows {
                let Some(profile) = guard.profile_by_id(id) else {
                    continue;
                };
                let row = node_row(profile, &map, guard.selected_id.as_deref());
                if let Some(model) = cache.rows.get(group_index) {
                    model.set_row_data(*row_index, row);
                }
            }

            let Some(mut group_row) = cache.model.row_data(group_index) else {
                continue;
            };
            let key = group_row.id.to_string();
            let nodes: Vec<&config::Profile> = guard
                .profiles
                .iter()
                .filter(|profile| profile.group_key() == key)
                .collect();
            let (ping, ping_state) = group_ping(&nodes, &map);
            group_row.ping = ping.into();
            group_row.ping_state = ping_state;
            cache.model.set_row_data(group_index, group_row);
        }
    });
}

/// Coalesces bursts of latency results and repaints only the affected rows.
fn refresh_groups(ui: &AppWindow, store: &Arc<Mutex<Store>>, pings: &PingState) {
    let now = Instant::now();
    let throttled = {
        let last = GROUP_REFRESH_AT.lock().unwrap();
        last.is_some_and(|at| now.duration_since(at) < Duration::from_millis(120))
    };
    if throttled {
        // Make sure a refresh still happens after the window, otherwise the
        // final ping result could stay invisible.
        if !GROUP_REFRESH_PENDING.swap(true, Ordering::SeqCst) {
            let weak = ui.as_weak();
            let store = store.clone();
            let pings = pings.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(130));
                GROUP_REFRESH_PENDING.store(false, Ordering::SeqCst);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    apply_ping_updates(&ui, &store, &pings);
                    *GROUP_REFRESH_AT.lock().unwrap() = Some(Instant::now());
                });
            });
        }
        return;
    }
    *GROUP_REFRESH_AT.lock().unwrap() = Some(now);
    apply_ping_updates(ui, store, pings);
}

/// Parses both `120 ms` and the endpoint-estimate form `≈ 120 ms`.
fn latency_ms(text: &str) -> Option<u64> {
    text.trim_start_matches('≈')
        .trim()
        .strip_suffix(" ms")?
        .trim()
        .parse()
        .ok()
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
    mark_ping_dirty(id);
    refresh_groups(ui, store, pings);
}

/// A second tap this soon after a start is an accidental double-click, not a
/// request to cancel: before the click was acknowledged instantly, the second
/// tap of a double-click silently killed the connection.
fn should_ignore_toggle(busy: bool, connected: bool, since: Option<Duration>) -> bool {
    busy && !connected && since.is_some_and(|elapsed| elapsed < Duration::from_millis(700))
}

/// Fails a start that never reaches `Running`: without this the bar can sit in
/// "Connecting…" forever, which is exactly when users start double-clicking.
fn spawn_start_watchdog(ui: &AppWindow, engine: &EngineHandle, logs: &LogBuffer, native: bool) {
    let weak = ui.as_weak();
    let logs = logs.clone();
    let engine = engine.clone();
    let started = session();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if session() != started || ui.get_connected() || !ui.get_busy() {
                return;
            }
            STARTING.store(false, Ordering::SeqCst);
            SWITCHING.store(false, Ordering::SeqCst);
            *BUSY_SINCE.lock().unwrap() = None;
            push_log(&ui, &logs, "connection timed out".to_string());
            ui.set_busy(false);
            ui.set_status_text("Connection timed out".into());
            if native {
                // Keep no gate open for a backend that is being abandoned:
                // a late UP would otherwise turn the bar green after the
                // timeout was already reported.
                NATIVE_TUNNEL_ACTIVE.store(false, Ordering::SeqCst);
                crate::stop_native_tunnel();
                if KILL_SWITCH.load(Ordering::SeqCst) {
                    crate::start_kill_switch();
                }
            } else {
                engine.stop();
            }
        });
    });
}

/// Leave the busy state when a start is aborted before the core launches.
/// Restores the previous visual state because an aborted switch leaves the
/// running tunnel untouched.
fn cancel_starting(ui: &AppWindow, was_connected: bool) {
    SWITCHING.store(false, Ordering::SeqCst);
    STARTING.store(false, Ordering::SeqCst);
    *BUSY_SINCE.lock().unwrap() = None;
    ui.set_busy(false);
    ui.set_connected(was_connected);
    ui.set_status_text(if was_connected {
        "Connected".into()
    } else {
        "Disconnected".into()
    });
    // Entering the busy state stops the sinkhole; an aborted start (no profile,
    // credentials dialog) must hand the VPN slot back when disconnected.
    if !was_connected && KILL_SWITCH.load(Ordering::SeqCst) {
        crate::start_kill_switch();
    }
}

/// Enter the yellow "busy" state immediately when a connection starts or the
/// user switches profiles, and clear the previous session's counters and
/// addresses so no stale values stay on screen while the new core comes up.
/// Starts the privileged desktop tunnel once the proxy accepts on [port].
#[cfg(not(target_os = "android"))]
fn start_desktop_tunnel(ui: &AppWindow, logs: &LogBuffer, store: &Arc<Mutex<Store>>, port: u16) {
    let (enabled, bypass, dns, routes) = {
        let guard = store.lock().unwrap();
        let enabled = guard.settings.tunnel_mode;
        let bypass = guard
            .selected_profile()
            .map(|profile| crate::desktop_tun::bypass_for(&profile.address, profile.port))
            .unwrap_or_default();
        let mut dns: Vec<String> = guard
            .settings
            .custom_dns
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if dns.is_empty() {
            dns = vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()];
        }
        let routes: Vec<String> = guard
            .settings
            .tunnel_routes
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        (enabled, bypass, dns, routes)
    };
    if !enabled {
        return;
    }
    // Starting the helper waits for it to report ready; keep that off the UI
    // thread and post the result back.
    let weak = ui.as_weak();
    let logs = logs.clone();
    let started = session();
    std::thread::spawn(move || {
        let result = crate::desktop_tun::start(port, &bypass, &dns, &routes);
        let _ = slint::invoke_from_event_loop(move || {
            if session() != started {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(message) => push_log(&ui, &logs, message),
                Err(error) => {
                    push_log(&ui, &logs, format!("tunnel mode: {error}"));
                    if ui.get_connected() {
                        ui.set_status_text("Connected · tunnel failed".into());
                    }
                }
            }
        });
    });
}

/// Restores desktop routing when the tunnel is no longer wanted.
#[cfg(not(target_os = "android"))]
fn stop_desktop_tunnel() {
    crate::desktop_tun::stop();
}

/// `desktop_tun::stop` waits up to a few seconds for the helper to exit, so it
/// must never run on the Slint thread.
#[cfg(not(target_os = "android"))]
fn stop_desktop_tunnel_async() {
    std::thread::spawn(stop_desktop_tunnel);
}

fn new_session() -> u64 {
    SESSION.fetch_add(1, Ordering::SeqCst) + 1
}

fn session() -> u64 {
    SESSION.load(Ordering::SeqCst)
}

fn enter_starting_state(ui: &AppWindow, switching: bool) {
    let _ = new_session();
    *BUSY_SINCE.lock().unwrap() = Some(Instant::now());
    STARTING.store(true, Ordering::SeqCst);
    ui.set_connected(false);
    ui.set_busy(true);
    ui.set_status_text(if switching {
        "Switching…".into()
    } else {
        "Connecting…".into()
    });
    ui.set_total_down_text("0 B".into());
    ui.set_total_up_text("0 B".into());
    ui.set_tunnel_ip("".into());
    set_network_text(ui, "", "");
    ui.set_country_flag(Default::default());
    clear_last_network();
}

fn openvpn_needs_auth(profile: &config::Profile) -> bool {
    if profile.transport != bamdad_core::Transport::OpenVpn {
        return false;
    }
    let Some(raw) = profile.raw_config.as_deref() else {
        return false;
    };
    if raw.contains("<auth-user-pass>") {
        return false;
    }
    let has_directive = raw
        .lines()
        .any(|line| line.trim_start().starts_with("auth-user-pass"));
    if !has_directive {
        return false;
    }
    let empty = |value: Option<&String>| value.map(|value| value.trim().is_empty()).unwrap_or(true);
    empty(profile.auth_user.as_ref()) || empty(profile.auth_pass.as_ref())
}

fn open_credentials_dialog(ui: &AppWindow, store: &Arc<Mutex<Store>>, id: &str) {
    let prepared = {
        let guard = store.lock().unwrap();
        let Some(profile) = guard.profile_by_id(id) else {
            return;
        };
        (
            profile.id.clone(),
            profile.name.clone(),
            profile.auth_user.clone().unwrap_or_default(),
            profile.auth_pass.clone().unwrap_or_default(),
        )
    };
    *CREDENTIALS_TARGET.lock().unwrap() = prepared.0;
    ui.set_credentials_name(prepared.1.into());
    ui.set_credentials_user(prepared.2.into());
    ui.set_credentials_pass(prepared.3.into());
    ui.set_credentials_open(true);
}

fn start_connection(
    ui: &AppWindow,
    store: &Arc<Mutex<Store>>,
    engine: &EngineHandle,
    logs: &LogBuffer,
    switching: bool,
) {
    // Flip the bar into the yellow connecting state before locking the store
    // or doing any engine work: the click must be acknowledged on the next
    // frame even when another thread holds the store lock.
    let was_connected = ui.get_connected();
    if switching {
        SWITCHING.store(true, Ordering::SeqCst);
    }
    enter_starting_state(ui, switching);

    let (settings, profile) = {
        let guard = store.lock().unwrap();
        (guard.settings.clone(), guard.selected_profile().cloned())
    };

    let Some(profile) = profile else {
        push_log(ui, logs, "no profile selected".to_string());
        cancel_starting(ui, was_connected);
        return;
    };
    ui.set_active_profile(profile.name.clone().into());
    set_active_profile_name(&profile.name);
    set_active_profile_id(&profile.id);

    // OpenVPN profiles that need auth-user-pass but have neither an inline
    // block nor saved credentials ask before anything starts; the bar falls
    // back to its previous state while the dialog is open.
    if openvpn_needs_auth(&profile) {
        cancel_starting(ui, was_connected);
        *PENDING_CONNECT.lock().unwrap() = Some(profile.id.clone());
        open_credentials_dialog(ui, store, &profile.id);
        return;
    }

    if !profile.is_proxy() {
        #[cfg(target_os = "android")]
        {
            let Some(mut request) = profile.tunnel_profile() else {
                push_log(ui, logs, "native profile has no raw config".to_string());
                cancel_starting(ui, was_connected);
                return;
            };
            if request.config.trim().is_empty() {
                push_log(ui, logs, "native profile has an empty config".to_string());
                cancel_starting(ui, was_connected);
                return;
            }
            request.accent = settings.accent.clone();
            let json = match serde_json::to_string(&request) {
                Ok(json) => json,
                Err(error) => {
                    push_log(ui, logs, format!("could not encode native tunnel: {error}"));
                    cancel_starting(ui, was_connected);
                    return;
                }
            };
            NATIVE_TUNNEL_ACTIVE.store(true, Ordering::SeqCst);
            crate::reset_native_event_state();
            engine.stop();
            crate::stop_android_vpn();
            // Hand the single VPN slot over as late as possible.
            crate::stop_kill_switch();
            push_log(
                ui,
                logs,
                format!(
                    "starting {} via {} (direct VpnService)",
                    profile.name,
                    profile.transport_label()
                ),
            );
            crate::start_native_tunnel(&json);
            spawn_start_watchdog(ui, engine, logs, true);
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
            cancel_starting(ui, was_connected);
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
        cancel_starting(ui, was_connected);
        return;
    };

    let api_port = match config::free_port() {
        Ok(port) => port,
        Err(error) => {
            push_log(ui, logs, format!("could not reserve API port: {error}"));
            cancel_starting(ui, was_connected);
            return;
        }
    };

    let prepared = match config::prepare_config(profile.core, &profile.config, &settings, api_port)
    {
        Ok(config) => config,
        Err(error) => {
            push_log(ui, logs, format!("invalid config: {error}"));
            cancel_starting(ui, was_connected);
            return;
        }
    };

    if let Err(error) = paths::ensure_layout() {
        push_log(
            ui,
            logs,
            format!("could not create data directory: {error}"),
        );
        cancel_starting(ui, was_connected);
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
        ready_port: Some(settings.socks_port),
    };

    engine.start(spec);
    spawn_start_watchdog(ui, engine, logs, false);

    if settings.system_proxy && !cfg!(target_os = "android") {
        let proxy = ProxySettings {
            socks_port: settings.socks_port,
            http_port: settings.http_port,
        };
        // `gsettings`/`netsh`/`networksetup` can take hundreds of ms; never run
        // them on the Slint thread.
        let weak = ui.as_weak();
        let logs = logs.clone();
        std::thread::spawn(move || {
            if let Err(error) = system_proxy::enable(proxy) {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        push_log(&ui, &logs, format!("system proxy: {error}"));
                    }
                });
            }
        });
    }
}

fn stop_connection(
    ui: &AppWindow,
    store: &Arc<Mutex<Store>>,
    engine: &EngineHandle,
    logs: &LogBuffer,
) {
    let _ = new_session();
    *BUSY_SINCE.lock().unwrap() = None;
    SWITCHING.store(false, Ordering::SeqCst);
    STARTING.store(false, Ordering::SeqCst);
    if NATIVE_TUNNEL_ACTIVE.swap(false, Ordering::SeqCst) {
        crate::stop_native_tunnel();
    }
    engine.stop();
    crate::stop_android_vpn();
    #[cfg(not(target_os = "android"))]
    stop_desktop_tunnel_async();
    ui.set_busy(true);
    ui.set_status_text("Stopping…".into());

    let system_proxy_enabled = store.lock().unwrap().settings.system_proxy;
    if system_proxy_enabled {
        let weak = ui.as_weak();
        let logs = logs.clone();
        std::thread::spawn(move || {
            if let Err(error) = system_proxy::disable() {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        push_log(
                            &ui,
                            &logs,
                            format!("could not restore system proxy: {error}"),
                        );
                    }
                });
            }
        });
    }
}

fn start_bridge(
    weak: Weak<AppWindow>,
    events: Receiver<EngineEvent>,
    logs: LogBuffer,
    store: Arc<Mutex<Store>>,
) {
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            let weak = weak.clone();
            let logs = logs.clone();
            let store = store.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    apply_event(&ui, &logs, &store, event);
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

fn apply_event(ui: &AppWindow, logs: &LogBuffer, store: &Arc<Mutex<Store>>, event: EngineEvent) {
    match event {
        EngineEvent::State(state) => match state {
            EngineState::Stopped => {
                // A start in flight emits Stopped from the engine it just
                // stopped (the actor answers Stop with Stopped when there is
                // no session yet). Applying that would flip a fresh native
                // connect to "Disconnected", clear NATIVE_TUNNEL_ACTIVE and
                // drop every event the new backend sends.
                if SWITCHING.load(Ordering::SeqCst) || STARTING.load(Ordering::SeqCst) {
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
                #[cfg(not(target_os = "android"))]
                stop_desktop_tunnel_async();
                *BUSY_SINCE.lock().unwrap() = None;
                if KILL_SWITCH.load(Ordering::SeqCst) {
                    crate::start_kill_switch();
                }
                let _ = system_proxy::disable();
            }
            EngineState::Starting => {
                STARTING.store(false, Ordering::SeqCst);
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(true);
                ui.set_status_text(
                    if SWITCHING.load(Ordering::SeqCst) {
                        "Switching…"
                    } else {
                        "Connecting…"
                    }
                    .into(),
                );
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
                *BUSY_SINCE.lock().unwrap() = None;
                // Keep the yellow "connecting" state visible for a moment even
                // when the core starts very quickly. The session check keeps a
                // timer from an earlier connection from clearing a newer one.
                let connected_session = session();
                let weak_busy = ui.as_weak();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(450));
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak_busy.upgrade() {
                            if session() == connected_session && ui.get_connected() {
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
                    #[cfg(not(target_os = "android"))]
                    let store = store.clone();
                    let probe_session = session();
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
                        if ready && session() == probe_session {
                            crate::stop_kill_switch();
                            crate::start_android_vpn(port);
                        }

                        #[cfg(not(target_os = "android"))]
                        if ready {
                            let store = store.clone();
                            let weak = weak.clone();
                            let logs = logs.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if session() != probe_session {
                                    return;
                                }
                                if let Some(ui) = weak.upgrade() {
                                    start_desktop_tunnel(&ui, &logs, &store, port);
                                }
                            });
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
                                    if session() != probe_session
                                        || !ui.get_connected()
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
                            if session() != probe_session {
                                return;
                            }
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
                #[cfg(not(target_os = "android"))]
                stop_desktop_tunnel_async();
            }
            EngineState::Error(message) => {
                let proxy_failure = PROXY_FAILURE.swap(false, Ordering::SeqCst);
                STARTING.store(false, Ordering::SeqCst);
                SWITCHING.store(false, Ordering::SeqCst);
                NATIVE_TUNNEL_ACTIVE.store(false, Ordering::SeqCst);
                *BUSY_SINCE.lock().unwrap() = None;
                clear_last_network();
                ui.set_connected(false);
                ui.set_busy(false);
                let blocked = {
                    #[cfg(target_os = "android")]
                    {
                        false
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        KILL_SWITCH.load(Ordering::SeqCst)
                    }
                };
                // On desktop the TUN can hold the traffic while the core is
                // gone; only keep it when the user asked for a kill switch.
                #[cfg(not(target_os = "android"))]
                if !blocked {
                    stop_desktop_tunnel_async();
                }
                // A short, user-facing reason (VPN permission denied, slot
                // busy) is more useful than a generic "Core error"; long
                // engine diagnostics stay in the log.
                let short = !message.is_empty() && message.chars().count() <= 48;
                ui.set_status_text(
                    if short {
                        message.clone()
                    } else if blocked {
                        "Core error · traffic blocked".to_string()
                    } else {
                        "Core error".to_string()
                    }
                    .into(),
                );
                set_network_text(ui, "", "");
                ui.set_country_flag(Default::default());
                #[cfg(not(target_os = "android"))]
                let _ = system_proxy::disable();
                // A failed proxy VpnService never held the slot: on Android the
                // usual "the TUN stays up as a blackhole" does not apply, so
                // hand the slot to the sinkhole explicitly.
                #[cfg(target_os = "android")]
                if proxy_failure && KILL_SWITCH.load(Ordering::SeqCst) {
                    crate::start_kill_switch();
                }
                // Android: for core failures the existing TUN stays up as a
                // blackhole so apps cannot fall back to the physical network.
                push_log(ui, logs, message.clone());
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
    let line = line.into();
    // Caged runs cannot scroll the log view, so mirror every line to a file
    // when the preview flag is set.
    if std::env::var("BAMDAD_PREVIEW_LOG_FILE").is_ok() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths::data_dir().join("preview.log"))
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
    }
    {
        let mut buffer = logs.lock().unwrap();
        buffer.push(line);
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
    use super::{
        display_name, group_model, mask_ipv4, mask_ipv6, openvpn_needs_auth, should_ignore_toggle,
    };
    use bamdad_core::config::{self, Store};
    use bamdad_core::CoreKind;
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
        let nodes = groups.model.row_data(0).unwrap().nodes;
        assert_eq!(nodes.row_data(0).unwrap().id.as_str(), "second");
        assert_eq!(nodes.row_data(1).unwrap().id.as_str(), "first");
    }

    #[test]
    fn targeted_row_update_changes_one_cell() {
        let mut store = Store::default();
        let mut first = config::default_profile();
        first.id = "first".into();
        first.name = "First".into();
        let mut second = config::default_profile();
        second.id = "second".into();
        second.name = "Second".into();
        second.group_id = Some("sub".into());
        second.group_name = Some("Subscription".into());
        store.profiles = vec![first, second];

        let pings: super::PingState = Arc::new(Mutex::new(HashMap::from([
            ("first".to_string(), ("120 ms".to_string(), 2)),
            ("second".to_string(), ("130 ms".to_string(), 2)),
        ])));
        let model = group_model(&store, &pings);
        let nodes = model.model.row_data(1).unwrap().nodes;

        pings
            .lock()
            .unwrap()
            .insert("second".into(), ("42 ms".to_string(), 2));
        let profile = store.profile_by_id("second").unwrap();
        let row = super::node_row(profile, &pings.lock().unwrap(), None);
        // The cached row model and the published group share one VecModel.
        model.rows[1].set_row_data(0, row);

        assert_eq!(nodes.row_data(0).unwrap().ping.as_str(), "42 ms");
        assert_eq!(model.model.row_data(0).unwrap().nodes.row_count(), 1);
    }

    #[test]
    fn group_header_understands_approximate_latency() {
        let mut profile = config::default_profile();
        profile.id = "a".into();
        let nodes = vec![&profile];
        let pings = HashMap::from([("a".to_string(), ("≈ 150 ms".to_string(), 2))]);
        assert_eq!(super::group_ping(&nodes, &pings), ("150 ms".to_string(), 2));
    }

    #[test]
    fn model_build_and_store_clone_cost() {
        // Rough cost of the operations that run on the UI thread when a ping
        // result arrives. Printed with --nocapture; no timing assertion so it
        // cannot flake on a loaded runner.
        let mut store = Store::default();
        // Roughly the size of a real link-derived Xray outbound (~1.5 KB).
        let filler: String = "x".repeat(1_200);
        let config = serde_json::json!({
            "outbounds": [{
                "protocol": "vless",
                "settings": { "vnext": [{
                    "address": "example.com",
                    "port": 443,
                    "users": [{ "id": "00000000-0000-0000-0000-000000000000", "encryption": "none" }]
                }] },
                "streamSettings": {
                    "network": "xhttp",
                    "security": "tls",
                    "tlsSettings": { "serverName": "example.com", "alpn": ["h2", "http/1.1"] },
                    "xhttpSettings": { "path": "/a-fairly-long-path", "host": "example.com", "extra": filler }
                }
            }]
        });
        for index in 0..300 {
            store.profiles.push(config::Profile {
                id: format!("p{index}"),
                name: format!("🇩🇪 Node {index} · a reasonably long profile name"),
                core: CoreKind::Xray,
                transport: bamdad_core::Transport::Proxy,
                protocol: "vless".into(),
                address: "example.com".into(),
                port: 443,
                config: config.clone(),
                raw_config: None,
                source: None,
                group_id: Some("sub".into()),
                group_name: Some("Subscription".into()),
                group_url: None,
                auth_user: None,
                auth_pass: None,
            });
        }
        let pings: super::PingState = Arc::new(Mutex::new(
            (0..300)
                .map(|index| (format!("p{index}"), ("120 ms".to_string(), 2)))
                .collect::<HashMap<_, _>>(),
        ));

        let rounds = 10u32;
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            let model = group_model(&store, &pings);
            assert_eq!(model.model.row_count(), 2);
        }
        eprintln!("group_model(300): {:?}", start.elapsed() / rounds);

        let start = std::time::Instant::now();
        for _ in 0..rounds {
            let _ = store.clone();
        }
        eprintln!("store.clone(300): {:?}", start.elapsed() / rounds);
    }

    fn openvpn_profile(raw: &str, user: Option<&str>, pass: Option<&str>) -> config::Profile {
        config::Profile {
            id: "ovpn".into(),
            name: "OpenVPN test".into(),
            core: CoreKind::Xray,
            transport: bamdad_core::Transport::OpenVpn,
            protocol: "openvpn".into(),
            address: "198.51.100.7".into(),
            port: 1194,
            config: serde_json::json!({}),
            raw_config: Some(raw.to_string()),
            source: None,
            group_id: None,
            group_name: None,
            group_url: None,
            auth_user: user.map(str::to_string),
            auth_pass: pass.map(str::to_string),
        }
    }

    #[test]
    fn ignores_a_double_click_while_connecting() {
        use std::time::Duration;

        // Right after a start: the second tap is accidental.
        assert!(should_ignore_toggle(
            true,
            false,
            Some(Duration::from_millis(120))
        ));
        // A deliberate cancel later on still works.
        assert!(!should_ignore_toggle(
            true,
            false,
            Some(Duration::from_millis(1500))
        ));
        // While connected the button must always disconnect.
        assert!(!should_ignore_toggle(
            true,
            true,
            Some(Duration::from_millis(10))
        ));
        // Idle: normal connect path.
        assert!(!should_ignore_toggle(false, false, None));
    }

    #[test]
    fn asks_for_credentials_only_when_nothing_is_available() {
        let directive = "client\nauth-user-pass\nremote 198.51.100.7 1194\n";

        // Saved credentials mean the dialog is not needed.
        assert!(!openvpn_needs_auth(&openvpn_profile(
            directive,
            Some("bamdad"),
            Some("secret")
        )));

        // A missing half still needs the prompt.
        assert!(openvpn_needs_auth(&openvpn_profile(
            directive,
            Some("bamdad"),
            None
        )));
        assert!(openvpn_needs_auth(&openvpn_profile(
            directive,
            None,
            Some("secret")
        )));
        assert!(openvpn_needs_auth(&openvpn_profile(directive, None, None)));

        // An inline block already carries them.
        assert!(!openvpn_needs_auth(&openvpn_profile(
            "<auth-user-pass>\nbamdad\nsecret\n</auth-user-pass>",
            None,
            None
        )));

        // No directive at all: nothing to prompt for.
        assert!(!openvpn_needs_auth(&openvpn_profile(
            "client\nremote 198.51.100.7 1194\n",
            None,
            None
        )));

        // The prompt only applies to OpenVPN profiles.
        let mut proxy = openvpn_profile(directive, None, None);
        proxy.transport = bamdad_core::Transport::Proxy;
        assert!(!openvpn_needs_auth(&proxy));
    }

    #[test]
    fn parses_plain_and_approximate_latency() {
        assert_eq!(super::latency_ms("120 ms"), Some(120));
        assert_eq!(super::latency_ms("≈ 120 ms"), Some(120));
        assert_eq!(super::latency_ms("—"), None);
        assert_eq!(super::latency_ms("…"), None);
        assert_eq!(super::latency_ms("timeout"), None);
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
