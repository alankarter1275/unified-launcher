//! Shared data types and JSON-line IPC protocol.

use serde::{Deserialize, Serialize};

/// A fully resolved desktop entry with a stable desktop-entry identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    /// Desktop-file name, for example `org.mozilla.firefox.desktop`.
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub needs_terminal: bool,
}

/// The app data the daemon exposes to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInit {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
}

/// Sway-oriented session actions supported by the daemon.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Lock,
    Logout,
    Shutdown,
    Reboot,
}

impl PowerAction {
    /// Convert a UI string into a validated power action.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "lock" => Some(Self::Lock),
            "logout" => Some(Self::Logout),
            "shutdown" => Some(Self::Shutdown),
            "reboot" => Some(Self::Reboot),
            _ => None,
        }
    }
}

/// Commands a client can send to the per-user daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    LaunchApp { app_id: String },
    PowerAction { action: PowerAction },
}

/// Messages emitted by the daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Init { apps: Vec<AppInit> },
    ActionResult { success: bool, message: String },
    Error { message: String },
}

/// The daemon's in-memory state.
pub struct DaemonState {
    pub apps: Vec<AppEntry>,
}

/// Serialize a protocol message as one newline-delimited JSON record.
pub fn json_line<T: Serialize>(message: &T) -> serde_json::Result<String> {
    let mut encoded = serde_json::to_string(message)?;
    encoded.push('\n');
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_round_trip_as_json_lines() {
        let message = ClientMessage::LaunchApp {
            app_id: "org.example.App.desktop".to_string(),
        };
        let line = json_line(&message).expect("serialize message");

        assert!(line.ends_with('\n'));
        let decoded: ClientMessage = serde_json::from_str(line.trim_end()).expect("deserialize");
        match decoded {
            ClientMessage::LaunchApp { app_id } => {
                assert_eq!(app_id, "org.example.App.desktop");
            }
            ClientMessage::PowerAction { .. } => panic!("decoded the wrong message variant"),
        }
    }

    #[test]
    fn power_actions_reject_unknown_values() {
        assert!(PowerAction::parse("lock").is_some());
        assert!(PowerAction::parse("erase-everything").is_none());
    }
}
