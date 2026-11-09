//! Bamdad Slint application.
//!
//! This crate is both a desktop binary (`bamdad`) and an Android `cdylib`
//! (`bamdad_app`). The Slint UI and controller are shared; only the entry point
//! and the platform data/core paths differ.

use slint::ComponentHandle;

pub mod controller;

slint::include_modules!();

fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

fn run_ui(ui: AppWindow, runtime: tokio::runtime::Runtime) -> anyhow::Result<()> {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let _guard = runtime.enter();
    let engine = bamdad_core::engine::spawn(event_tx);
    drop(_guard);

    controller::init(&ui, engine.clone(), runtime.handle().clone(), event_rx);

    ui.run()?;

    // Clean shutdown: stop the child, restore the OS proxy, drain the runtime.
    engine.stop();
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
    let runtime = build_runtime()?;
    run_ui(ui, runtime)
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

    use jni::objects::{JClass, JObject, JString};
    use jni::signature::{Primitive, ReturnType};
    use jni::sys::jclass;

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
        let Ok(env) = vm.attach_current_thread() else {
            return;
        };
        // SAFETY: activity_as_ptr is a valid JNI local reference for this call.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
        let Ok(class) = env.get_object_class(&activity) else {
            return;
        };
        let Ok(global) = env.new_global_ref(&class) else {
            return;
        };
        if let Ok(mut slot) = ACTIVITY_CLASS.lock() {
            *slot = Some(global);
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
            let runtime = build_runtime()?;
            run_ui(ui, runtime)
        })();
        android_ping_call("contentReady");
        if let Err(error) = result {
            eprintln!("bamdad: Android app failed: {error:#}");
        }
    }
}
