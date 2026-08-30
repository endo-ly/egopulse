//! Local IPC between the shared runtime and local channel clients.

pub(crate) mod client;
pub(crate) mod protocol;
mod server;
mod service;

use std::path::{Path, PathBuf};

use crate::error::{ConfigError, EgoPulseError};

pub(crate) use client::LocalRuntimeClient;

/// Stable protocol version used by the runtime and its local clients.
pub(crate) const PROTOCOL_VERSION: u32 = 2;
const SOCKET_FILE_NAME: &str = "egopulse.sock";

/// Returns the local runtime socket path for a state root.
pub(crate) fn socket_path(state_root: &Path) -> PathBuf {
    state_root.join("runtime").join(SOCKET_FILE_NAME)
}

/// Resolves the local runtime socket without loading provider or secret configuration.
pub(crate) fn resolve_socket_path(config_path: Option<&Path>) -> Result<PathBuf, ConfigError> {
    let state_root = crate::config::resolve_state_root(config_path)?;
    Ok(socket_path(&state_root))
}

/// Starts the local API listener owned by the runtime supervisor.
pub(crate) fn start(state: &std::sync::Arc<crate::runtime::AppState>) -> Result<(), EgoPulseError> {
    server::start(state)
}
