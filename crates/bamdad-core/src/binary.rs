use std::path::PathBuf;
use std::process::Command;

use crate::error::{CoreError, Result};
use crate::kind::CoreKind;
use crate::paths;

/// Resolve an Xray executable (kept for compatibility with the first MVP).
pub fn resolve() -> Option<PathBuf> {
    resolve_kind(CoreKind::Xray)
}

/// Resolve a core executable.
///
/// Search order:
/// 1. The core-specific environment variable (`BAMDAD_XRAY_BIN`,
///    `BAMDAD_SINGBOX_BIN`).
/// 2. `<app data>/bin/<binary>` (the managed copy).
/// 3. The binary on `PATH`.
pub fn resolve_kind(kind: CoreKind) -> Option<PathBuf> {
    if let Ok(value) = std::env::var(kind.env_var()) {
        if !value.trim().is_empty() {
            let path = PathBuf::from(value);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "android")]
    if let Some(dir) = paths::native_lib_dir() {
        let name = match kind {
            CoreKind::Xray => "libbamdad_xray.so",
            CoreKind::SingBox => "libbamdad_singbox.so",
        };
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Self-contained release bundles keep cores next to the executable.
    if let Ok(executable) = std::env::current_exe() {
        if let Some(dir) = executable.parent() {
            for candidate in [
                dir.join(kind.binary_name()),
                dir.join("bin").join(kind.binary_name()),
            ] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    let managed = kind.managed_path();
    if managed.is_file() {
        return Some(managed);
    }

    which(kind.binary_stem()).or_else(|| which(kind.binary_name()))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Copy a core binary into the managed location and make it executable.
pub fn install_from_path(source: &std::path::Path, kind: CoreKind) -> Result<PathBuf> {
    paths::ensure_layout()?;
    let destination = kind.managed_path();
    std::fs::copy(source, &destination)?;
    make_executable(&destination)?;
    Ok(destination)
}

/// Write a core binary (e.g. one embedded in the app bundle) into the managed
/// location.
pub fn install_from_bytes(bytes: &[u8], kind: CoreKind) -> Result<PathBuf> {
    paths::ensure_layout()?;
    let destination = kind.managed_path();
    std::fs::write(&destination, bytes)?;
    make_executable(&destination)?;
    Ok(destination)
}

pub fn make_executable(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Best-effort version probe used by the settings screen.
pub fn version(binary: &std::path::Path) -> Result<String> {
    let output = Command::new(binary)
        .arg("version")
        .output()
        .map_err(|e| CoreError::Spawn(format!("{}: {e}", binary.display())))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.into_owned()
    } else {
        stdout.into_owned()
    };

    Ok(text.lines().next().unwrap_or("unknown").trim().to_string())
}

/// The directory that holds `geoip.dat` / `geosite.dat`.
pub fn asset_dir() -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(dir) = executable.parent() {
            let bundled = dir.join("bin").join("assets");
            if bundled.is_dir() {
                return bundled;
            }
        }
    }
    paths::assets_dir()
}
