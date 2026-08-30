use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use contextos_core::{
    Clock, ContentHash, CreateDirectoryMutation, DeleteMode, DeleteMutation, MoveMutation, OpKind,
    OperationEvent, OperationWarning, Origin, RoutesOperations, VaultPath, VaultPathInput,
    VaultRoot, VaultRootInput, VaultSet, WriteMutation,
};
use contextos_fs::{
    EditFileRequest, Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig,
    FsError, FsLimits, GuardsAtomicWrites, RoutedFilesystemServiceConfig, TextEdit,
    default_hidden_patterns,
};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-07-18 10:11:12 +10:00)
    }
}

#[test]
fn fr_14_hard_delete_removes_a_file_and_emits_one_delete_event()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    std::fs::write(root.path().join("obsolete.md"), "obsolete")?;
    let (service, roots) = fixture(root.path().to_path_buf())?;
    let path = path(&roots, "obsolete.md")?;

    let result = service.delete_path(&DeleteMutation {
        path,
        mode: DeleteMode::Hard,
        origin: Origin::Tool("fs_delete_file".to_owned()),
    })?;

    assert!(!root.path().join("obsolete.md").exists());
    assert!(result.value.deleted);
    assert_eq!(
        result.event.as_ref().map(|event| event.kind),
        Some(OpKind::Delete)
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn fr_14_trash_delete_moves_the_file_to_an_isolated_freedesktop_trash()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let data_home = tempdir()?;
    std::fs::write(vault.path().join("recoverable.md"), "recoverable")?;

    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", "fr_14_trash_delete_child"])
        .env("CONTEXTOS_MCP_TRASH_TEST_VAULT", vault.path())
        .env("XDG_DATA_HOME", data_home.path())
        .status()?;

    assert!(status.success());
    assert!(!vault.path().join("recoverable.md").exists());
    assert!(
        data_home
            .path()
            .join("Trash/files/recoverable.md")
            .is_file()
    );
    assert!(
        data_home
            .path()
            .join("Trash/info/recoverable.md.trashinfo")
            .is_file()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn fr_14_trash_delete_child() -> Result<(), Box<dyn std::error::Error>> {
    let Some(vault) = std::env::var_os("CONTEXTOS_MCP_TRASH_TEST_VAULT") else {
        return Ok(());
    };
    let vault = PathBuf::from(vault);
    let (service, roots) = fixture(vault)?;

    let result = service.delete_path(&DeleteMutation {
        path: path(&roots, "recoverable.md")?,
        mode: DeleteMode::Trash,
        origin: Origin::Tool("fs_delete_file".to_owned()),
    })?;

    assert!(result.value.deleted);
    assert!(result.value.trashed);
    assert_eq!(result.event.map(|event| event.kind), Some(OpKind::Delete));
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct DegradedSubstrate;

impl RoutesOperations for DegradedSubstrate {
    fn route(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        vec![OperationWarning {
            code: "index/stale".to_owned(),
            message: "index reconciliation queued".to_owned(),
        }]
    }
}

#[derive(Debug)]
struct InterruptAfterFlush;

impl GuardsAtomicWrites for InterruptAfterFlush {
    fn after_flush(&self, _target: &std::path::Path) -> Result<(), std::io::Error> {
        Err(std::io::Error::other(
            "deterministic interruption before atomic rename",
        ))
    }
}

#[derive(Debug)]
struct ParkAfterFlush;

impl GuardsAtomicWrites for ParkAfterFlush {
    fn after_flush(&self, _target: &std::path::Path) -> Result<(), std::io::Error> {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "CONTEXTOS_MCP_TEMPORARY_FLUSHED")?;
        stdout.flush()?;
        loop {
            std::thread::park();
        }
    }
}

fn fixture(
    root: PathBuf,
) -> Result<(FilesystemService<FixedClock>, VaultSet), Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: root,
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let service = FilesystemService::from(FilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
    });
    Ok((service, roots))
}

