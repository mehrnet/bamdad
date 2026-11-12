//! Bamdad Slint application.
//!
//! This crate is both a desktop binary (`bamdad`) and an Android `cdylib`
//! (`bamdad_app`). The Slint UI and controller are shared; only the entry point
//! and the platform data/core paths differ.

use slint::ComponentHandle;

#[cfg(target_os = "android")]
mod android_session;
pub mod controller;
pub(crate) mod country;
pub(crate) mod country_names;
#[cfg(target_os = "android")]
mod tunnel;

slint::include_modules!();

pub(crate) fn register_emoji_font() {
    use slint::fontique_011::fontique;
    use std::sync::OnceLock;

    static REGISTERED: OnceLock<()> = OnceLock::new();
    if REGISTERED.set(()).is_err() {
        return;
    }

    let bytes = include_bytes!("../assets/NotoEmoji.ttf").to_vec();
    let blob = fontique::Blob::new(std::sync::Arc::new(bytes));
    let mut collection = slint::fontique_011::shared_collection();
    let families: Vec<fontique::FamilyId> = collection
        .register_fonts(blob, None)
        .into_iter()
        .map(|family| family.0)
        .collect();
    if families.is_empty() {
        return;
    }

    // Make the emoji font the first choice for emoji so multi-codepoint
    // sequences (notably country flags) are shaped by the same font instead
    // of being split across system fallbacks.
    collection.set_generic_families(fontique::GenericFamily::Emoji, families.iter().copied());
    collection.set_fallbacks(
        fontique::FallbackKey::new(fontique::Script::from_bytes(*b"Zsye"), None),
        families.iter().copied(),
    );
    collection.append_fallbacks(
        fontique::FallbackKey::new(fontique::Script::from_bytes(*b"Zyyy"), None),
        families.iter().copied(),
    );
}

pub(crate) fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

#[cfg(not(target_os = "android"))]
fn run_ui(ui: AppWindow, runtime: tokio::runtime::Runtime) -> anyhow::Result<()> {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let _guard = runtime.enter();
    let engine = bamdad_core::engine::spawn(event_tx);
    drop(_guard);

    controller::init(&ui, engine.clone(), runtime.handle().clone(), event_rx);

    ui.run()?;

    // Clean shutdown: stop the child, restore the OS proxy/VPN, drain the runtime.
    engine.stop();
    stop_android_vpn();
    let _ = bamdad_core::system_proxy::disable();
    std::thread::sleep(std::time::Duration::from_millis(250));
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}

pub fn run() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("bamdad {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    bamdad_core::paths::ensure_layout()?;
    let ui = AppWindow::new()?;
    register_emoji_font();
    #[cfg(target_os = "android")]
    {
        android_session::run(ui)
    }
    #[cfg(not(target_os = "android"))]
    {
        let runtime = build_runtime()?;
        run_ui(ui, runtime)
    }
}

