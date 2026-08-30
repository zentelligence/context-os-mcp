use contextos_core::{OpKind, OperationEvent, Origin, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig};
use git2::{Repository, Status};
use tempfile::tempdir;
use time::macros::datetime;

mod support;

#[test]
fn fr_30_commit_contains_only_mcp_owned_paths_and_leaves_operator_staging_intact()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("managed.md"), "baseline\n")?;
    std::fs::write(vault.path().join("operator.md"), "baseline\n")?;
    let root = contextos_core::VaultRoot::try_from(contextos_core::VaultRootInput {
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

    std::fs::write(vault.path().join("operator.md"), "operator staged\n")?;
    let repository = Repository::open(vault.path())?;
    let mut index = repository.index()?;
    index.add_path(std::path::Path::new("operator.md"))?;
    index.write()?;

    std::fs::write(vault.path().join("managed.md"), "MCP staged\n")?;
    git.stage(&OperationEvent {
        kind: OpKind::Modify,
        paths: vec![support::vault_path(&root, "managed.md")?],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "Overwrote managed.md (11 bytes)".to_owned(),
        at: datetime!(2026-07-18 18:30:00 +10:00),
    })
    .map_err(|warning| warning.message)?;

    let result = git.commit(None)?;
    let head = repository.head()?.peel_to_commit()?;
    let tree = head.tree()?;
    let managed = repository.find_blob(tree.get_path(std::path::Path::new("managed.md"))?.id())?;
    let operator =
        repository.find_blob(tree.get_path(std::path::Path::new("operator.md"))?.id())?;

    assert_eq!(
        result.commit_id.as_deref(),
        Some(head.id().to_string().as_str())
    );
    assert_eq!(managed.content(), b"MCP staged\n");
    assert_eq!(operator.content(), b"baseline\n");
    assert!(
        !repository
            .status_file(std::path::Path::new("managed.md"))?
            .contains(Status::INDEX_MODIFIED)
    );
    assert!(
        repository
            .status_file(std::path::Path::new("operator.md"))?
            .contains(Status::INDEX_MODIFIED)
    );
    Ok(())
}
