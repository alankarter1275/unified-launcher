//! Shared data types and JSON-line IPC protocol.

use serde::{Deserialize, Serialize};

use crate::notes::{Note, NoteSummary};
use crate::state::{FolderPin, LauncherState};
use crate::vault::VaultItem;

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
    LaunchApp {
        app_id: String,
    },
    SetAppPin {
        slot: u8,
        app_id: String,
    },
    CreateFolderPin {
        label: String,
        path: String,
    },
    DeleteFolderPin {
        id: String,
    },
    OpenFolder {
        id: String,
    },
    FolderPathSuggestions {
        path: String,
    },
    CreateNote {
        title: String,
    },
    LoadNote {
        id: String,
    },
    SaveNote {
        id: String,
        title: String,
        content: String,
    },
    DeleteNote {
        id: String,
    },
    OpenFile {
        path: String,
    },
    OpenFileInYazi {
        path: String,
    },
    PowerAction {
        action: PowerAction,
    },
    VaultUnlock {
        password: String,
    },
    VaultLock,
    VaultAdd {
        name: String,
        secret: String,
        note: String,
    },
    VaultDelete {
        id: String,
    },
}

/// Messages emitted by the daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Init {
        apps: Vec<AppInit>,
        pinned_app_ids: Vec<Option<String>>,
        folder_pins: Vec<FolderPin>,
        notes: Vec<NoteSummary>,
    },
    ActionResult {
        success: bool,
        message: String,
        folder_pins: Option<Vec<FolderPin>>,
        notes: Option<Vec<NoteSummary>>,
        note: Option<Note>,
        vault_items: Option<Vec<VaultItem>>,
        vault_locked: Option<bool>,
    },
    FolderPathSuggestions {
        suggestions: Vec<String>,
    },
    NoteLoaded {
        note: Note,
    },
    Error {
        message: String,
    },
}

/// The daemon's in-memory state.
pub struct DaemonState {
    pub apps: Vec<AppEntry>,
    /// Persistent user choices owned by the long-lived daemon.
    pub launcher_state: LauncherState,
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
            ClientMessage::SetAppPin { .. }
            | ClientMessage::CreateFolderPin { .. }
            | ClientMessage::DeleteFolderPin { .. }
            | ClientMessage::OpenFolder { .. }
            | ClientMessage::FolderPathSuggestions { .. }
            | ClientMessage::CreateNote { .. }
            | ClientMessage::LoadNote { .. }
            | ClientMessage::SaveNote { .. }
            | ClientMessage::DeleteNote { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::OpenFileInYazi { .. }
            | ClientMessage::PowerAction { .. }
            | ClientMessage::VaultUnlock { .. }
            | ClientMessage::VaultLock
            | ClientMessage::VaultAdd { .. }
            | ClientMessage::VaultDelete { .. } => panic!("decoded the wrong message variant"),
        }
    }

    #[test]
    fn power_actions_reject_unknown_values() {
        assert!(PowerAction::parse("lock").is_some());
        assert!(PowerAction::parse("erase-everything").is_none());
    }
}
