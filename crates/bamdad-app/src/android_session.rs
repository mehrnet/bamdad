//! Android session lifetime.
//!
//! The Slint window is destroyed when the activity finishes, but a VPN must
//! keep running. The engine, runtime and core process therefore live in this
//! process-wide session; a recreated activity reattaches to the same session
//! and repaints its connected state instead of starting a second core.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use slint::ComponentHandle;

use bamdad_core::engine::{self, EngineEvent, EngineHandle, EngineState};

use crate::controller::{self, LogBuffer, SessionSnapshot};
use crate::AppWindow;

struct Session {
    runtime: tokio::runtime::Runtime,
    engine: EngineHandle,
    logs: LogBuffer,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    ui_tx: Arc<Mutex<Option<Sender<EngineEvent>>>>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

type SessionHandles = (
    EngineHandle,
    tokio::runtime::Handle,
    LogBuffer,
    Arc<Mutex<SessionSnapshot>>,
    Arc<Mutex<Option<Sender<EngineEvent>>>>,
);

pub fn run(ui: AppWindow) -> anyhow::Result<()> {
    let (engine, runtime, logs, snapshot, ui_tx) = ensure_session()?;

    let (tx, rx) = channel();
    *ui_tx.lock().unwrap() = Some(tx);

    controller::init_attached(&ui, engine, runtime, logs, snapshot, rx);
    let result = ui.run();

    // The activity is gone, but the foreground VPN service and the core must
    // stay alive. Detach the UI channel and keep the session for the next
    // activity instance.
    *ui_tx.lock().unwrap() = None;
    result?;
    Ok(())
}

fn ensure_session() -> anyhow::Result<SessionHandles> {
    let mut slot = SESSION.lock().unwrap();
    if let Some(session) = slot.as_ref() {
        return Ok((
            session.engine.clone(),
            session.runtime.handle().clone(),
            session.logs.clone(),
            session.snapshot.clone(),
            session.ui_tx.clone(),
        ));
    }

    let runtime = crate::build_runtime()?;
    let (event_tx, event_rx) = channel();
    crate::tunnel::set_event_sender(event_tx.clone());

    let _guard = runtime.enter();
    let engine = engine::spawn(event_tx);
    drop(_guard);

    let logs: LogBuffer = Arc::new(Mutex::new(Vec::new()));
    let snapshot = Arc::new(Mutex::new(SessionSnapshot::default()));
    let ui_tx: Arc<Mutex<Option<Sender<EngineEvent>>>> = Arc::new(Mutex::new(None));

    spawn_relay(event_rx, snapshot.clone(), ui_tx.clone());

    let runtime_handle = runtime.handle().clone();
    let engine_clone = engine.clone();
    let logs_clone = logs.clone();
    let snapshot_clone = snapshot.clone();
    let ui_tx_clone = ui_tx.clone();

    *slot = Some(Session {
        runtime,
        engine,
        logs,
        snapshot,
        ui_tx,
    });

    Ok((
        engine_clone,
        runtime_handle,
        logs_clone,
        snapshot_clone,
        ui_tx_clone,
    ))
}

/// Drain engine events even while no activity is attached. This keeps the
/// snapshot current and stops the VPN if the core dies in the background.
fn spawn_relay(
    events: Receiver<EngineEvent>,
    snapshot: Arc<Mutex<SessionSnapshot>>,
    ui_tx: Arc<Mutex<Option<Sender<EngineEvent>>>>,
) {
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            {
                let mut snap = snapshot.lock().unwrap();
                match &event {
                    EngineEvent::State(next) => snap.state = next.clone(),
                    EngineEvent::Traffic {
                        total_up,
                        total_down,
                        ..
                    } => {
                        snap.total_up = *total_up;
                        snap.total_down = *total_down;
                    }
                    _ => {}
                }
            }

            if matches!(
                &event,
                EngineEvent::State(EngineState::Error(_))
                    | EngineEvent::State(EngineState::Stopped)
            ) {
                crate::stop_android_vpn();
            }

            let tx = ui_tx.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx.send(event);
            }
        }
    });
}
