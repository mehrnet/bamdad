use serde::{Deserialize, Serialize};

/// A proxy engine that Bamdad can supervise.
///
/// Adding a core means teaching three small pieces: how to launch it, how to
/// inject telemetry, and how to read that telemetry. Everything else (profiles,
/// UI, lifecycle, system proxy) is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CoreKind {
    #[default]
    Xray,
    SingBox,
}

impl CoreKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Xray => "Xray",
            Self::SingBox => "sing-box",
        }
    }

    pub fn binary_stem(self) -> &'static str {
        match self {
            Self::Xray => "xray",
            Self::SingBox => "sing-box",
        }
    }

    pub fn binary_name(self) -> &'static str {
        if cfg!(target_os = "windows") {
            match self {
                Self::Xray => "xray.exe",
                Self::SingBox => "sing-box.exe",
            }
        } else {
            match self {
                Self::Xray => "xray",
                Self::SingBox => "sing-box",
            }
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Self::Xray => "BAMDAD_XRAY_BIN",
            Self::SingBox => "BAMDAD_SINGBOX_BIN",
        }
    }

    pub fn managed_path(self) -> std::path::PathBuf {
        crate::paths::bin_dir().join(self.binary_name())
    }
}
