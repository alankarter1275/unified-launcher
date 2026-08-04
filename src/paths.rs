//! XDG-aware filesystem locations and runtime socket helpers.
//!
//! The launcher keeps configuration, persistent data, cache, and runtime
//! artifacts separate. Runtime IPC deliberately avoids a shared `/tmp` path.

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

const APP_NAME: &str = "unified-launcher";
const SOCKET_NAME: &str = "unified-launcher.sock";

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn home_dir() -> io::Result<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| invalid_input("HOME is not set to an absolute path"))
}

fn xdg_base_dir(variable: &str, fallback: impl FnOnce(PathBuf) -> PathBuf) -> io::Result<PathBuf> {
    match env::var_os(variable).filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(invalid_input(format!("{variable} must be an absolute path")))
            }
        }
        None => Ok(fallback(home_dir()?)),
    }
}

fn app_dir(variable: &str, fallback: impl FnOnce(PathBuf) -> PathBuf) -> io::Result<PathBuf> {
    Ok(xdg_base_dir(variable, fallback)?.join(APP_NAME))
}

/// Return the per-user configuration directory without creating it.
pub fn config_dir() -> io::Result<PathBuf> {
    app_dir("XDG_CONFIG_HOME", |home| home.join(".config"))
}

/// Return the per-user persistent-data directory without creating it.
pub fn data_dir() -> io::Result<PathBuf> {
    app_dir("XDG_DATA_HOME", |home| home.join(".local/share"))
}

/// Return the per-user cache directory without creating it.
pub fn cache_dir() -> io::Result<PathBuf> {
    app_dir("XDG_CACHE_HOME", |home| home.join(".cache"))
}

/// Return the future human-editable configuration file location.
pub fn config_file() -> io::Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Return the persistent launcher-state file location.
pub fn state_file() -> io::Result<PathBuf> {
    Ok(data_dir()?.join("state.json"))
}

/// Return the application cache file location.
pub fn app_cache_file() -> io::Result<PathBuf> {
    Ok(cache_dir()?.join("apps_cache.json"))
}

/// Create a directory with owner-only permissions, or tighten an existing one.
pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_input(format!(
            "{} must be a real directory",
            path.display()
        )));
    }

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn runtime_directory() -> io::Result<PathBuf> {
    match env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(invalid_input("XDG_RUNTIME_DIR must be an absolute path"));
            }

            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(invalid_input("XDG_RUNTIME_DIR must be a real directory"));
            }

            // A system-provided XDG runtime directory is expected to be private
            // to the logged-in user. Refuse a group/world-accessible location
            // rather than placing an IPC endpoint in an unsafe directory.
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "XDG_RUNTIME_DIR is not private to the current user",
                ));
            }

            Ok(path)
        }
        None => {
            // This fallback remains outside shared /tmp and is private to the
            // user. Normal graphical Sway sessions provide XDG_RUNTIME_DIR.
            let path = data_dir()?.join("runtime");
            ensure_private_directory(&path)?;
            Ok(path)
        }
    }
}

/// Resolve the Unix socket path.
///
/// `UNIFIED_LAUNCHER_SOCKET` remains available for development and testing. In
/// normal sessions the endpoint is `$XDG_RUNTIME_DIR/unified-launcher.sock`.
pub fn socket_path() -> io::Result<PathBuf> {
    if let Some(value) = env::var_os("UNIFIED_LAUNCHER_SOCKET").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
        return Err(invalid_input(
            "UNIFIED_LAUNCHER_SOCKET must be an absolute path",
        ));
    }

    Ok(runtime_directory()?.join(SOCKET_NAME))
}

/// Remove only a stale socket at `path`; reject any other filesystem object.
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to replace non-socket IPC path at {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Restrict a freshly bound socket to the current user.
pub fn make_socket_private(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_directory_is_created_with_owner_only_permissions() {
        let path = env::temp_dir().join(format!(
            "unified-launcher-paths-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);

        ensure_private_directory(&path).expect("create private directory");
        let mode = fs::metadata(&path)
            .expect("read metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);

        fs::remove_dir_all(path).expect("remove test directory");
    }
}
