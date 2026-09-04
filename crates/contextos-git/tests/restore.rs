use contextos_core::{
    MoveMutation, NoSearchUpdates, OpKind, OperationRouter, OperationRouterConfig, Origin, VaultRoot, VaultRootInput,
    VaultSet,
};
use contextos_fs::{FilesystemService, FilesystemServiceConfig, RoutedFilesystemServiceConfig};
use contextos_git::{Git2Vault, Git2VaultConfig, GitRestoreRequest};
use contextos_index::{IndexService, IndexServiceConfig};
use contextos_oplog::{OperationLog, OperationLogConfig};
use tempfile::tempdir;

mod support;

#[test]
fn bad_overwrite_is_restored_through_an_ordinary_restore_mutation() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "baseline\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let writer = support::writer(&root)?;
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?;
    let baseline = git.initialise(&writer)?.commit_id.ok_or("initial commit missing")?;
    std::fs::write(vault.path().join("note.md"), "damaged\n")?;

    let result = git.restore(
        &GitRestoreRequest {
            path: support::vault_path(&root, "note.md")?,
            reference: baseline,
            dry_run: false,
        },
        &writer,
    )?;

    assert_eq!(std::fs::read_to_string(vault.path().join("note.md"))?, "baseline\n");
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].kind, OpKind::Restore);
    assert!(result.diff.contains("-damaged"));
    assert!(result.diff.contains("+baseline"));
    Ok(())
}

#[test]
fn wrong_delete_is_restored_as_a_new_file_without_rewriting_history() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("deleted.md"), "recover me\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let writer = support::writer(&root)?;
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?;
    let baseline = git.initialise(&writer)?.commit_id.ok_or("initial commit missing")?;
    std::fs::remove_file(vault.path().join("deleted.md"))?;

    let result = git.restore(
        &GitRestoreRequest {
            path: support::vault_path(&root, "deleted.md")?,
            reference: baseline.clone(),
            dry_run: false,
        },
        &writer,
    )?;
    let repository = git2::Repository::open(vault.path())?;

    assert_eq!(
        std::fs::read_to_string(vault.path().join("deleted.md"))?,
        "recover me\n"
    );
    assert_eq!(result.events[0].kind, OpKind::Restore);
    assert_eq!(repository.head()?.peel_to_commit()?.id().to_string(), baseline);
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end recovery drill keeps Git, index, oplog, and operator-file assertions together"
)]
fn phase_2_bulk_move_recovery_restores_owned_paths_and_preserves_untracked_operator_file()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("projects"))?;
    std::fs::create_dir_all(vault.path().join("memory/log/2026/07"))?;
    let log_path = vault.path().join("memory/log/2026/07/2026-07-18.md");
    let legacy_log = concat!(
        "# 2026-07-18: Operation Log\n\n",
        "17:00:00 | move | Legacy entry | files: legacy.md\n",
    );
    std::fs::write(&log_path, legacy_log)?;
    std::fs::write(vault.path().join("projects/a.md"), "A\n")?;
    std::fs::write(vault.path().join("projects/b.md"), "B\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let filesystem = support::filesystem(&root)?;
    let plain_writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: support::FixedClock,
    });
    let indexes = IndexService::try_from(IndexServiceConfig {
        root: root.clone(),
        roots: VaultSet::try_from(vec![root.clone()])?,
        reader: filesystem.clone(),
        writer: plain_writer.clone(),
        excluded: vec![".git".to_owned(), ".contextos".to_owned()],
    })?;
    let operation_log = OperationLog::try_from(OperationLogConfig {
        root: root.clone(),
        roots: VaultSet::try_from(vec![root.clone()])?,
        relative_directory: "memory/log".to_owned(),
        appender: plain_writer.clone(),
    })?;
    indexes.rebuild(
        &support::vault_path(&root, ".")?,
        Origin::Tool("vault_index_rebuild".to_owned()),
    )?;
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: true,
        protected_restore_paths: vec!["memory/log".into()],
    })?;
    let baseline = git
        .initialise(&plain_writer)?
        .commit_id
        .ok_or("initial commit missing")?;
    let routed_writer = FilesystemService::from(RoutedFilesystemServiceConfig {
        filesystem,
        clock: support::FixedClock,
        services: OperationRouter::from(OperationRouterConfig {
            indexes,
            operation_log,
            versions: git.clone(),
            search: NoSearchUpdates,
        }),
    });
    routed_writer.move_file(&MoveMutation {
        source: support::vault_path(&root, "projects")?,
        destination: support::vault_path(&root, "misplaced")?,
        origin: Origin::Tool("fs_move_file".to_owned()),
    })?;
    std::fs::write(vault.path().join("operator-untracked.md"), "keep me\n")?;

    let result = git.restore(
        &GitRestoreRequest {
            path: support::vault_path(&root, ".")?,
            reference: baseline,
            dry_run: false,
        },
        &routed_writer,
    )?;
    let recovery = git.commit(Some("mcp: recover bulk move"))?;

    assert_eq!(std::fs::read_to_string(vault.path().join("projects/a.md"))?, "A\n");
    assert_eq!(std::fs::read_to_string(vault.path().join("projects/b.md"))?, "B\n");
    assert!(!vault.path().join("misplaced").exists());
    assert_eq!(
        std::fs::read_to_string(vault.path().join("operator-untracked.md"))?,
        "keep me\n"
    );
    assert!(result.events.iter().any(|event| event.kind == OpKind::Restore));
    assert!(result.events.iter().any(|event| event.kind == OpKind::Delete));
    assert!(recovery.commit_id.is_some());
    let root_index = std::fs::read_to_string(vault.path().join("index.md"))?;
    let project_index = std::fs::read_to_string(vault.path().join("projects/index.md"))?;
    assert!(root_index.contains("| [projects/](projects/index.md)"));
    assert!(!root_index.contains("misplaced"));
    assert!(project_index.contains("| [a.md](a.md)"));
    assert!(project_index.contains("| [b.md](b.md)"));
    let operation_log = std::fs::read_to_string(log_path)?;
    assert!(operation_log.starts_with(legacy_log));
    assert!(operation_log.contains("| fs_move_file | move |"));
    assert!(operation_log.contains("| git_restore | restore |"));
    assert!(operation_log.contains("| git_restore | delete |"));
    let Err(error) = git.restore(
        &GitRestoreRequest {
            path: support::vault_path(&root, "memory/log")?,
            reference: "HEAD".to_owned(),
            dry_run: true,
        },
        &routed_writer,
    ) else {
        return Err(std::io::Error::other("append-only operation log accepted a restore").into());
    };
    assert_eq!(error.code(), "git/restore");
    Ok(())
}
