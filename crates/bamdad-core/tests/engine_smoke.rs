//! End-to-end smoke test for the real Xray-core child process.
//!
//! Skipped automatically when no core binary is installed. Run:
//!   cargo test -p bamdad-core --test engine_smoke -- --nocapture

use std::sync::mpsc;
use std::time::{Duration, Instant};

use bamdad_core::config::{self, AppSettings};
use bamdad_core::engine::{self, EngineEvent, EngineState, LaunchSpec};
use bamdad_core::stats;
use bamdad_core::CoreKind;

fn recv_state(rx: &mpsc::Receiver<EngineEvent>, wanted: EngineState, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(EngineEvent::State(state)) => {
                eprintln!("state: {state:?}");
                if state == wanted {
                    return true;
                }
                if let EngineState::Error(message) = state {
                    panic!("engine error: {message}");
                }
            }
            Ok(EngineEvent::Log { line, .. }) => eprintln!("log: {line}"),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_core_and_queries_stats() {
    let Some(binary) = bamdad_core::binary::resolve() else {
        eprintln!("skipping: Xray-core not installed");
        return;
    };

    let settings = AppSettings {
        socks_port: config::free_port().unwrap(),
        http_port: config::free_port().unwrap(),
        system_proxy: false,
        api_addr: "127.0.0.1".to_string(),
    };
    let api_port = config::free_port().unwrap();
    let profile = config::default_profile();
    let prepared =
        config::prepare_config(CoreKind::Xray, &profile.config, &settings, api_port).unwrap();

    let spec = LaunchSpec {
        core: CoreKind::Xray,
        binary,
        config: prepared,
        assets_dir: bamdad_core::paths::assets_dir(),
        work_dir: bamdad_core::paths::run_dir(),
        api_addr: settings.api_addr.clone(),
        api_port,
    };

    let (tx, rx) = mpsc::channel();
    let handle = engine::spawn(tx);

    handle.start(spec);
    assert!(
        recv_state(&rx, EngineState::Running, Duration::from_secs(10)),
        "core did not reach Running"
    );

    // SOCKS listener must accept connections.
    let socks_addr = format!("127.0.0.1:{}", settings.socks_port);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(&socks_addr).await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "SOCKS listener never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Telemetry may need a moment after the listeners are up.
    let mut last_error = None;
    let mut telemetry_ok = false;
    for _ in 0..20 {
        match stats::connect(&settings.api_addr, api_port).await {
            Ok(channel) => match stats::query_traffic(channel).await {
                Ok(_totals) => {
                    telemetry_ok = true;
                    break;
                }
                Err(error) => last_error = Some(error.to_string()),
            },
            Err(error) => last_error = Some(error.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        telemetry_ok,
        "gRPC telemetry unavailable: {}",
        last_error.unwrap_or_default()
    );

    handle.stop();
    assert!(
        recv_state(&rx, EngineState::Stopped, Duration::from_secs(10)),
        "core did not stop cleanly"
    );
}
