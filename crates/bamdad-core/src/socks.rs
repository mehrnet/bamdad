//! Blocking, dependency-free SOCKS5 probe used to verify that a running core
//! can actually reach the internet, not only that its listener accepts TCP.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::error::{CoreError, Result};

/// Open a TCP connection through the local SOCKS5 listener. Success means the
/// proxy accepted the request and established the upstream connection.
///
/// The destination is sent as a domain name (SOCKS `ATYP=3`), so this also
/// verifies that the proxy can resolve names on the remote side.
pub fn open_through(
    socks_port: u16,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream> {
    let proxy = SocketAddr::from((Ipv4Addr::LOCALHOST, socks_port));
    let mut stream = TcpStream::connect_timeout(&proxy, timeout).map_err(|error| {
        CoreError::Spawn(format!(
            "SOCKS {proxy} is not accepting connections: {error}"
        ))
    })?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    // Greeting: version 5, one method, "no authentication".
    stream.write_all(&[0x05, 0x01, 0x00])?;
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting)?;
    if greeting != [0x05, 0x00] {
        return Err(CoreError::Spawn(format!(
            "SOCKS method negotiation failed: {greeting:?}"
        )));
    }

    // CONNECT to a domain name.
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(CoreError::InvalidLink("host name is too long".into()));
    }
    let mut request = Vec::with_capacity(7 + host_bytes.len());
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    request.extend_from_slice(host_bytes);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request)?;

    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply)?;
    if reply[0] != 0x05 {
        return Err(CoreError::Spawn(format!(
            "unexpected SOCKS version in reply: {}",
            reply[0]
        )));
    }
    if reply[1] != 0x00 {
        return Err(CoreError::Spawn(format!(
            "SOCKS proxy could not connect to {host}:{port} (reply code {})",
            reply[1]
        )));
    }
    skip_bound_address(&mut stream, reply[3])?;
    Ok(stream)
}

/// Open an HTTP request through the local SOCKS5 listener and return the
/// response status line (for example `HTTP/1.0 200 OK`).
pub fn http_through(socks_port: u16, host: &str, port: u16, timeout: Duration) -> Result<String> {
    let mut stream = open_through(socks_port, host, port, timeout)?;

    stream.write_all(
        format!(
            "GET / HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: bamdad-check\r\n\r\n"
        )
        .as_bytes(),
    )?;

    let mut buffer = [0u8; 512];
    let read = stream.read(&mut buffer)?;
    if read == 0 {
        return Err(CoreError::Spawn(
            "proxy closed the connection before sending an HTTP response".into(),
        ));
    }
    let text = String::from_utf8_lossy(&buffer[..read]);
    let line = text.lines().next().unwrap_or("").trim().to_string();
    if !line.starts_with("HTTP/") {
        return Err(CoreError::Spawn(format!(
            "unexpected response through proxy: {line}"
        )));
    }
    Ok(line)
}

/// Fetch a small plain-HTTP resource through the local SOCKS5 listener.
/// Used for exit-IP lookups; all traffic stays inside the tunnel.
pub fn http_get(socks_port: u16, host: &str, path: &str, timeout: Duration) -> Result<String> {
    let mut stream = open_through(socks_port, host, 80, timeout)?;
    stream.write_all(
        format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: bamdad-exit-ip\r\n\r\n"
        )
        .as_bytes(),
    )?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("");
    Ok(body.trim().to_string())
}

fn skip_bound_address(stream: &mut TcpStream, address_type: u8) -> Result<()> {
    let length = match address_type {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            len[0] as usize
        }
        other => {
            return Err(CoreError::Spawn(format!(
                "unsupported SOCKS address type: {other}"
            )))
        }
    };
    let mut ignored = vec![0u8; length + 2];
    stream.read_exact(&mut ignored)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn speaks_socks5_and_returns_http_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut greeting = [0u8; 3];
            client.read_exact(&mut greeting).unwrap();
            client.write_all(&[0x05, 0x00]).unwrap();
            let mut head = [0u8; 5];
            client.read_exact(&mut head).unwrap();
            let mut host = vec![0u8; head[4] as usize];
            client.read_exact(&mut host).unwrap();
            let mut port = [0u8; 2];
            client.read_exact(&mut port).unwrap();
            client
                .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 80])
                .unwrap();
            let mut request = [0u8; 256];
            let _ = client.read(&mut request).unwrap();
            client
                .write_all(b"HTTP/1.0 204 No Content\r\n\r\n")
                .unwrap();
        });

        let status = http_through(port, "example.com", 80, Duration::from_secs(2)).unwrap();
        assert_eq!(status, "HTTP/1.0 204 No Content");
        server.join().unwrap();
    }

    #[test]
    fn reports_a_refused_connection() {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let error = http_through(port, "example.com", 80, Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("not accepting"));
    }
}
