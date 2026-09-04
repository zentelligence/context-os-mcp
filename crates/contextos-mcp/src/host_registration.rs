//! Registers, deregisters, and reports on this server's entry in Claude
//! Desktop's `claude_desktop_config.json`.
//!
//! Only the single `mcpServers.contextos` key is ever inserted, removed, or
//! read; every other key, and every other registered server's entry,
//! passes through the parsed `serde_json::Value` tree unchanged.
//! "Byte-for-byte" preservation is implemented here as "every untouched
//! key's *value* is unchanged" through a JSON parse/reserialise round trip
//! (pretty-printed, 2-space indent, matching Claude Desktop's own
//! convention): this workspace has no JSON equivalent of `toml_edit`, so
//! literal preservation of the original file's formatting outside the
//! edited key is not attempted.
//!
//! Every write re-checks that the host is not currently running as the
//! first step inside [`register`]/[`deregister`] themselves, not earlier in
//! CLI argument processing, so the check is a real re-check immediately
//! before the write. A timestamped backup of the pre-edit file is
//! written first, and the write is verified by reading the file back
//! afterwards and confirming the intended value actually landed.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use thiserror::Error;

use crate::config_io::{ConfigIoError, write_file_atomically};

/// The `mcpServers` key this server registers itself under.
const SERVER_KEY: &str = "contextos";

/// Case-insensitive substring matched against a running process's name to
/// detect Claude Desktop.
const CLAUDE_DESKTOP_PROCESS_NEEDLE: &str = "claude";

/// One `mcpServers.contextos` entry: the command and arguments Claude
/// Desktop should launch this server with.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisteredServer {
    pub command: String,
    pub args: Vec<String>,
}

/// The current registration state read by [`status`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationStatus {
    Registered(RegisteredServer),
    NotRegistered,
}

/// The outcome of [`deregister`]: whether an entry was actually present to
/// remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeregisterOutcome {
    Removed,
    NotRegistered,
}

/// Detects whether a process matching a name substring is currently
/// running, abstracted so the "re-check immediately before the write"
/// requirement is testable without a real Claude Desktop install.
pub trait DetectsRunningProcesses {
    fn is_running(&self, name_needle: &str) -> bool;
}

/// The real [`DetectsRunningProcesses`] implementation, backed by
/// `sysinfo`'s process enumeration. A fresh [`System`] is created and
/// refreshed on every call rather than held across calls, so each check is
/// a genuine, uncached snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessDetector;

impl DetectsRunningProcesses for SystemProcessDetector {
    fn is_running(&self, name_needle: &str) -> bool {
        let mut system =
            System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()));
        system.refresh_processes(ProcessesToUpdate::All, true);
        let needle = name_needle.to_ascii_lowercase();
        system
            .processes()
            .values()
            .any(|process| process.name().to_string_lossy().to_ascii_lowercase().contains(&needle))
    }
}

/// Returns whether Claude Desktop is currently running.
#[must_use]
pub fn is_claude_desktop_running(detector: &dyn DetectsRunningProcesses) -> bool {
    detector.is_running(CLAUDE_DESKTOP_PROCESS_NEEDLE)
}

/// Reports the current `mcpServers.contextos` registration in the config
/// file at `path`, performing no write.
///
/// # Errors
///
/// Returns [`HostRegistrationError`] when the file exists but cannot be
/// read or parsed, or when it parses but is not a JSON object, or when a
/// present `contextos` entry does not match the expected shape.
pub fn status(path: &Path) -> Result<RegistrationStatus, HostRegistrationError> {
    let document = load_document(path)?;
    match server_entry(path, &document)? {
        Some(entry) => Ok(RegistrationStatus::Registered(entry)),
        None => Ok(RegistrationStatus::NotRegistered),
    }
}

/// Registers `entry` under `mcpServers.contextos` in the config file at
/// `path`, creating the file (and its parent directory) if it does not yet
/// exist.
///
/// # Errors
///
/// Returns [`HostRegistrationError::HostRunning`] when the host is detected
/// running and `force` is `false`. Returns other [`HostRegistrationError`]
/// variants for a read, parse, backup, write, or read-back-verification
/// failure; no write happens on any of those paths.
pub fn register(
    path: &Path,
    entry: &RegisteredServer,
    detector: &dyn DetectsRunningProcesses,
    force: bool,
) -> Result<(), HostRegistrationError> {
    if is_claude_desktop_running(detector) && !force {
        return Err(HostRegistrationError::HostRunning);
    }

    let mut document = load_document(path)?;
    let entry_value = serde_json::to_value(entry).map_err(|source| HostRegistrationError::SerialiseEntry { source })?;
    let servers = document
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(HostRegistrationError::MalformedStructure {
            path: path.to_path_buf(),
            key: "mcpServers".to_owned(),
        });
    };
    servers.insert(SERVER_KEY.to_owned(), entry_value);

    write_document(path, &document)?;

    match status(path)? {
        RegistrationStatus::Registered(written) if &written == entry => Ok(()),
        RegistrationStatus::Registered(written) => Err(HostRegistrationError::ReadBackMismatch {
            path: path.to_path_buf(),
            expected: format!("{entry:?}"),
            found: format!("{written:?}"),
        }),
        RegistrationStatus::NotRegistered => Err(HostRegistrationError::ReadBackMismatch {
            path: path.to_path_buf(),
            expected: format!("{entry:?}"),
            found: "no entry".to_owned(),
        }),
    }
}

