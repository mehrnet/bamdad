use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
static NATIVE_LIB_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set the app data root. Used by the Android entry point, which receives an
/// app-private path from the platform instead of XDG/HOME.
pub fn set_data_dir(path: PathBuf) {
    let _ = DATA_DIR_OVERRIDE.set(path);
}

/// Set the directory where Android placed the packaged native libraries.
pub fn set_native_lib_dir(path: PathBuf) {
    let _ = NATIVE_LIB_DIR.set(path);
}

pub fn native_lib_dir() -> Option<&'static PathBuf> {
    NATIVE_LIB_DIR.get()
}

/// Application data directory, resolved without extra dependencies.
///
/// * Linux/BSD: `$XDG_DATA_HOME/bamdad` or `$HOME/.local/share/bamdad`
/// * macOS:     `$HOME/Library/Application Support/bamdad`
/// * Windows:   `%APPDATA%\bamdad`
pub fn data_dir() -> PathBuf {
    if let Some(path) = DATA_DIR_OVERRIDE.get() {
        return path.clone();
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("bamdad");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("bamdad");
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return PathBuf::from(xdg).join("bamdad");
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("bamdad");
        }
    }

    PathBuf::from(".bamdad")
}

pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

pub fn assets_dir() -> PathBuf {
    data_dir().join("assets")
}

pub fn run_dir() -> PathBuf {
    data_dir().join("run")
}

pub fn store_path() -> PathBuf {
    data_dir().join("profiles.json")
}

pub fn core_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "xray.exe"
    } else {
        "xray"
    }
}

pub fn ensure_layout() -> std::io::Result<()> {
    for dir in [data_dir(), bin_dir(), assets_dir(), run_dir()] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}
