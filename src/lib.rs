//! Unified Launcher — shared library for daemon, client, and polkit-client.
//!
//! # Crate Structure
//! - [`types`] — shared data types and JSON-line IPC protocol
//! - [`paths`] — XDG-aware configuration, data, cache, and runtime locations
//! - [`storage`] — atomic persistence helpers
//! - [`state`] — versioned persistent launcher state
//! - [`desktop`] — `.desktop` file crawling and icon resolution
//! - [`cache`] — app cache persistence across restarts
//! - [`power`] — system power/session actions
//! - [`quick_settings`] — Sway-oriented Quick Settings integrations

pub mod cache;
pub mod desktop;
pub mod error;
pub mod paths;
pub mod power;
pub mod quick_settings;
pub mod state;
pub mod storage;
pub mod types;
