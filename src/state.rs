//! Versioned persistent launcher state.
//!
//! UI features such as pinned applications and pinned folders will use this
//! module instead of writing ad-hoc files from short-lived client processes.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::state_file;
use crate::storage::atomic_write;

/// Current on-disk state schema version.
pub const STATE_VERSION: u32 = 1;
/// The launcher has six fixed quick-launch slots.
pub const PIN_SLOT_COUNT: usize = 6;

/// A user-visible directory shortcut.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FolderPin {
    /// Stable identifier used for editing and ordering later.
    pub id: String,
    /// User-provided label shown in the folder pane.
    pub label: String,
    /// Normalized absolute path to the directory.
    pub path: String,
}

/// Launcher-owned state persisted under the XDG data directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LauncherState {
    pub version: u32,
    pub pinned_apps: Vec<Option<String>>,
    pub folder_pins: Vec<FolderPin>,
}

impl Default for LauncherState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            pinned_apps: vec![None; PIN_SLOT_COUNT],
            folder_pins: Vec::new(),
        }
    }
}

impl LauncherState {
    /// Assign an application identity to one of the six quick-launch slots.
    pub fn set_pinned_app(&mut self, slot: usize, app_id: String) -> io::Result<()> {
        let pin = self.pinned_apps.get_mut(slot).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid app-pin slot {}; expected 0..{}",
                    slot, PIN_SLOT_COUNT
                ),
            )
        })?;
        *pin = Some(app_id);
        Ok(())
    }
}

fn invalid_state(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn validate(state: LauncherState) -> io::Result<LauncherState> {
    if state.version != STATE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported launcher-state version {}; expected {}",
                state.version, STATE_VERSION
            ),
        ));
    }

    if state.pinned_apps.len() != PIN_SLOT_COUNT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected {PIN_SLOT_COUNT} app-pin slots, found {}",
                state.pinned_apps.len()
            ),
        ));
    }

    Ok(state)
}

/// Load launcher state, returning an empty state when it has not been created.
pub fn load() -> io::Result<LauncherState> {
    load_from(&state_file()?)
}

/// Persist launcher state with an atomic replacement.
pub fn save(state: &LauncherState) -> io::Result<()> {
    let state = validate(state.clone())?;
    let bytes = serde_json::to_vec_pretty(&state).map_err(invalid_state)?;
    atomic_write(&state_file()?, &bytes)
}

fn load_from(path: &Path) -> io::Result<LauncherState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LauncherState::default())
        }
        Err(error) => return Err(error),
    };

    let state: LauncherState = serde_json::from_slice(&bytes).map_err(invalid_state)?;
    validate(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_has_six_empty_slots() {
        let state = LauncherState::default();
        assert_eq!(state.pinned_apps, vec![None; PIN_SLOT_COUNT]);
        assert!(state.folder_pins.is_empty());
    }

    #[test]
    fn invalid_pin_slot_count_is_rejected() {
        let state = LauncherState {
            pinned_apps: vec![None; PIN_SLOT_COUNT - 1],
            ..LauncherState::default()
        };
        assert_eq!(
            validate(state).expect_err("invalid state").kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn app_pin_assignment_updates_the_requested_slot() {
        let mut state = LauncherState::default();
        state
            .set_pinned_app(2, "org.example.App.desktop".to_string())
            .expect("assign pin");

        assert_eq!(
            state.pinned_apps[2].as_deref(),
            Some("org.example.App.desktop")
        );
        assert!(state
            .set_pinned_app(PIN_SLOT_COUNT, "other".to_string())
            .is_err());
    }
}
