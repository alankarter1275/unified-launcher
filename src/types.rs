//! Shared data types and JSON-line IPC protocol.

use serde::{Deserialize, Serialize};

use crate::notes::{Note, NoteSummary};
use crate::state::{FolderPin, LauncherState};

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

/// TLP profile choices exposed in Quick Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerProfile {
    Automatic,
    Performance,
    Balanced,
    PowerSaver,
    Unknown,
}

impl PowerProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "performance" => Some(Self::Performance),
            "balanced" => Some(Self::Balanced),
            "power-saver" | "power_saver" => Some(Self::PowerSaver),
            _ => None,
        }
    }

    pub fn as_ui_value(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::PowerSaver => "power-saver",
            Self::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Performance => "Performance",
            Self::Balanced => "Balanced",
            Self::PowerSaver => "Power saver",
            Self::Unknown => "Unavailable",
        }
    }
}

/// A serializable snapshot of Quick Settings state for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickSettingsSnapshot {
    pub wifi_enabled: Option<bool>,
    pub bluetooth_enabled: Option<bool>,
    pub idle_inhibited: bool,
    pub sleep_inhibited: bool,
    pub power_profile: PowerProfile,
}

impl Default for QuickSettingsSnapshot {
    fn default() -> Self {
        Self {
            wifi_enabled: None,
            bluetooth_enabled: None,
            idle_inhibited: false,
            sleep_inhibited: false,
            power_profile: PowerProfile::Unknown,
        }
    }
}

/// Actions available from the inline Quick Settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum QuickSettingsAction {
    ToggleWifi,
    ToggleBluetooth,
    ToggleIdleInhibit,
    ToggleSleepInhibit,
    SetPowerProfile { profile: PowerProfile },
    OpenWifiManager,
    OpenBluetoothManager,
}

/// A file or directory result returned by the daemon's home-directory index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResult {
    pub name: String,
    /// Absolute path used only for opening the result.
    pub path: String,
    /// Compact path shown in the UI, normally relative to HOME.
    pub display_path: String,
    pub is_directory: bool,
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
    SearchFiles {
        query: String,
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
    QuickSettings {
        action: QuickSettingsAction,
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
        quick_settings: QuickSettingsSnapshot,
    },
    ActionResult {
        success: bool,
        message: String,
        quick_settings: Option<QuickSettingsSnapshot>,
        folder_pins: Option<Vec<FolderPin>>,
        notes: Option<Vec<NoteSummary>>,
        note: Option<Note>,
    },
    FolderPathSuggestions {
        suggestions: Vec<String>,
    },
    NoteLoaded {
        note: Note,
    },
    FileSearchResults {
        results: Vec<FileSearchResult>,
        indexing: bool,
        indexed_count: usize,
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
    /// Runtime Quick Settings state. Inhibitor process handles stay daemon-local.
    pub quick_settings: QuickSettingsSnapshot,
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
            | ClientMessage::SearchFiles { .. }
            | ClientMessage::OpenFile { .. }
            | ClientMessage::OpenFileInYazi { .. }
            | ClientMessage::PowerAction { .. }
            | ClientMessage::QuickSettings { .. } => panic!("decoded the wrong message variant"),
        }
    }

    #[test]
    fn power_actions_reject_unknown_values() {
        assert!(PowerAction::parse("lock").is_some());
        assert!(PowerAction::parse("erase-everything").is_none());
    }
}
