//! Manual batch-probe benchmark. Requires a real Xray binary:
//!
//! BAMDAD_XRAY_BIN=$HOME/.local/share/bamdad/bin/xray \
//!   cargo test -p bamdad-core --test http_batch -- --ignored --nocapture

use std::time::{Duration, Instant};

use bamdad_core::httpprobe::{measure_batch, BatchJob};
use bamdad_core::CoreKind;
use serde_json::json;

#[test]
#[ignore]
fn batches_many_freedom_profiles() {
    let config = json!({
        "log": { "loglevel": "warning" },
        "outbounds": [{ "protocol": "freedom", "tag": "direct" }]
    });

    for count in [10usize, 200, 1000] {
        let jobs: Vec<BatchJob> = (0..count)
            .map(|index| BatchJob {
                id: format!("probe-{index}"),
                core: CoreKind::Xray,
                config: config.clone(),
            })
            .collect();

        let started = Instant::now();
        let results = measure_batch(jobs, Duration::from_secs(3));
        let elapsed = started.elapsed();
        let ok = results.iter().filter(|r| r.latency_ms.is_some()).count();
        eprintln!("{count} probes: {ok} ok in {elapsed:?}");
        assert_eq!(results.len(), count);
        assert!(ok > count / 2, "only {ok}/{count} probes succeeded");
    }
}