/// Removes the `mcpServers.contextos` entry from the config file at `path`.
/// A missing file, or a file with no such entry, is reported as
/// [`DeregisterOutcome::NotRegistered`], not an error.
///
/// # Errors
///
/// Returns [`HostRegistrationError::HostRunning`] when the host is detected
/// running and `force` is `false`. Returns other [`HostRegistrationError`]
/// variants for a read, parse, backup, write, or read-back-verification
/// failure; no write happens on any of those paths.
pub fn deregister(
    path: &Path,
    detector: &dyn DetectsRunningProcesses,
    force: bool,
) -> Result<DeregisterOutcome, HostRegistrationError> {
    if is_claude_desktop_running(detector) && !force {
        return Err(HostRegistrationError::HostRunning);
    }

    let mut document = load_document(path)?;
    let removed = match document.get_mut("mcpServers") {
        Some(Value::Object(servers)) => servers.remove(SERVER_KEY).is_some(),
        Some(_) => {
            return Err(HostRegistrationError::MalformedStructure {
                path: path.to_path_buf(),
                key: "mcpServers".to_owned(),
            });
        }
        None => false,
    };
    if !removed {
        return Ok(DeregisterOutcome::NotRegistered);
    }

    write_document(path, &document)?;

    match status(path)? {
        RegistrationStatus::NotRegistered => Ok(DeregisterOutcome::Removed),
        RegistrationStatus::Registered(found) => Err(HostRegistrationError::ReadBackMismatch {
            path: path.to_path_buf(),
            expected: "no entry".to_owned(),
            found: format!("{found:?}"),
        }),
    }
}

/// Loads `path` into a JSON object, or an empty one when the file does not
/// exist yet (the same "absent file means fresh start" precedent
/// `load_config_document` sets for `config.toml`).
fn load_document(path: &Path) -> Result<Map<String, Value>, HostRegistrationError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(source) => {
            return Err(HostRegistrationError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: Value = serde_json::from_str(&text).map_err(|source| HostRegistrationError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(HostRegistrationError::MalformedStructure {
            path: path.to_path_buf(),
            key: "<root>".to_owned(),
        }),
    }
}

/// Reads the `mcpServers.contextos` entry out of an already-loaded
/// document, if present.
fn server_entry(path: &Path, document: &Map<String, Value>) -> Result<Option<RegisteredServer>, HostRegistrationError> {
    let Some(servers) = document.get("mcpServers") else {
        return Ok(None);
    };
    let Value::Object(servers) = servers else {
        return Err(HostRegistrationError::MalformedStructure {
            path: path.to_path_buf(),
            key: "mcpServers".to_owned(),
        });
    };
    let Some(entry) = servers.get(SERVER_KEY) else {
        return Ok(None);
    };
    let entry =
        serde_json::from_value(entry.clone()).map_err(|source| HostRegistrationError::DeserialiseEntry { source })?;
    Ok(Some(entry))
}

/// Backs up an existing `path` (skipped when it does not yet exist), then
/// atomically writes `document`'s pretty-printed JSON over it.
fn write_document(path: &Path, document: &Map<String, Value>) -> Result<(), HostRegistrationError> {
    if path.exists() {
        backup(path)?;
    }
    let rendered = serde_json::to_vec_pretty(document).map_err(|source| HostRegistrationError::SerialiseDocument {
        path: path.to_path_buf(),
        source,
    })?;
    write_file_atomically(path, &rendered).map_err(|source| HostRegistrationError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes a timestamped copy of `path` (`<path>.bak-<unix-seconds>`)
/// alongside it before any edit, as defence in depth,
/// since the running-process check alone cannot close the TOCTOU window
/// against an external application this server does not control.
fn backup(path: &Path) -> Result<(), HostRegistrationError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let mut backup_path = path.as_os_str().to_owned();
    backup_path.push(format!(".bak-{timestamp}"));
    let backup_path = PathBuf::from(backup_path);
    fs::copy(path, &backup_path)
        .map(|_| ())
        .map_err(|source| HostRegistrationError::Backup {
            path: backup_path,
            source,
        })
}

/// Typed failures registering, deregistering, or reporting on Claude
/// Desktop's `mcpServers.contextos` entry.
#[derive(Debug, Error)]
pub enum HostRegistrationError {
    #[error("Claude Desktop is currently running; close it first, or pass --force")]
    HostRunning,
    #[error("could not read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not valid JSON: {source}", path.display())]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{} does not have the expected object structure at {key}", path.display())]
    MalformedStructure { path: PathBuf, key: String },
    #[error("could not interpret the registered contextos entry: {source}")]
    DeserialiseEntry {
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialise the contextos entry: {source}")]
    SerialiseEntry {
        #[source]
        source: serde_json::Error,
    },
    #[error("could not serialise {}: {source}", path.display())]
    SerialiseDocument {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not back up {} before editing it: {source}", path.display())]
    Backup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: ConfigIoError,
    },
    #[error(
        "wrote {} but the read-back check found unexpected content (expected {expected}, found {found})",
        path.display()
    )]
    ReadBackMismatch {
        path: PathBuf,
        expected: String,
        found: String,
    },
}

#[cfg(test)]
#[path = "host_registration_test.rs"]
mod tests;
