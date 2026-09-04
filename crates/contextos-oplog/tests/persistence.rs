use contextos_core::{
    Clock, LogsOperations, OpKind, OperationEvent, Origin, VaultPath, VaultPathInput, VaultRoot, VaultRootInput,
    VaultSet,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_oplog::{ManualLogInput, OperationLog, OperationLogConfig};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-07-18 18:30:00 +10:00)
    }
}

fn path(roots: &VaultSet, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[test]
fn appends_origin_aware_lines_without_rewriting_existing_content() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/log/2026/07"))?;
    let log_path = vault.path().join("memory/log/2026/07/2026-07-18.md");
    std::fs::write(
        &log_path,
        "# 2026-07-18: Operation Log\n\n17:00:00 | create | Existing legacy entry | files: notes/existing.md\n",
    )?;
    std::fs::create_dir(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/a.md"), "alpha")?;
    std::fs::write(vault.path().join("notes/b.md"), "beta")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let appender = FilesystemService::from(FilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
    });
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots: roots.clone(),
        relative_directory: "memory/log".to_owned(),
        appender,
    })?;

    let first = log
        .append(&OperationEvent {
            kind: OpKind::Modify,
            paths: vec![path(&roots, "notes/a.md")?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Overwrote notes/a.md (5 bytes)".to_owned(),
            at: datetime!(2026-07-18 18:30:00 +10:00),
        })
        .map_err(|warning| warning.message)?;
    let second = log
        .append(&OperationEvent {
            kind: OpKind::Move,
            paths: vec![path(&roots, "notes/a.md")?, path(&roots, "notes/b.md")?],
            origin: Origin::Tool("fs_move_file".to_owned()),
            summary: "Moved notes/a.md to notes/b.md".to_owned(),
            at: datetime!(2026-07-18 18:31:02 +10:00),
        })
        .map_err(|warning| warning.message)?;

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].origin, Origin::Internal("oplog".to_owned()));
    assert_eq!(
        std::fs::read_to_string(log_path)?,
        concat!(
            "# 2026-07-18: Operation Log\n\n",
            "17:00:00 | create | Existing legacy entry | files: notes/existing.md\n",
            "18:30:00 | fs_write_file | modify | Overwrote notes/a.md (5 bytes) | files: notes/a.md\n",
            "18:31:02 | fs_move_file | move | Moved notes/a.md to notes/b.md | files: notes/a.md, notes/b.md\n",
        )
    );
    Ok(())
}

#[test]
fn manual_append_uses_manual_origin_and_log_operation() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots: roots.clone(),
        relative_directory: "memory/log".to_owned(),
        appender: FilesystemService::from(FilesystemServiceConfig {
            filesystem,
            clock: FixedClock,
        }),
    })?;

    log.append_manual(&ManualLogInput {
        entry: "Reviewed priorities | retained operator intent".to_owned(),
        files: vec![path(&roots, "note.md")?],
        at: datetime!(2026-07-18 18:32:00 +10:00),
    })?;

    let persisted = std::fs::read_to_string(vault.path().join("memory/log/2026/07/2026-07-18.md"))?;
    assert!(
        persisted
            .contains("18:32:00 | manual | log | Reviewed priorities \\| retained operator intent | files: note.md\n")
    );
    Ok(())
}
