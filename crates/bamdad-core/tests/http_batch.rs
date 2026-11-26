//! Manual batch-probe benchmark. Requires a real Xray binary:
//!
//! BAMDAD_XRAY_BIN=$HOME/.local/share/bamdad/bin/xray \
//!   cargo test -p bamdad-core --test http_batch -- --ignored --nocapture

use std::time::{Duration, Instant};

use bamdad_core::httpprobe::{measure_batch, measure_batch_stream, BatchJob};
use bamdad_core::CoreKind;
use serde_json::json;

#[test]
#[ignore]
fn batches_many_freedom_profiles() {
    let config = json!({
        "log": { "loglevel": "warning" },
        "outbounds": [{ "protocol": "freedom", "tag": "direct" }]
    });

    let counts: Vec<usize> = std::env::var("BAMDAD_PROBE_COUNTS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![10, 200, 1000]);
    for count in counts {
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

/// One config Xray rejects (the placeholder node some subscriptions ship)
/// used to make the whole merged core exit, so every row in the batch looked
/// dead. The prober now isolates it and still measures the good configs.
/// Run with:
///   BAMDAD_XRAY_BIN=$HOME/.local/share/bamdad/bin/xray \
///     cargo test -p bamdad-core --test http_batch -- --ignored --nocapture
#[test]
#[ignore]
fn a_rejected_config_does_not_poison_the_batch() {
    let freedom = json!({
        "log": { "loglevel": "warning" },
        "outbounds": [{ "protocol": "freedom", "tag": "direct" }]
    });
    let poison = json!({
        "log": { "loglevel": "warning" },
        "outbounds": [{
            "protocol": "vless",
            "settings": { "vnext": [{
                "address": "always.update.please",
                "port": 1,
                "users": [{ "id": "00000000-0000-0000-0000-000000000000", "encryption": "none" }]
            }] },
            "streamSettings": { "network": "tcp", "security": "none" }
        }]
    });

    let jobs = vec![
        BatchJob {
            id: "first".into(),
            core: CoreKind::Xray,
            config: freedom.clone(),
        },
        BatchJob {
            id: "poison".into(),
            core: CoreKind::Xray,
            config: poison,
        },
        BatchJob {
            id: "last".into(),
            core: CoreKind::Xray,
            config: freedom,
        },
    ];
    let results = measure_batch(jobs, Duration::from_secs(4));
    let by_id = |id: &str| results.iter().find(|r| r.id == id).unwrap().latency_ms;
    assert!(by_id("first").is_some(), "first config was not measured");
    assert!(by_id("last").is_some(), "config after the bad one was lost");
    assert!(by_id("poison").is_none());
}

#[test]
#[ignore]
fn streams_results_as_they_finish() {
    let config = json!({
        "log": { "loglevel": "warning" },
        "outbounds": [{ "protocol": "freedom", "tag": "direct" }]
    });
    let jobs: Vec<BatchJob> = (0..3)
        .map(|index| BatchJob {
            id: format!("stream-{index}"),
            core: CoreKind::Xray,
            config: config.clone(),
        })
        .collect();

    let seen = std::sync::Mutex::new(Vec::new());
    measure_batch_stream(jobs, Duration::from_secs(4), |result| {
        seen.lock().unwrap().push(result.id);
    });
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.len(), 3, "not every result was streamed: {seen:?}");
}
