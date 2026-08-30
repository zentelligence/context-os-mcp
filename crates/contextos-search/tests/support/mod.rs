pub mod http_stub;

use std::path::PathBuf;

use contextos_core::{PathError, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_search::{DocumentSource, IndexedDocument};
use time::OffsetDateTime;

fn sole_root(path: PathBuf) -> Result<VaultSet, PathError> {
    VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path,
        managed: true,
        name: Some("vault".to_owned()),
    })?])
}

pub fn vault_note(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<(VaultSet, VaultPath), Box<dyn std::error::Error>> {
    let absolute = vault.path().join(relative);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&absolute, content)?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: relative,
    })?;
    Ok((roots, path))
}

// Shared across every integration test binary in this crate; not every
// binary uses every helper, so an unused helper in one binary is expected
// rather than genuine dead code.
#[allow(dead_code)]
pub fn timestamp() -> Result<OffsetDateTime, Box<dyn std::error::Error>> {
    Ok(OffsetDateTime::from_unix_timestamp(1_770_000_000)?)
}

#[allow(dead_code)]
pub fn document(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<IndexedDocument, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified: timestamp()?,
    }))
}
