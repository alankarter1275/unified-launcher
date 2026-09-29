//! Incremental, home-directory file index and fuzzy search.
//!
//! The index is built in a blocking worker so the launcher daemon remains
//! responsive. It publishes small batches while scanning and deliberately skips
//! common cache/build trees that are both noisy and expensive on HDDs.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::types::FileSearchResult;

const PUBLISH_BATCH_SIZE: usize = 256;
const RESULT_LIMIT: usize = 100;

#[derive(Debug, Clone)]
struct IndexedPath {
    name: String,
    path: String,
    display_path: String,
    is_directory: bool,
}

/// Shared state read by daemon client handlers while a background worker scans.
#[derive(Debug, Default)]
pub struct FileIndex {
    entries: Vec<IndexedPath>,
    indexing: bool,
}

pub type FileIndexHandle = Arc<RwLock<FileIndex>>;

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "HOME is not set to an absolute path".to_string())
}

fn should_skip_directory(name: &str) -> bool {
    matches!(
        name,
        ".cache"
            | ".cargo"
            | ".git"
            | ".gradle"
            | ".local"
            | ".npm"
            | ".rustup"
            | ".var"
            | "__pycache__"
            | "build"
            | "coverage"
            | "dist"
            | "node_modules"
            | "target"
    )
}

fn display_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Ok(relative) => format!("~/{}", relative.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn publish_batch(index: &FileIndexHandle, batch: &mut Vec<IndexedPath>) {
    if batch.is_empty() {
        return;
    }
    if let Ok(mut index) = index.write() {
        index.entries.append(batch);
    }
}

fn scan_directory(root: &Path, home: &Path, index: &FileIndexHandle, batch: &mut Vec<IndexedPath>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        // Do not follow symlinks while recursively scanning: following them can
        // escape HOME or create loops. They remain available through normal
        // filesystem navigation in Yazi.
        if file_type.is_symlink() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let is_directory = file_type.is_dir();
        if is_directory && should_skip_directory(&name) {
            continue;
        }

        batch.push(IndexedPath {
            name,
            path: path.to_string_lossy().into_owned(),
            display_path: display_path(&path, home),
            is_directory,
        });
        if batch.len() >= PUBLISH_BATCH_SIZE {
            publish_batch(index, batch);
        }

        if is_directory {
            scan_directory(&path, home, index, batch);
        }
    }
}

/// Begin an incremental scan of `$HOME` in a background worker.
pub fn start() -> FileIndexHandle {
    let index = Arc::new(RwLock::new(FileIndex {
        indexing: true,
        ..FileIndex::default()
    }));
    let worker_index = Arc::clone(&index);

    tokio::task::spawn_blocking(move || {
        let home = match home_dir() {
            Ok(home) => home,
            Err(_) => {
                if let Ok(mut index) = worker_index.write() {
                    index.indexing = false;
                }
                return;
            }
        };

        let mut batch = Vec::with_capacity(PUBLISH_BATCH_SIZE);
        scan_directory(&home, &home, &worker_index, &mut batch);
        publish_batch(&worker_index, &mut batch);
        if let Ok(mut index) = worker_index.write() {
            index.indexing = false;
        }
    });

    index
}

/// Search indexed file names and paths with filename-biased fuzzy ranking.
pub fn search(index: &FileIndexHandle, query: &str) -> (Vec<FileSearchResult>, bool, usize) {
    let index = match index.read() {
        Ok(index) => index,
        Err(_) => return (Vec::new(), false, 0),
    };
    let indexed_count = index.entries.len();
    let indexing = index.indexing;
    let query = query.trim();
    if query.is_empty() {
        return (Vec::new(), indexing, indexed_count);
    }

    let matcher = SkimMatcherV2::default();
    let prioritize_path = query.contains('/');
    let mut matches = index
        .entries
        .iter()
        .filter_map(|entry| {
            let name_score = matcher.fuzzy_match(&entry.name, query);
            let path_score = matcher.fuzzy_match(&entry.display_path, query);
            let score = if prioritize_path {
                path_score.or(name_score)?
            } else {
                match (name_score, path_score) {
                    (Some(name), Some(path)) => name.saturating_add(120).max(path),
                    (Some(name), None) => name.saturating_add(120),
                    (None, Some(path)) => path,
                    (None, None) => return None,
                }
            };
            Some((
                score,
                FileSearchResult {
                    name: entry.name.clone(),
                    path: entry.path.clone(),
                    display_path: entry.display_path.clone(),
                    is_directory: entry.is_directory,
                },
            ))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|item| std::cmp::Reverse(item.0));
    let results = matches
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|(_, result)| result)
        .collect();
    (results, indexing, indexed_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noisy_directories_are_excluded() {
        assert!(should_skip_directory("node_modules"));
        assert!(should_skip_directory(".cache"));
        assert!(!should_skip_directory("Documents"));
    }

    #[test]
    fn home_paths_are_displayed_compactly() {
        let home = Path::new("/home/example");
        assert_eq!(
            display_path(Path::new("/home/example/Documents/notes.md"), home),
            "~/Documents/notes.md"
        );
    }
}
