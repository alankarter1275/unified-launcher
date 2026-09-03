//! Lightweight, local Markdown note storage.
//!
//! Each note is a readable file under the launcher's XDG data directory. The
//! first Markdown heading is its title; the remainder is plain note content.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::paths::{data_dir, ensure_private_directory};
use crate::storage::atomic_write;

/// Metadata shown in the note list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
}

/// A complete editable note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
}

fn notes_dir() -> io::Result<PathBuf> {
    Ok(data_dir()?.join("notes"))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn valid_id(id: &str) -> bool {
    id.starts_with("note-")
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn note_path(id: &str) -> io::Result<PathBuf> {
    if !valid_id(id) {
        return Err(invalid_input("invalid note identifier"));
    }
    Ok(notes_dir()?.join(format!("{id}.md")))
}

fn normalize_title(title: &str) -> String {
    let title = title
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

fn encode_note(title: &str, content: &str) -> String {
    format!("# {}\n\n{}", normalize_title(title), content)
}

fn decode_note(id: String, document: String) -> Note {
    if let Some(without_marker) = document.strip_prefix("# ") {
        let (title, content) = without_marker
            .split_once('\n')
            .map_or((without_marker, ""), |(title, content)| (title, content));
        return Note {
            id,
            title: normalize_title(title),
            content: content.strip_prefix('\n').unwrap_or(content).to_string(),
        };
    }

    Note {
        id,
        title: "Untitled".to_string(),
        content: document,
    }
}

fn read_note(path: PathBuf, id: String) -> io::Result<Note> {
    fs::read_to_string(path).map(|document| decode_note(id, document))
}

/// List notes newest-first. A missing notes directory simply means no notes yet.
pub fn list() -> io::Result<Vec<NoteSummary>> {
    let directory = notes_dir()?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut notes = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md")
                || !valid_id(&id)
            {
                return None;
            }
            let note = read_note(path.clone(), id).ok()?;
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            Some((
                modified,
                NoteSummary {
                    id: note.id,
                    title: note.title,
                },
            ))
        })
        .collect::<Vec<_>>();

    notes.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.title.cmp(&right.1.title))
    });
    Ok(notes.into_iter().map(|(_, note)| note).collect())
}

/// Load a note by identifier.
pub fn load(id: &str) -> io::Result<Note> {
    read_note(note_path(id)?, id.to_string())
}

/// Create an empty named note.
pub fn create(title: &str) -> io::Result<Note> {
    let directory = notes_dir()?;
    ensure_private_directory(&directory)?;

    let mut nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    loop {
        let id = format!("note-{}-{nonce}", std::process::id());
        let path = note_path(&id)?;
        if !path.exists() {
            let note = Note {
                id,
                title: normalize_title(title),
                content: String::new(),
            };
            atomic_write(&path, encode_note(&note.title, &note.content).as_bytes())?;
            return Ok(note);
        }
        nonce += 1;
    }
}

/// Atomically save a note's title and content.
pub fn save(id: &str, title: &str, content: &str) -> io::Result<Note> {
    let note = Note {
        id: id.to_string(),
        title: normalize_title(title),
        content: content.to_string(),
    };
    let path = note_path(&note.id)?;
    if !path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "note no longer exists",
        ));
    }
    atomic_write(&path, encode_note(&note.title, &note.content).as_bytes())?;
    Ok(note)
}

/// Delete a note file by identifier.
pub fn delete(id: &str) -> io::Result<()> {
    fs::remove_file(note_path(id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_document_round_trips() {
        let document = encode_note("Project ideas", "First thought\nSecond thought");
        let note = decode_note("note-1-2".to_string(), document);

        assert_eq!(note.title, "Project ideas");
        assert_eq!(note.content, "First thought\nSecond thought");
    }

    #[test]
    fn note_identifiers_reject_path_traversal() {
        assert!(valid_id("note-123-456"));
        assert!(!valid_id("../note-123"));
        assert!(!valid_id("note-123.md"));
    }
}
