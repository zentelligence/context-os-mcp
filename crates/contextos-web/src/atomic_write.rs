//! Atomic replacement for a single file this crate owns exclusively (a
//! configuration file, or a generated OS service definition): write a
//! uniquely named temporary file in the target directory, then replace the
//! target with a single rename, so a reader never observes a partially
//! written file (`security.md`'s persistence discipline).
//!
//! Shared by `routes::settings` (`web.toml`) and `service` (generated
//! systemd/launchd service definitions) rather than each keeping its own
//! copy.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Writes `contents` to `path` via a uniquely named temporary file in the
/// same directory, replaced atomically.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when the temporary file cannot be
/// written or the rename onto `path` fails; `path` is left unchanged on
/// either failure.
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temp_path = directory.to_path_buf();
    temp_path.push(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        uniqueness_token()
    ));
    std::fs::write(&temp_path, contents)?;
    std::fs::rename(&temp_path, path)
}

fn uniqueness_token() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "atomic_write_test.rs"]
mod tests;
