//! Shared types for the Unified Launcher client/daemon protocol.

use serde::{Deserialize, Serialize};

/// A fully resolved desktop entry with icon paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub needs_terminal: bool,
}

/// A lightweight app descriptor sent over the IPC socket (no Exec field).
#[derive(Clone, Serialize, Deserialize)]
pub struct AppInit {
    pub name: String,
    pub icon: Option<String>,
}

/// Payload sent from daemon → client on connection.
#[derive(Serialize, Deserialize)]
pub struct InitPayload {
    pub apps: Vec<AppInit>,
}

/// The daemon's in-memory state.
pub struct DaemonState {
    pub apps: Vec<AppEntry>,
}

/// Resolve the Unix socket path (env override or default).
pub fn socket_path() -> String {
    std::env::var("UNIFIED_LAUNCHER_SOCKET")
        .unwrap_or_else(|_| "/tmp/unified_launcher.sock".to_string())
}