fn path(roots: &VaultSet, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[test]
fn fr_03_write_creates_parents_atomically_and_returns_hash()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;
    let note = path(&roots, "new/child/note.md")?;

    let result = service.write_file(&WriteMutation {
        path: note,
        content: "hello".to_owned(),
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    })?;

    assert!(result.value.created);
    assert_eq!(result.value.bytes_written, 5);
    assert_eq!(
        <&str>::from(&result.value.content_hash),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("new/child/note.md"))?,
        "hello"
    );
    assert!(result.event.is_some());
    Ok(())
}

#[test]
fn phase_2_filesystem_service_returns_secondary_warnings_after_persistence()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let service = FilesystemService::from(RoutedFilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
        services: DegradedSubstrate,
    });

    let result = service.write_file(&WriteMutation {
        path: path(&roots, "note.md")?,
        content: "persisted".to_owned(),
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    })?;

    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "persisted"
    );
    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings[0].code, "index/stale");
    Ok(())
}

#[test]
fn nfr_04_conflict_preserves_existing_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "original")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;

    let result = service.write_file(&WriteMutation {
        path: path(&roots, "note.md")?,
        content: "replacement".to_owned(),
        expected_hash: Some(ContentHash::from([0_u8; 32])),
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    });

    assert!(matches!(result, Err(FsError::Conflict { .. })));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "original"
    );
    Ok(())
}

#[test]
fn nfr_03_interruption_after_temporary_flush_preserves_old_content()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let target = vault.path().join("note.md");
    std::fs::write(&target, "original")?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: Some(Arc::new(InterruptAfterFlush)),
    })?;
    let service = FilesystemService::from(FilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
    });

    let result = service.write_file(&WriteMutation {
        path: path(&roots, "note.md")?,
        content: "replacement".to_owned(),
        expected_hash: None,
        force: true,
        origin: Origin::Tool("fs_write_file".to_owned()),
    });

    assert!(matches!(
        result,
        Err(FsError::AtomicWriteInterrupted { .. })
    ));
    assert_eq!(std::fs::read_to_string(target)?, "original");
    assert_eq!(std::fs::read_dir(vault.path())?.count(), 1);
    Ok(())
}

#[test]
fn nfr_03_process_kill_mid_write_preserves_complete_content()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let target = vault.path().join("note.md");
    std::fs::write(&target, "original")?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "nfr_03_process_kill_child", "--nocapture"])
        .env("CONTEXTOS_MCP_KILL_TEST_VAULT", vault.path())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("kill-test child stdout was not piped"))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut reached_flush_boundary = false;
    for line in &mut lines {
        if line?.contains("CONTEXTOS_MCP_TEMPORARY_FLUSHED") {
            reached_flush_boundary = true;
            break;
        }
    }
    if !reached_flush_boundary {
        let status = child.wait()?;
        return Err(std::io::Error::other(format!(
            "kill-test child exited before the flush boundary with {status}"
        ))
        .into());
    }

    child.kill()?;
    let status = child.wait()?;
    let content = std::fs::read_to_string(target)?;

    assert!(!status.success());
    assert!(content == "original" || content == "replacement");
    Ok(())
}

#[test]
fn nfr_03_process_kill_child() -> Result<(), Box<dyn std::error::Error>> {
    let Some(vault) = std::env::var_os("CONTEXTOS_MCP_KILL_TEST_VAULT") else {
        return Ok(());
    };
    let vault = PathBuf::from(vault);
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.clone(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: Some(Arc::new(ParkAfterFlush)),
    })?;
    let service = FilesystemService::from(FilesystemServiceConfig {
        filesystem,
        clock: FixedClock,
    });

    let result = service.write_file(&WriteMutation {
        path: path(&roots, "note.md")?,
        content: "replacement".to_owned(),
        expected_hash: None,
        force: true,
        origin: Origin::Tool("fs_write_file".to_owned()),
    });

    Err(std::io::Error::other(format!("kill-test write unexpectedly returned: {result:?}")).into())
}

