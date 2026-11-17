//! Entry point for the privileged desktop TUN helper.
//!
//! Linux and macOS own the device themselves; on Windows the sidecar creates a
//! Wintun adapter and this helper configures routing and DNS around it.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(target_os = "windows")]
fn main() {
    windows::main();
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("bamdad-tun-helper: unsupported platform");
    std::process::exit(1);
}