// ---------------------------------------------------------------------------
// Android entry point
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::Mutex;

    use jni::objects::{JClass, JObject, JString, JValue};
    use jni::signature::{Primitive, ReturnType};
    use jni::sys::jclass;

    use bamdad_core::engine::EngineEvent;

    static JAVA_VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    static ACTIVITY_CLASS: Mutex<Option<jni::objects::GlobalRef>> = Mutex::new(None);

    fn cache_activity_class(app: &slint::android::AndroidApp) {
        let vm = JAVA_VM.load(Ordering::Acquire);
        if vm.is_null() {
            return;
        }
        // SAFETY: the VM pointer comes from AndroidApp and stays valid for the
        // lifetime of the process.
        let Ok(vm) = (unsafe { jni::JavaVM::from_raw(vm.cast::<jni::sys::JavaVM>()) }) else {
            return;
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        // SAFETY: activity_as_ptr is a valid JNI local reference for this call.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
        let Ok(class) = env.get_object_class(&activity) else {
            return;
        };
        register_tunnel_natives(&mut env, &class);
        let Ok(global) = env.new_global_ref(&class) else {
            return;
        };
        if let Ok(mut slot) = ACTIVITY_CLASS.lock() {
            *slot = Some(global);
        }
    }

    /// Register the reverse (Kotlin -> Rust) tunnel entry points on the
    /// activity class. Registering against the already-loaded class object
    /// avoids the classloader mismatch that plain JNI exports run into with
    /// NativeActivity.
    fn register_tunnel_natives(env: &mut jni::JNIEnv, class: &JClass) {
        use jni::sys::{jboolean, jint};
        use jni::NativeMethod;
        use std::os::raw::c_void;

        extern "system" fn start(
            _env: jni::JNIEnv,
            _this: JObject,
            fd: jint,
            socks_port: jint,
        ) -> jboolean {
            tunnel::start(fd, socks_port as u16) as jboolean
        }

        extern "system" fn stop(_env: jni::JNIEnv, _this: JObject) {
            tunnel::stop();
        }

        extern "system" fn native_log(mut env: jni::JNIEnv, _class: JClass, line: JString) {
            let Ok(line) = env.get_string(&line) else {
                return;
            };
            tunnel::emit_event(EngineEvent::Log {
                stream: bamdad_core::engine::LogStream::App,
                line: line.into(),
            });
        }

        extern "system" fn native_import(
            mut env: jni::JNIEnv,
            _class: JClass,
            group: JString,
            files: JString,
        ) {
            let group: String = env
                .get_string(&group)
                .map(|value| value.into())
                .unwrap_or_default();
            let files: String = env
                .get_string(&files)
                .map(|value| value.into())
                .unwrap_or_default();
            controller::import_native_files(&group, &files);
        }

        extern "system" fn native_event(mut env: jni::JNIEnv, _class: JClass, json: JString) {
            let Ok(text) = env.get_string(&json) else {
                return;
            };
            let text: String = text.into();
            let Ok(status) = serde_json::from_str::<bamdad_core::TunnelStatus>(&text) else {
                eprintln!("bamdad: invalid native tunnel event: {text}");
                return;
            };
            use bamdad_core::TunnelState;
            let state = match status.state {
                TunnelState::Down => EngineEvent::State(bamdad_core::EngineState::Stopped),
                TunnelState::Connecting => EngineEvent::State(bamdad_core::EngineState::Starting),
                TunnelState::Up => EngineEvent::State(bamdad_core::EngineState::Running),
                TunnelState::Disconnecting => {
                    EngineEvent::State(bamdad_core::EngineState::Stopping)
                }
                TunnelState::Error => EngineEvent::State(bamdad_core::EngineState::Error(
                    status
                        .error
                        .clone()
                        .unwrap_or_else(|| "native tunnel failed".into()),
                )),
            };
            tunnel::emit_event(state);
            if status.rx_bytes > 0 || status.tx_bytes > 0 {
                tunnel::emit_event(EngineEvent::Traffic {
                    up_bps: 0,
                    down_bps: 0,
                    total_up: status.tx_bytes,
                    total_down: status.rx_bytes,
                });
            }
        }

        let methods = [
            NativeMethod {
                name: "startTunnelNative".into(),
                sig: "(II)Z".into(),
                fn_ptr: start as *mut c_void,
            },
            NativeMethod {
                name: "stopTunnelNative".into(),
                sig: "()V".into(),
                fn_ptr: stop as *mut c_void,
            },
            NativeMethod {
                name: "nativeTunnelEvent".into(),
                sig: "(Ljava/lang/String;)V".into(),
                fn_ptr: native_event as *mut c_void,
            },
            NativeMethod {
                name: "nativeImportConfigs".into(),
                sig: "(Ljava/lang/String;Ljava/lang/String;)V".into(),
                fn_ptr: native_import as *mut c_void,
            },
            NativeMethod {
                name: "nativeLog".into(),
                sig: "(Ljava/lang/String;)V".into(),
                fn_ptr: native_log as *mut c_void,
            },
        ];
        if let Err(error) = env.register_native_methods(class, &methods) {
            eprintln!("bamdad: could not register tunnel natives: {error}");
        }
    }

    fn with_activity<R>(f: impl FnOnce(&mut jni::JNIEnv, &JClass) -> Option<R>) -> Option<R> {
        let vm = JAVA_VM.load(Ordering::Acquire);
        if vm.is_null() {
            return None;
        }
        // SAFETY: the VM pointer comes from AndroidApp and stays valid.
        let Ok(vm) = (unsafe { jni::JavaVM::from_raw(vm.cast::<jni::sys::JavaVM>()) }) else {
            return None;
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return None;
        };
        let guard = ACTIVITY_CLASS.lock().ok()?;
        let global = guard.as_ref()?;
        // SAFETY: the global reference keeps the class alive for this call.
        let class = unsafe { JClass::from_raw(global.as_obj().as_raw() as jclass) };
        let result = f(&mut env, &class);
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        result
    }

    fn android_string_call(method: &str) -> Option<String> {
        with_activity(|env, class| {
            let method_id = env
                .get_static_method_id(class, method, "()Ljava/lang/String;")
                .ok()?;
            // SAFETY: method_id was resolved from this exact jclass.
            let value = unsafe {
                env.call_static_method_unchecked(class, method_id, ReturnType::Object, &[])
                    .ok()?
            };
            let object = value.l().ok()?;
            let text = JString::from(object);
            env.get_string(&text)
                .ok()
                .map(|string| String::from(string))
        })
    }

    pub(super) fn system_theme() -> Option<String> {
        android_string_call("systemTheme")
    }

    pub(super) fn logcat(line: &str) {
        with_activity(|env, class| {
            let method_id = env
                .get_static_method_id(class, "logLine", "(Ljava/lang/String;)V")
                .ok()?;
            let text = env.new_string(line).ok()?;
            let args = [JValue::Object(&text).as_jni()];
            // SAFETY: method_id was resolved from this exact jclass.
            let _ = unsafe {
                env.call_static_method_unchecked(
                    class,
                    method_id,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )
            };
            Some(())
        });
    }

    fn string_activity_call(method: &str, value: &str) {
        with_activity(|env, class| {
            let method_id = env
                .get_static_method_id(class, method, "(Ljava/lang/String;)V")
                .ok()?;
            let text = env.new_string(value).ok()?;
            let args = [JValue::Object(&text).as_jni()];
            // SAFETY: method_id was resolved from this exact jclass.
            let _ = unsafe {
                env.call_static_method_unchecked(
                    class,
                    method_id,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )
            };
            Some(())
        });
    }

    pub(super) fn update_vpn_notification(profile: &str, down: &str, up: &str, accent: &str) {
        with_activity(|env, class| {
            let method_id = env
                .get_static_method_id(
                    class,
                    "updateVpnNotification",
                    "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                )
                .ok()?;
            let profile = env.new_string(profile).ok()?;
            let down = env.new_string(down).ok()?;
            let up = env.new_string(up).ok()?;
            let accent = env.new_string(accent).ok()?;
            let args = [
                JValue::Object(&profile).as_jni(),
                JValue::Object(&down).as_jni(),
                JValue::Object(&up).as_jni(),
                JValue::Object(&accent).as_jni(),
            ];
            // SAFETY: method_id was resolved from this exact jclass.
            let _ = unsafe {
                env.call_static_method_unchecked(
                    class,
                    method_id,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )
            };
            Some(())
        });
    }

    pub(super) fn apply_launcher_accent(accent: &str) {
        string_activity_call("applyLauncherAccent", accent);
    }

    pub(super) fn start_native_tunnel(json: &str) {
        string_activity_call("startNativeTunnel", json);
    }

    pub(super) fn stop_native_tunnel() {
        android_ping_call("stopNativeTunnel");
    }

    pub(super) fn open_import_file() {
        android_ping_call("openImportFile");
    }

    pub(super) fn read_clipboard() -> String {
        android_string_call("readClipboard").unwrap_or_default()
    }

    pub(super) fn copy_logs(text: &str) {
        string_activity_call("copyLogs", text);
    }

    pub(super) fn share_logs(text: &str) {
        string_activity_call("shareLogs", text);
    }

    pub(super) fn start_vpn(port: u16) {
        android_int_call("startVpn", port as i32);
    }

    pub(super) fn stop_vpn() {
        android_ping_call("stopVpn");
    }

    fn android_int_call(method: &str, value: i32) {
        with_activity(|env, class| {
            let method_id = env.get_static_method_id(class, method, "(I)V").ok()?;
            let args = [JValue::Int(value).as_jni()];
            // SAFETY: method_id was resolved from this exact jclass.
            let _ = unsafe {
                env.call_static_method_unchecked(
                    class,
                    method_id,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )
            };
            Some(())
        });
    }

    fn android_ping_call(method: &str) {
        with_activity(|env, class| {
            let method_id = env.get_static_method_id(class, method, "()V").ok()?;
            // SAFETY: method_id was resolved from this exact jclass.
            let _ = unsafe {
                env.call_static_method_unchecked(
                    class,
                    method_id,
                    ReturnType::Primitive(Primitive::Void),
                    &[],
                )
            };
            Some(())
        });
    }

    #[unsafe(no_mangle)]
    pub fn android_main(app: slint::android::AndroidApp) {
        JAVA_VM.store(app.vm_as_ptr(), Ordering::Release);
        cache_activity_class(&app);

        let data_dir = app
            .internal_data_path()
            .unwrap_or_else(|| PathBuf::from("."));
        bamdad_core::paths::set_data_dir(data_dir);

        if let Some(dir) = android_string_call("nativeLibraryDir") {
            if !dir.is_empty() {
                bamdad_core::paths::set_native_lib_dir(PathBuf::from(dir));
            }
        }

        if let Err(error) = slint::android::init(app) {
            eprintln!("bamdad: Slint Android init failed: {error}");
            return;
        }

        let result = (|| -> anyhow::Result<()> {
            bamdad_core::paths::ensure_layout()?;
            let ui = AppWindow::new()?;
            register_emoji_font();
            android_session::run(ui)
        })();
        android_ping_call("contentReady");
        if let Err(error) = result {
            eprintln!("bamdad: Android app failed: {error:#}");
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) fn logcat(line: &str) {
    android::logcat(line);
}

#[cfg(target_os = "android")]
pub fn read_clipboard() -> String {
    android::read_clipboard()
}

#[cfg(not(target_os = "android"))]
pub fn read_clipboard() -> String {
    String::new()
}

#[cfg(target_os = "android")]
pub fn copy_logs(text: &str) {
    android::copy_logs(text);
}

#[cfg(not(target_os = "android"))]
pub fn copy_logs(_text: &str) {}

#[cfg(target_os = "android")]
pub fn share_logs(text: &str) {
    android::share_logs(text);
}

#[cfg(not(target_os = "android"))]
pub fn share_logs(text: &str) {
    let path = bamdad_core::paths::data_dir().join("bamdad-logs.txt");
    let _ = std::fs::write(path, text);
}

#[cfg(target_os = "android")]
pub fn start_native_tunnel(json: &str) {
    android::start_native_tunnel(json);
}

#[cfg(not(target_os = "android"))]
pub fn start_native_tunnel(_json: &str) {}

#[cfg(target_os = "android")]
pub fn stop_native_tunnel() {
    android::stop_native_tunnel();
}

#[cfg(not(target_os = "android"))]
pub fn stop_native_tunnel() {}

#[cfg(target_os = "android")]
pub fn open_import_file() {
    android::open_import_file();
}

#[cfg(not(target_os = "android"))]
pub fn open_import_file() {}

#[cfg(target_os = "android")]
pub fn start_android_vpn(port: u16) {
    android::start_vpn(port);
}

#[cfg(target_os = "android")]
pub fn stop_android_vpn() {
    android::stop_vpn();
}

#[cfg(not(target_os = "android"))]
pub fn start_android_vpn(_port: u16) {}

#[cfg(not(target_os = "android"))]
pub fn stop_android_vpn() {}
