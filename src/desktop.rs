//! Desktop entry crawling and icon resolution.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tokio::fs::{read_dir, File};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::types::AppEntry;

/// Try to find an icon file on disk for the given icon name.
pub fn find_icon(name: &str) -> Option<String> {
    if name.starts_with('/') && PathBuf::from(name).exists() {
        return Some(name.to_string());
    }
    let extensions = ["svg", "png", "xpm"];
    let base_directories = [
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/48x48/apps",
        "/usr/share/icons/Papirus/64x64/apps",
        "/usr/share/pixmaps",
    ];
    for base_directory in &base_directories {
        for extension in &extensions {
            let path = format!("{base_directory}/{name}.{extension}");
            if PathBuf::from(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Standard application directory paths.
pub fn get_app_dirs() -> Vec<PathBuf> {
    let mut directories = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        directories.push(PathBuf::from(format!("{home}/.local/share/applications")));
        let flatpak_user = PathBuf::from(format!(
            "{home}/.local/share/flatpak/exports/share/applications"
        ));
        if flatpak_user.exists() {
            directories.push(flatpak_user);
        }
    }
    let snap_path = PathBuf::from("/var/lib/snapd/desktop/applications");
    if snap_path.exists() {
        directories.push(snap_path);
    }
    directories
}

/// Crawl all application directories and parse `.desktop` files.
///
/// The desktop-file name becomes the stable launcher identity. Later
/// directories win, allowing user-level desktop entries to override system
/// entries with the same desktop-file name.
pub async fn crawl_desktop_entries() -> Vec<AppEntry> {
    let mut entries_by_id: BTreeMap<String, AppEntry> = BTreeMap::new();

    for directory_path in get_app_dirs() {
        if !directory_path.exists() {
            continue;
        }
        if let Ok(mut directory) = read_dir(&directory_path).await {
            while let Ok(Some(entry)) = directory.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) == Some("desktop") {
                    if let Some(app) = parse_single_desktop_file(&path).await {
                        entries_by_id.insert(app.id.clone(), app);
                    }
                }
            }
        }
    }

    entries_by_id.into_values().collect()
}

/// Parse a single desktop file into an [`AppEntry`].
pub async fn parse_single_desktop_file(path: &PathBuf) -> Option<AppEntry> {
    let id = path.file_name()?.to_str()?.to_string();
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
                .filter(|word| !word.starts_with('%'))
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
            id,
            name,
            exec,
            icon,
            needs_terminal,
        })
    }
}
