//! Application cache: persists resolved entries to disk so we skip crawling on restart.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
use serde::{Deserialize, Serialize};
use crate::desktop::get_app_dirs;
use crate::types::AppEntry;

#[derive(Serialize, Deserialize)]
struct AppCache {
    version: u32,
    apps: Vec<AppEntry>,
}

fn cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let mut path = PathBuf::from(home);
    path.push(".cache/unified-launcher/apps_cache.json");
    path
}

/// Returns the most recent mtime across all application directories.
fn source_dirs_max_mtime() -> Option<SystemTime> {
    let mut latest: Option<SystemTime> = None;
    for dir in get_app_dirs() {
        if let Ok(meta) = fs::metadata(&dir) {
            if let Ok(mtime) = meta.modified() {
                match latest {
                    Some(t) if mtime > t => latest = Some(mtime),
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
    let path = cache_path();
    let cache_meta = fs::metadata(&path).ok()?;
    let cache_mtime = cache_meta.modified().ok()?;

    // Invalidate if any source directory changed after the cache was written
    if let Some(src_mtime) = source_dirs_max_mtime() {
        if src_mtime > cache_mtime {
            return None;
        }
    }

    let data = fs::read_to_string(&path).ok()?;
    let cache: AppCache = serde_json::from_str(&data).ok()?;
    if cache.version != 1 {
        return None;
    }
    Some(cache.apps)
}

/// Save app entries to disk cache.
pub fn save_cache(apps: &[AppEntry]) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let cache = AppCache {
        version: 1,
        apps: apps.to_vec(),
    };
    if let Ok(data) = serde_json::to_string(&cache) {
        let _ = fs::write(&path, data);
    }
}
