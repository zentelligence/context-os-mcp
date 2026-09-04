//! Resolves each vault's derived-state directory: the text index, its lock
//! files, the link-graph cache, and the vector store.
//!
//! Default: a platform-local app-data directory keyed to the vault's
//! resolved root, so index segments, OS-level locks, and the vector store
//! never sit inside a directory a third-party sync tool (for example
//! Obsidian Sync) can observe or replicate. `[[vault]] state_directory`
//! overrides this per vault: a relative path resolves against the vault
//! root (opting back into in-vault storage), an absolute path is used
//! exactly as given.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Typed failure resolving a vault's derived-state directory.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum StateDirError {
    #[error(
        "could not resolve a platform app-data directory for derived vault state; set \
         [[vault]] state_directory explicitly"
    )]
    AppDataUnavailable,
}

/// Resolves one vault's derived-state directory per the precedence
/// described in the module documentation.
///
/// # Errors
///
/// Returns [`StateDirError::AppDataUnavailable`] when no override is
/// configured and the platform cannot resolve a home directory to root the
/// default under.
pub(crate) fn resolve_state_directory(
    override_directory: Option<&Path>,
    vault_root: &Path,
) -> Result<PathBuf, StateDirError> {
    match override_directory {
        Some(path) if path.is_absolute() => Ok(path.to_path_buf()),
        Some(path) => Ok(vault_root.join(path)),
        None => default_state_directory(vault_root),
    }
}

fn default_state_directory(vault_root: &Path) -> Result<PathBuf, StateDirError> {
    let app_data_root = ProjectDirs::from("", "", "contextos")
        .ok_or(StateDirError::AppDataUnavailable)?
        .data_local_dir()
        .to_path_buf();
    Ok(vault_state_directory(&app_data_root, vault_root))
}

/// Pure composition of an app-data root and a vault's state-directory key;
/// split out from [`default_state_directory`] so it is testable without
/// depending on the operator's actual home directory (`AGENTS.md` test
/// standards).
fn vault_state_directory(app_data_root: &Path, vault_root: &Path) -> PathBuf {
    app_data_root
        .join("vaults")
        .join(vault_state_key(vault_root))
}

/// Deterministic, collision-resistant key for a vault's resolved root
/// path: a lowercase SHA-256 hex digest, so two different vaults never
/// share a state directory and the same vault always resolves to the same
/// one.
fn vault_state_key(vault_root: &Path) -> String {
    let digest = Sha256::digest(vault_root.to_string_lossy().as_bytes());
    digest
        .iter()
        .fold(String::with_capacity(64), |mut key, byte| {
            let _ = write!(key, "{byte:02x}");
            key
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_vault_root_always_resolves_to_the_same_key() {
        let first = vault_state_key(Path::new("vaults/vault"));
        let second = vault_state_key(Path::new("vaults/vault"));
        assert_eq!(first, second);
    }

    #[test]
    fn different_vault_roots_resolve_to_different_keys() {
        let vault = vault_state_key(Path::new("vaults/vault"));
        let other = vault_state_key(Path::new("vaults/other"));
        assert_ne!(vault, other);
    }

    #[test]
    fn key_is_a_lowercase_sha256_hex_digest() {
        let key = vault_state_key(Path::new("vaults/vault"));
        assert_eq!(key.len(), 64);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn default_directory_is_scoped_under_the_app_data_root_and_a_vaults_segment() {
        let app_data_root = Path::new("app-data-root");
        let resolved = vault_state_directory(app_data_root, Path::new("vaults/vault"));
        assert!(resolved.starts_with(app_data_root.join("vaults")));
    }

    #[test]
    fn override_uses_an_absolute_path_exactly() -> Result<(), Box<dyn std::error::Error>> {
        let vault_root = tempfile::tempdir()?;
        let custom_state = tempfile::tempdir()?;

        let resolved = resolve_state_directory(Some(custom_state.path()), vault_root.path())?;

        assert_eq!(resolved, custom_state.path());
        Ok(())
    }

    #[test]
    fn override_resolves_a_relative_path_against_the_vault_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let vault_root = tempfile::tempdir()?;

        let resolved = resolve_state_directory(Some(Path::new(".contextos")), vault_root.path())?;

        assert_eq!(resolved, vault_root.path().join(".contextos"));
        Ok(())
    }
}
