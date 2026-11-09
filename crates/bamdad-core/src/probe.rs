use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::error::{CoreError, Result};

/// Transport-level latency shared by every protocol.
///
/// All supported protocols (VLESS, VMess, Trojan, Shadowsocks, and their
/// TLS/REALITY/WS/gRPC variants) use a TCP endpoint, so a timed TCP handshake
/// is a meaningful, protocol-agnostic "ping". It intentionally does not send
/// credentials, so it is safe to run for every saved profile.
pub async fn tcp_latency(host: &str, port: u16, timeout: Duration) -> Result<u64> {
    if host.trim().is_empty() || port == 0 {
        return Err(CoreError::InvalidLink(
            "profile has no reachable endpoint".to_string(),
        ));
    }

    let start = Instant::now();
    let connect = TcpStream::connect((host, port));
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(start.elapsed().as_millis() as u64)
        }
        Ok(Err(error)) => Err(CoreError::Io(error)),
        Err(_) => Err(CoreError::Spawn(format!(
            "timed out after {} ms",
            timeout.as_millis()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_a_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let latency = tcp_latency("127.0.0.1", port, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(latency < 1_000);
    }

    #[tokio::test]
    async fn rejects_empty_endpoints() {
        assert!(tcp_latency("", 0, Duration::from_millis(50)).await.is_err());
    }
}
