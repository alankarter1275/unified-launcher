//! Unified Launcher — shared library for daemon, client, and polkit-client.
//!
//! # Crate Structure
//! - [`types`] — shared data types and JSON-line IPC protocol
//! - [`paths`] — XDG-aware configuration, data, cache, and runtime locations
//! - [`storage`] — atomic persistence helpers
//! - [`state`] — versioned persistent launcher state
//! - [`desktop`] — `.desktop` file crawling and icon resolution
//! - [`folders`] — folder-pin validation and lightweight path completion
//! - [`file_index`] — incremental home-directory file search
//! - [`cache`] — app cache persistence across restarts
//! - [`notes`] — local Markdown note storage
//! - [`calendar`] — local clock and month-calendar helpers
//! - [`power`] — system power/session actions
//! - [`quick_settings`] — Sway-oriented Quick Settings integrations

pub mod cache;
pub mod calendar;
pub mod desktop;
pub mod error;
pub mod folders;
pub mod notes;
pub mod paths;
pub mod power;
pub mod state;
pub mod storage;
pub mod types;
pub mod vault;
