//! Desktop entry crawling and icon resolution.

use std::path::PathBuf;
use tokio::fs::{read_dir, File};
use tokio::io::{AsyncBufReadExt, BufReader};
use crate::types::AppEntry;

/// Try to find an icon file on disk for the given icon name.
pub fn find_icon(name: &str) -> Option<String> {
    if name.starts_with('/') && PathBuf::from(name).exists() {
        return Some(name.to_string());
    }
    let exts = ["svg", "png", "xpm"];
    let bases = [
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/48x48/apps",
        "/usr/share/icons/Papirus/64x64/apps",
        "/usr/share/pixmaps",
    ];
    for base in &bases {
        for ext in &exts {
            let path = format!("{}/{}.{}", base, name, ext);
            if PathBuf::from(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Standard application directory paths.
pub fn get_app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
        let flatpak_user = PathBuf::from(format!(
            "{}/.local/share/flatpak/exports/share/applications",
            home
        ));
        if flatpak_user.exists() {
            dirs.push(flatpak_user);
        }
    }
    let snap_path = PathBuf::from("/var/lib/snapd/desktop/applications");
    if snap_path.exists() {
        dirs.push(snap_path);
    }
    dirs
}

/// Crawl all application directories and parse .desktop files.
pub async fn crawl_desktop_entries() -> Vec<AppEntry> {
    let mut entries = Vec::new();
    for dir_path in get_app_dirs() {
        if !dir_path.exists() {
            continue;
        }
        if let Ok(mut dir) = read_dir(&dir_path).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Some(app) = parse_single_desktop_file(&path).await {
                        entries.push(app);
                    }
                }
            }
        }
    }
    entries
}

/// Parse a single .desktop file into an AppEntry.
pub async fn parse_single_desktop_file(path: &PathBuf) -> Option<AppEntry> {
    let file = File::open(path).await.ok()?;
    let mut reader = BufReader::new(file).lines();
    let mut name = String::new();
    let mut exec = String::new();
    let mut icon = None;
    let mut no_display = false;
    let mut needs_terminal = false;
    while let Ok(Some(line)) = reader.next_line().await {
        if line.starts_with("Name=") && !line.contains("Name[") && name.is_empty() {
            name = line[5..].trim().to_string();
        } else if line.starts_with("Exec=") && exec.is_empty() {
            exec = line[5..]
                .split_whitespace()
                .filter(|&w| !w.starts_with('%'))
                .collect::<Vec<_>>()
                .join(" ");
        } else if line.starts_with("Icon=") && icon.is_none() {
            icon = find_icon(line[5..].trim());
        } else if line.starts_with("NoDisplay=true") || line.starts_with("Hidden=true") {
            no_display = true;
        } else if line.starts_with("Terminal=true") {
            needs_terminal = true;
        }
    }
    if no_display || name.is_empty() || exec.is_empty() {
        None
    } else {
        Some(AppEntry {
            name,
            exec,
            icon,
            needs_terminal,
        })
    }
}
