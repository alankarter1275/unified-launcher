//! Small, atomic persistence helpers for launcher-owned data.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths::ensure_private_directory;

/// Atomically replace `path` with `contents` using an owner-only temporary file.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot write a file without a parent directory",
        )
    })?;
    ensure_private_directory(parent)?;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("state"),
        std::process::id(),
        nonce
    ));

    let write_result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary_path, path)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_contents() {
        let root = std::env::temp_dir().join(format!(
            "unified-launcher-storage-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("state.json");

        atomic_write(&path, b"first").expect("first write");
        atomic_write(&path, b"second").expect("replacement write");

        assert_eq!(fs::read(&path).expect("read state"), b"second");
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
