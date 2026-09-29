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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Zbus(error) => write!(formatter, "D-Bus error: {error}"),
            Self::Slint(error) => write!(formatter, "UI error: {error}"),
            Self::Other(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for LauncherError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Zbus(error) => Some(error),
            Self::Slint(error) => Some(error),
            Self::Other(_) => None,
        }
    }
}

impl From<std::io::Error> for LauncherError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LauncherError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<zbus::Error> for LauncherError {
    fn from(error: zbus::Error) -> Self {
        Self::Zbus(error)
    }
}

impl From<slint::PlatformError> for LauncherError {
    fn from(error: slint::PlatformError) -> Self {
        Self::Slint(error)
    }
}

/// Shorthand for `Result<T, LauncherError>`.
pub type Result<T> = std::result::Result<T, LauncherError>;
