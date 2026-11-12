//! Android tunnel process management.
//!
//! The VPN service owns the TUN file descriptor. It is duplicated into the
//! child's stdin and tun2socks consumes it through `fd://0`. Stdio survives
//! every Android process-spawn implementation, unlike arbitrary inherited
//! descriptors, and keeping the child in Rust avoids `ProcessBuilder`'s
//! descriptor handling entirely.

use std::io::{BufRead, BufReader, Read};
use std::os::fd::BorrowedFd;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bamdad_core::engine::{EngineEvent, LogStream};
use bamdad_core::paths;

static EVENT_SENDER: OnceLock<Sender<EngineEvent>> = OnceLock::new();
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

pub fn set_event_sender(sender: Sender<EngineEvent>) {
    let _ = EVENT_SENDER.set(sender);
}

/// Emit a transport-neutral event from a Kotlin native backend.
pub fn emit_event(event: EngineEvent) {
    if let Some(sender) = EVENT_SENDER.get() {
        let _ = sender.send(event);
    }
}

fn log(line: impl Into<String>) {
    let line = line.into();
    crate::logcat(&line);
    if let Some(sender) = EVENT_SENDER.get() {
        let _ = sender.send(EngineEvent::Log {
            stream: LogStream::App,
            line,
        });
    } else {
        eprintln!("bamdad: {line}");
    }
}

fn binary_path() -> Option<std::path::PathBuf> {
    if let Ok(value) = std::env::var("BAMDAD_TUN2SOCKS_BIN") {
        if !value.trim().is_empty() {
            let path = std::path::PathBuf::from(value);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let dir = paths::native_lib_dir()?;
    let candidate = dir.join("libbamdad_tun2socks.so");
    candidate.is_file().then_some(candidate)
}

/// Start tun2socks against the already-established TUN descriptor.
pub fn start(fd: i32, socks_port: u16) -> bool {
    stop();

    let Some(binary) = binary_path() else {
        log("tun2socks binary not found in nativeLibraryDir");
        return false;
    };

    let proxy = format!("socks5://127.0.0.1:{socks_port}");
    // Duplicate the TUN descriptor into the child's stdin. Stdio is always
    // preserved across exec, unlike arbitrary inherited descriptors on some
    // Android process-spawn implementations.
    let tun = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .map_err(|error| log(format!("could not duplicate the TUN fd: {error}")))
        .ok();
    let Some(tun) = tun else {
        return false;
    };
    let tun_stdio = Stdio::from(std::fs::File::from(tun));
    log(format!(
        "starting tun2socks: {} -device fd://0 -proxy {proxy}",
        binary.display()
    ));

    let mut child = match Command::new(&binary)
        .arg("--device")
        .arg("fd://0")
        .arg("--proxy")
        .arg(&proxy)
        .arg("--mtu")
        .arg("1400")
        .arg("--tcp-auto-tuning")
        .arg("--tcp-rcvbuf")
        .arg("4m")
        .arg("--tcp-sndbuf")
        .arg("4m")
        .arg("--loglevel")
        .arg("info")
        .stdin(tun_stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log(format!("failed to spawn tun2socks: {error}"));
            return false;
        }
    };

    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr);
    }

    log(format!("tun2socks running (pid {:?})", child.id()));
    *CHILD.lock().unwrap() = Some(child);
    spawn_monitor();
    true
}

/// Watch for an unexpected tunnel exit. The VPN is intentionally left up:
/// stopping it would let apps fall back to the physical network and leak.
fn spawn_monitor() {
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(2));
        let mut slot = CHILD.lock().unwrap();
        let Some(child) = slot.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                log(format!(
                    "tun2socks exited unexpectedly ({status}); VPN stays up and blocks traffic"
                ));
                *slot = None;
                drop(slot);
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log(format!("could not check tun2socks status: {error}"));
                return;
            }
        }
    });
}

fn spawn_reader<R: Read + Send + 'static>(reader: R) {
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            log(format!("[tun2socks] {line}"));
        }
    });
}

/// Stop the tunnel process, if any.
pub fn stop() {
    let mut slot = CHILD.lock().unwrap();
    let Some(mut child) = slot.take() else {
        return;
    };
    log("stopping tun2socks");
    let _ = child.kill();
    match child.wait() {
        Ok(status) => log(format!("tun2socks stopped ({status})")),
        Err(error) => log(format!("tun2socks wait failed: {error}")),
    }
}
