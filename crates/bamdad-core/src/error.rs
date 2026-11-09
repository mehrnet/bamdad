use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid share link: {0}")]
    InvalidLink(String),

    #[error("unsupported share link scheme: {0}")]
    UnsupportedScheme(String),

    #[error("could not find the Xray-core binary. Put it in {0} or set BAMDAD_CORE_BIN")]
    CoreNotFound(PathBuf),

    #[error("Xray-core failed to start: {0}")]
    Spawn(String),

    #[error("telemetry error: {0}")]
    Telemetry(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
