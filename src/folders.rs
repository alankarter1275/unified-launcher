//! Folder-pin validation and lightweight path completion.
//!
//! Completion only reads direct child directories of the typed path. It never
//! indexes the entire disk, which keeps it suitable for the launcher's HDD-first
//! performance target.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "HOME is not set to an absolute path".to_string())
}

/// Expand `~` and `~/…`; other input must be an absolute path.
pub fn expand_path(input: &str) -> Result<PathBuf, String> {
    let input = input.trim();
    let home = home_dir()?;

    if input.is_empty() || input == "~" {
        return Ok(home);
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return Ok(home.join(rest));
    }

    let path = PathBuf::from(input);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("Path must be absolute or begin with ~/".to_string())
    }
}

fn display_path(path: &Path, home: &Path, prefer_tilde: bool) -> String {
    if prefer_tilde {
        if let Ok(relative) = path.strip_prefix(home) {
            if relative.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", relative.to_string_lossy());
        }
    }
    path.to_string_lossy().into_owned()
}

/// Canonicalize and validate a directory chosen for a folder pin.
pub fn resolve_directory(input: &str) -> Result<String, String> {
    let path = expand_path(input)?;
    let canonical = fs::canonicalize(&path)
        .map_err(|error| format!("Could not access {}: {error}", path.display()))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| format!("Could not inspect {}: {error}", canonical.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

/// Return direct-child directory suggestions for the current path input.
pub fn path_suggestions(input: &str) -> Result<Vec<String>, String> {
    let input = input.trim();
    let home = home_dir()?;
    let expanded = expand_path(input)?;
    let (directory, prefix) = if input.is_empty() || input == "~" || input.ends_with('/') {
        (expanded, String::new())
    } else {
        let parent = expanded
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "Path has no parent directory".to_string())?;
        let prefix = expanded
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        (parent, prefix)
    };

    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    let prefer_tilde = input.starts_with('~') || input.is_empty();
    let prefix_lower = prefix.to_ascii_lowercase();
    let mut suggestions = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let is_directory = path.is_dir();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_directory && name.to_ascii_lowercase().starts_with(&prefix_lower) {
                Some(display_path(&path, &home, prefer_tilde))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    suggestions.sort_by_key(|value| value.to_ascii_lowercase());
    suggestions.truncate(8);
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_paths_are_accepted() {
        assert_eq!(
            expand_path("/tmp").expect("absolute path"),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn relative_paths_are_rejected() {
        assert!(expand_path("Documents").is_err());
    }
}
