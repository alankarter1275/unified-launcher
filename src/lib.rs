//! Unified Launcher — shared library for daemon, client, and polkit-client.
//!
//! # Crate Structure
//! - [`types`] — Shared data types and IPC protocol
//! - [`error`] — Error types and `Result<T>` alias
//! - [`desktop`] — `.desktop` file crawling and icon resolution
//! - [`cache`] — App cache persistence across restarts
//! - [`power`] — System power/session actions

pub mod types;
pub mod error;
pub mod desktop;
pub mod cache;
pub mod power;
