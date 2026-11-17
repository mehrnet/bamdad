//! Entry point for the privileged desktop TUN helper.
//!
//! Linux and macOS are implemented; Windows gets a clear error instead of a
//! build failure, since the workspace is built there too.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "macos")]
fn main() {
    macos::main();
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn main() {
    eprintln!("bamdad-tun-helper: the desktop TUN helper supports Linux and macOS so far");
    std::process::exit(1);
}
