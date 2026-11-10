//! End-to-end smoke test for the sing-box adapter (launch, Clash API, stop).
//!
//! Skipped automatically when sing-box is not installed.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use bamdad_core::config::{self, AppSettings};
use bamdad_core::engine::{self, EngineEvent, EngineState, LaunchSpec};
use bamdad_core::stats;
use bamdad_core::CoreKind;

fn wait_for(rx: &mpsc::Receiver<EngineEvent>, wanted: EngineState, timeout: Duration) -> bool {
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
async fn starts_singbox_and_reads_clash_api() {
    let Some(binary) = bamdad_core::binary::resolve_kind(CoreKind::SingBox) else {
        eprintln!("skipping: sing-box not installed");
        return;
    };

    let settings = AppSettings {
        socks_port: config::free_port().unwrap(),
        http_port: config::free_port().unwrap(),
        system_proxy: false,
        api_addr: "127.0.0.1".to_string(),
        theme: "system".to_string(),
    };
    let api_port = config::free_port().unwrap();
    let profile = config::default_profiles().remove(1);
    let prepared =
        config::prepare_config(CoreKind::SingBox, &profile.config, &settings, api_port).unwrap();

    let spec = LaunchSpec {
        core: CoreKind::SingBox,
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
        wait_for(&rx, EngineState::Running, Duration::from_secs(10)),
        "sing-box did not reach Running"
    );

    let socks_addr = format!("127.0.0.1:{}", settings.socks_port);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(&socks_addr).await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "SOCKS listener never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut clash_ok = false;
    for _ in 0..20 {
        if stats::query_clash_traffic(&settings.api_addr, api_port)
            .await
            .is_ok()
        {
            clash_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(clash_ok, "Clash API traffic endpoint unavailable");

    handle.stop();
    assert!(
        wait_for(&rx, EngineState::Stopped, Duration::from_secs(10)),
        "sing-box did not stop cleanly"
    );
}