#[test]
fn nfr_04_existing_file_requires_hash_or_explicit_force() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "original")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;
    let note = path(&roots, "note.md")?;

    let guarded = service.write_file(&WriteMutation {
        path: note.clone(),
        content: "blocked".to_owned(),
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    });
    let forced = service.write_file(&WriteMutation {
        path: note,
        content: "forced".to_owned(),
        expected_hash: None,
        force: true,
        origin: Origin::Tool("fs_write_file".to_owned()),
    })?;

    assert!(matches!(guarded, Err(FsError::Conflict { .. })));
    assert!(!forced.value.created);
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "forced"
    );
    Ok(())
}

#[test]
fn fr_04_edit_dry_run_returns_unified_diff_without_writing()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "old line\nkeep\n")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;

    let result = service.edit_file(&EditFileRequest {
        path: path(&roots, "note.md")?,
        edits: vec![TextEdit {
            old_text: "old line".to_owned(),
            new_text: "new line".to_owned(),
        }],
        dry_run: true,
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_edit_file".to_owned()),
    })?;

    assert!(!result.applied);
    assert!(result.diff.contains("--- original"));
    assert!(result.diff.contains("+++ modified"));
    assert!(result.diff.contains("-old line"));
    assert!(result.diff.contains("+new line"));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "old line\nkeep\n"
    );
    Ok(())
}

#[test]
fn fr_04_rejects_missing_or_ambiguous_exact_edits_without_writing()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "same same")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;
    let note = path(&roots, "note.md")?;

    let missing = service.edit_file(&EditFileRequest {
        path: note.clone(),
        edits: vec![TextEdit {
            old_text: "absent".to_owned(),
            new_text: "new".to_owned(),
        }],
        dry_run: false,
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_edit_file".to_owned()),
    });
    let ambiguous = service.edit_file(&EditFileRequest {
        path: note,
        edits: vec![TextEdit {
            old_text: "same".to_owned(),
            new_text: "new".to_owned(),
        }],
        dry_run: false,
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_edit_file".to_owned()),
    });

    assert!(matches!(missing, Err(FsError::EditNotFound { .. })));
    assert!(matches!(ambiguous, Err(FsError::EditAmbiguous { .. })));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "same same"
    );
    Ok(())
}

#[test]
fn fr_05_directory_creation_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;
    let directory = path(&roots, "nested/directory")?;

    let first = service.create_directory(&CreateDirectoryMutation {
        path: directory.clone(),
        origin: Origin::Tool("fs_create_directory".to_owned()),
    })?;
    let second = service.create_directory(&CreateDirectoryMutation {
        path: directory,
        origin: Origin::Tool("fs_create_directory".to_owned()),
    })?;

    assert!(first.value.created);
    assert!(first.event.is_some());
    assert!(!second.value.created);
    assert!(second.event.is_none());
    Ok(())
}

#[test]
fn fr_09_move_fails_when_destination_exists() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("source.md"), "source")?;
    std::fs::write(vault.path().join("destination.md"), "destination")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;

    let result = service.move_file(&MoveMutation {
        source: path(&roots, "source.md")?,
        destination: path(&roots, "destination.md")?,
        origin: Origin::Tool("fs_move_file".to_owned()),
    });

    assert!(matches!(result, Err(FsError::DestinationExists { .. })));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("source.md"))?,
        "source"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("destination.md"))?,
        "destination"
    );
    Ok(())
}

#[test]
fn fr_09_move_creates_parent_and_emits_one_two_path_event() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempdir()?;
    std::fs::write(vault.path().join("source.md"), "source")?;
    let (service, roots) = fixture(vault.path().to_path_buf())?;
    let source = path(&roots, "source.md")?;
    let destination = path(&roots, "archive/source.md")?;

    let result = service.move_file(&MoveMutation {
        source: source.clone(),
        destination: destination.clone(),
        origin: Origin::Tool("fs_move_file".to_owned()),
    })?;

    assert!(!vault.path().join("source.md").exists());
    assert_eq!(
        std::fs::read_to_string(vault.path().join("archive/source.md"))?,
        "source"
    );
    let event = result.event.as_ref().ok_or("move must emit an event")?;
    assert_eq!(event.paths, vec![source, destination]);
    Ok(())
}
