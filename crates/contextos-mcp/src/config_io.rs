//! Reads and atomically writes a `config.toml` document from and to disk.
//! Kept separate from `config_writer.rs`'s pure in-memory edits so the pure
//! (rendered-text) and I/O (temp-file-then-rename) concerns stay testable
//! independently.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{ConfigDocument, ConfigWriterError};

/// Loads `path` into a [`ConfigDocument`] if it exists, or starts a fresh
/// one if it does not, matching `Config::try_from(ConfigLoadInput)`'s own
/// "missing config file means defaults" precedent (`config.rs`).
///
/// # Errors
///
/// Returns [`ConfigIoError::Read`] for any read failure other than the file
/// not existing, and [`ConfigIoError::InvalidToml`] when existing content is
/// not valid TOML.
pub fn load_config_document(path: &Path) -> Result<ConfigDocument, ConfigIoError> {
    match fs::read_to_string(path) {
        Ok(source) => ConfigDocument::parse(&source).map_err(|source| ConfigIoError::InvalidToml { source }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(ConfigDocument::new()),
        Err(source) => Err(ConfigIoError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Writes `document`'s rendered text to `path` via [`write_file_atomically`].
///
/// # Errors
///
/// Returns a typed [`ConfigIoError`] variant if the parent directory cannot
/// be created, the temporary file cannot be written or flushed, or the
/// rename fails.
pub fn write_config_document(path: &Path, document: &ConfigDocument) -> Result<(), ConfigIoError> {
    write_file_atomically(path, document.render().as_bytes())
}

/// Writes `contents` to `path`: a uniquely named temporary file in the same
/// directory, flushed, then atomically renamed over the target, the same
/// persistence discipline `security.md` requires for every vault write (see
/// `contextos_fs::mutate`'s `AppliesMutations::write`), applied here to a
/// file outside every vault root. Shared by [`write_config_document`]
/// (`config.toml`) and `host_registration.rs` (an external MCP host's own
/// config file), the one atomic-write primitive this workspace uses for any
/// non-vault file.
///
/// # Errors
///
/// Returns a typed [`ConfigIoError`] variant if the parent directory cannot
/// be created, the temporary file cannot be written or flushed, or the
/// rename fails.
pub(crate) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<(), ConfigIoError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| ConfigIoError::NoParentDirectory {
            path: path.to_path_buf(),
        })?;
    fs::create_dir_all(parent).map_err(|source| ConfigIoError::CreateParent {
        path: parent.to_path_buf(),
        source,
    })?;

    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| ConfigIoError::CreateTemporary {
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(contents)
        .map_err(|source| ConfigIoError::WriteTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| ConfigIoError::FlushTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| ConfigIoError::PersistTemporary {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    Ok(())
}

/// Typed failures reading or writing a `config.toml` document.
#[derive(Debug, Error)]
pub enum ConfigIoError {
    #[error("could not read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("existing configuration TOML is invalid")]
    InvalidToml {
        #[source]
        source: ConfigWriterError,
    },
    #[error("{} has no parent directory", path.display())]
    NoParentDirectory { path: PathBuf },
    #[error("could not create {}: {source}", path.display())]
    CreateParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not create a temporary file next to {}: {source}", path.display())]
    CreateTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write a temporary file for {}: {source}", path.display())]
    WriteTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not flush a temporary file for {}: {source}", path.display())]
    FlushTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not persist the temporary file to {}: {source}", path.display())]
    PersistTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
#[path = "config_io_test.rs"]
mod tests;
