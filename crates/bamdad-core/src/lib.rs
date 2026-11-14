//! Bamdad core: configuration, core binary management, process supervision and
//! telemetry. The UI crate depends on this library and never touches a core
//! directly.

pub mod binary;
pub mod config;
pub mod engine;
pub mod error;
pub mod httpprobe;
pub mod kind;
pub mod link;
pub mod native;
pub mod paths;
pub mod probe;
pub mod proxyhelper;
pub mod socks;
pub mod stats;
pub mod system_proxy;
pub mod transport;

pub use config::{AppSettings, Profile, Store};
pub use engine::{EngineEvent, EngineHandle, EngineState, LaunchSpec, LogStream};
pub use error::{CoreError, Result};
pub use kind::CoreKind;
pub use transport::{Transport, TunnelProfile, TunnelState, TunnelStatus};
