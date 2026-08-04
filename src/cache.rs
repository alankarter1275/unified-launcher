//! Application cache: persists resolved entries to disk so crawling is skipped
//! on normal daemon restarts.

use std::fs;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::desktop::get_app_dirs;
use crate::paths::app_cache_file;
use crate::storage::atomic_write;
use crate::types::AppEntry;

const CACHE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct AppCache {
    version: u32,
    apps: Vec<AppEntry>,
}

/// Returns the most recent mtime across all application directories.
fn source_dirs_max_mtime() -> Option<SystemTime> {
    let mut latest: Option<SystemTime> = None;
    for dir in get_app_dirs() {
        if let Ok(metadata) = fs::metadata(&dir) {
            if let Ok(mtime) = metadata.modified() {
                match latest {
                    Some(previous) if mtime > previous => latest = Some(mtime),
                    None => latest = Some(mtime),
                    _ => {}
                }
            }
        }
    }
    latest
}

/// Load cached app entries if the cache is fresh enough.
pub fn load_cache() -> Option<Vec<AppEntry>> {
    let path = app_cache_file().ok()?;
    let cache_metadata = fs::metadata(&path).ok()?;
    let cache_mtime = cache_metadata.modified().ok()?;

    // Invalidate if any source directory changed after the cache was written.
    if let Some(source_mtime) = source_dirs_max_mtime() {
        if source_mtime > cache_mtime {
            return None;
        }
    }

    let data = fs::read_to_string(&path).ok()?;
    let cache: AppCache = serde_json::from_str(&data).ok()?;
    if cache.version != CACHE_VERSION {
        return None;
    }
    Some(cache.apps)
}

/// Save app entries to the XDG cache directory with an atomic replacement.
pub fn save_cache(apps: &[AppEntry]) -> std::io::Result<()> {
    let cache = AppCache {
        version: CACHE_VERSION,
        apps: apps.to_vec(),
    };
    let data = serde_json::to_vec(&cache)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    atomic_write(&app_cache_file()?, &data)
}
