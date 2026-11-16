//! Entry point for the privileged desktop TUN helper.
//!
//! Only Linux is implemented for now; other platforms get a clear error
//! instead of a build failure, since the workspace is built on Windows and
//! macOS too.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("bamdad-tun-helper: the desktop TUN helper is Linux-only so far");
    std::process::exit(1);
}
