use contextos_core::{VaultRoot, VaultRootInput};
use contextos_git::{Git2Vault, Git2VaultConfig};
use tempfile::tempdir;

mod support;

#[test]
fn initialises_repository_ignore_policy_and_first_commit() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "tracked\n")?;
    std::fs::write(vault.path().join(".gitignore"), "operator-rule\n")?;
    std::fs::create_dir(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join(".contextos/cache"), "derived")?;
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

    let result = git.initialise(&writer)?;
    let repository = git2::Repository::open(vault.path())?;
    let head_id = repository.head()?.peel_to_commit()?.id().to_string();
    let index = repository.index()?;
    let ignore = std::fs::read_to_string(vault.path().join(".gitignore"))?;

    assert!(result.initialised);
    assert_eq!(result.commit_id.as_deref(), Some(head_id.as_str()));
    assert_eq!(ignore, "operator-rule\n.contextos/\n.obsidian/workspace*\n");
    assert!(index.get_path(std::path::Path::new("note.md"), 0).is_some());
    assert!(index.get_path(std::path::Path::new(".contextos/cache"), 0).is_none());
    Ok(())
}

#[test]
fn completes_an_existing_empty_repository_with_initial_history() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    git2::Repository::init(vault.path())?;
    std::fs::write(vault.path().join("note.md"), "tracked\n")?;
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

    let result = git.initialise(&writer)?;

    assert!(result.initialised);
    assert!(result.commit_id.is_some());
    assert!(vault.path().join(".gitignore").exists());
    Ok(())
}
