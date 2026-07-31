//! Error types for the Unified Launcher.

use std::fmt;

/// Top-level error for launcher operations.
#[derive(Debug)]
pub enum LauncherError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Zbus(zbus::Error),
    Slint(slint::PlatformError),
    Other(String),
}

impl fmt::Display for LauncherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LauncherError::Io(e) => write!(f, "I/O error: {}", e),
            LauncherError::Json(e) => write!(f, "JSON error: {}", e),
            LauncherError::Zbus(e) => write!(f, "D-Bus error: {}", e),
            LauncherError::Slint(e) => write!(f, "UI error: {}", e),
            LauncherError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for LauncherError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LauncherError::Io(e) => Some(e),
            LauncherError::Json(e) => Some(e),
            LauncherError::Zbus(e) => Some(e),
            LauncherError::Slint(e) => Some(e),
            LauncherError::Other(_) => None,
        }
    }
}

impl From<std::io::Error> for LauncherError {
    fn from(e: std::io::Error) -> Self { LauncherError::Io(e) }
}

impl From<serde_json::Error> for LauncherError {
    fn from(e: serde_json::Error) -> Self { LauncherError::Json(e) }
}

impl From<zbus::Error> for LauncherError {
    fn from(e: zbus::Error) -> Self { LauncherError::Zbus(e) }
}

impl From<slint::PlatformError> for LauncherError {
    fn from(e: slint::PlatformError) -> Self { LauncherError::Slint(e) }
}

/// Shorthand for `Result<T, LauncherError>`.
pub type Result<T> = std::result::Result<T, LauncherError>;
