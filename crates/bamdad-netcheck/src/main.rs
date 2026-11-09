//! Tiny end-to-end network checker used by the Android smoke test.
//!
//! It deliberately uses only std: when pushed to `/data/local/tmp` and run
//! from `adb shell`, its traffic has to cross the app's VPN TUN exactly like a
//! normal app's traffic.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "example.com".to_string());
    let port: u16 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(80);

    let timeout = Duration::from_secs(15);

    let addresses = match (host.as_str(), port).to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(error) => {
            eprintln!("resolve {host}:{port} failed: {error}");
            std::process::exit(2);
        }
    };
    if addresses.is_empty() {
        eprintln!("resolve {host}:{port} returned no addresses");
        std::process::exit(2);
    }
    println!("resolved {host} to {addresses:?}");

    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(mut stream) => {
                let _ = stream.set_read_timeout(Some(timeout));
                let _ = stream.set_write_timeout(Some(timeout));
                let request = format!(
                    "GET / HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: bamdad-netcheck\r\n\r\n"
                );
                if let Err(error) = stream.write_all(request.as_bytes()) {
                    last_error = Some(format!("write to {address}: {error}"));
                    continue;
                }
                let mut buffer = [0u8; 512];
                match stream.read(&mut buffer) {
                    Ok(0) => last_error = Some(format!("{address} closed without a response")),
                    Ok(read) => {
                        let text = String::from_utf8_lossy(&buffer[..read]);
                        let line = text.lines().next().unwrap_or("").trim();
                        println!("response from {address}: {line}");
                        if line.starts_with("HTTP/") {
                            std::process::exit(0);
                        }
                        last_error = Some(format!("unexpected response from {address}: {line}"));
                    }
                    Err(error) => last_error = Some(format!("read from {address}: {error}")),
                }
            }
            Err(error) => last_error = Some(format!("connect to {address}: {error}")),
        }
    }

    eprintln!(
        "no usable HTTP response for {host}:{port}: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    );
    std::process::exit(1);
}
