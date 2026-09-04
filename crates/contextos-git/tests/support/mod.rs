use contextos_core::{Clock, VaultPath, VaultPathInput, VaultRoot, VaultSet, WritesVault};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
pub struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-07-18 18:30:00 +10:00)
    }
}

pub fn writer(
    root: &VaultRoot,
) -> Result<impl WritesVault<Error = contextos_fs::FsError> + Clone, Box<dyn std::error::Error>> {
    let filesystem = filesystem(root)?;
    Ok(FilesystemService::from(FilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
    }))
}

pub fn filesystem(root: &VaultRoot) -> Result<Filesystem, Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![root.clone()])?;
    Ok(Filesystem::try_from(FilesystemConfig {
        roots,
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?)
}

#[allow(dead_code, reason = "not every integration-test crate needs path construction")]
pub fn vault_path(root: &VaultRoot, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![root.clone()])?;
    Ok(VaultPath::try_from(VaultPathInput { roots: &roots, raw })?)
}
