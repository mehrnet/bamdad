use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tonic::transport::{Channel, Endpoint};

use crate::error::{CoreError, Result};

pub mod pb {
    tonic::include_proto!("xray.app.stats.command");
}

use pb::stats_service_client::StatsServiceClient;
use pb::QueryStatsRequest;

#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficTotals {
    pub uplink: u64,
    pub downlink: u64,
}

/// Lazily connect to Xray's gRPC StatsService.
pub async fn connect(addr: &str, port: u16) -> Result<Channel> {
    let endpoint = Endpoint::from_shared(format!("http://{addr}:{port}"))
        .map_err(|e| CoreError::Telemetry(e.to_string()))?;
    endpoint
        .connect()
        .await
        .map_err(|e| CoreError::Telemetry(e.to_string()))
}

/// Read cumulative outbound counters from Xray. Traffic is summed across every
/// outbound so the UI shows the total for the active profile.
pub async fn query_traffic(channel: Channel) -> Result<TrafficTotals> {
    let mut client = StatsServiceClient::new(channel);
    let response = client
        .query_stats(QueryStatsRequest {
            pattern: "outbound".to_string(),
            reset: false,
        })
        .await
        .map_err(|e| CoreError::Telemetry(e.to_string()))?
        .into_inner();

    let mut totals = TrafficTotals::default();
    for stat in response.stat {
        let value = stat.value.max(0) as u64;
        if stat.name.ends_with("uplink") {
            totals.uplink += value;
        } else if stat.name.ends_with("downlink") {
            totals.downlink += value;
        }
    }
    Ok(totals)
}

#[derive(Debug, Deserialize)]
struct ClashTraffic {
    #[serde(default)]
    up: u64,
    #[serde(default)]
    down: u64,
}

/// Sing-box exposes the Clash-compatible API. `/traffic` is a streaming
/// endpoint that emits one `{"up":..,"down":..}` object per second; we read the
/// first object and close the connection.
pub async fn query_clash_traffic(addr: &str, port: u16) -> Result<(u64, u64)> {
    let mut stream =
        tokio::time::timeout(Duration::from_millis(700), TcpStream::connect((addr, port)))
            .await
            .map_err(|_| CoreError::Telemetry("clash api connect timed out".into()))??;

    let request = format!(
        "GET /traffic HTTP/1.1\r\nHost: {addr}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    let mut buffer = vec![0u8; 4096];
    let mut collected = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(
            remaining.min(Duration::from_millis(900)),
            stream.read(&mut buffer),
        )
        .await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                collected.extend_from_slice(&buffer[..n]);
                if let Some(json) = first_json_object(&collected) {
                    if let Ok(traffic) = serde_json::from_str::<ClashTraffic>(&json) {
                        return Ok((traffic.up, traffic.down));
                    }
                }
            }
            Ok(Err(error)) => return Err(CoreError::Io(error)),
            // `/traffic` emits one sample per second; keep waiting.
            Err(_) => continue,
        }
    }

    Err(CoreError::Telemetry(
        "could not read a traffic sample from the clash api".into(),
    ))
}

fn first_json_object(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| text[start..=end].to_string())
}
