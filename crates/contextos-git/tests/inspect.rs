use contextos_core::{OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig, GitDiffRequest, GitError, GitLogRequest};
use tempfile::tempdir;
use time::macros::datetime;

mod support;

#[test]
fn to_fr_32_status_log_and_diff_report_owned_and_worktree_state() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
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
    git.initialise(&writer)?;
    std::fs::write(vault.path().join("note.md"), "changed\n")?;
    let note = support::vault_path(&root, "note.md")?;
    git.stage(&OperationEvent {
        kind: OpKind::Modify,
        paths: vec![note],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "Changed note.md".to_owned(),
        at: datetime!(2026-07-18 18:30:00 +10:00),
    })
    .map_err(|warning| warning.message)?;

    let status = git.status()?;
    assert_eq!(status.branch, "main");
    assert_eq!(status.pending_paths, vec!["note.md"]);
    assert_eq!(git.pending_commit_count()?, 1);
    assert_eq!(status.staged, vec!["note.md"]);
    let diff = git.diff(&GitDiffRequest {
        from: None,
        to: None,
        path: Some("note.md".into()),
        max_bytes: 1024,
    })?;
    assert!(diff.content.contains("-initial"));
    assert!(diff.content.contains("+changed"));
    let capped = git.diff(&GitDiffRequest {
        from: None,
        to: None,
        path: Some("note.md".into()),
        max_bytes: 64,
    })?;
    assert!(capped.truncated);
    assert!(capped.content.ends_with("\n[diff truncated]\n"));
    assert!(capped.content.len() <= 64);
    git.commit(None)?;
    let log = git.log(&GitLogRequest {
        path: Some("note.md".into()),
        limit: 10,
    })?;
    assert_eq!(log.len(), 2);
    assert!(log[0].files_changed.contains(&"note.md".to_owned()));
    Ok(())
}

#[test]
fn startup_recovers_staged_state_as_a_new_commit() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
    std::fs::write(vault.path().join("operator.md"), "baseline\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let writer = support::writer(&root)?;
    let config = Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    };
    let first = Git2Vault::try_from(config.clone())?;
    first.initialise(&writer)?;
    std::fs::write(vault.path().join("note.md"), "staged\n")?;
    first
        .stage(&OperationEvent {
            kind: OpKind::Modify,
            paths: vec![support::vault_path(&root, "note.md")?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Interrupted write".to_owned(),
            at: datetime!(2026-07-18 18:30:00 +10:00),
        })
        .map_err(|warning| warning.message)?;
    std::fs::write(vault.path().join("operator.md"), "operator staged\n")?;
    let repository = git2::Repository::open(vault.path())?;
    let mut index = repository.index()?;
    index.add_path(std::path::Path::new("operator.md"))?;
    index.write()?;
    drop(first);

    let restarted = Git2Vault::try_from(config)?;
    let recovered = restarted.recover_staged()?;

    assert_eq!(recovered.message.as_deref(), Some("mcp: recovered staged changes"));
    assert_eq!(recovered.committed_paths, vec![std::path::PathBuf::from("note.md")]);
    let head = repository.head()?.peel_to_commit()?;
    let tree = head.tree()?;
    let operator = repository.find_blob(tree.get_path(std::path::Path::new("operator.md"))?.id())?;
    assert_eq!(operator.content(), b"baseline\n");
    assert!(
        repository
            .status_file(std::path::Path::new("operator.md"))?
            .contains(git2::Status::INDEX_MODIFIED)
    );
    Ok(())
}

#[test]
fn startup_rejects_tampered_pending_ownership_without_staging_operator_content()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("operator.md"), "baseline\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let writer = support::writer(&root)?;
    let config = Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    };
    Git2Vault::try_from(config.clone())?.initialise(&writer)?;
    std::fs::write(vault.path().join("operator.md"), "operator edit\n")?;
    let pending = vault.path().join(".git/contextos-mcp/pending");
    std::fs::create_dir_all(&pending)?;
    std::fs::write(pending.join("forged.pending"), "operator.md")?;

    let Err(error) = Git2Vault::try_from(config)?.recover_staged() else {
        return Err(std::io::Error::other("tampered pending ownership was accepted").into());
    };

    assert!(matches!(error, GitError::InvalidPendingMetadata));
    let repository = git2::Repository::open(vault.path())?;
    let status = repository.status_file(std::path::Path::new("operator.md"))?;
    assert!(status.contains(git2::Status::WT_MODIFIED));
    assert!(!status.contains(git2::Status::INDEX_MODIFIED));
    Ok(())
}
