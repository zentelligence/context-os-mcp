use std::path::PathBuf;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{
    DirectoryTreeRequest, EntryKind, FileInfoRequest, Filesystem, FilesystemConfig, FsError, FsLimits,
    ListDirectoryRequest, ListDirectoryWithSizesRequest, ReadTextRequest, SearchFilesRequest, SortBy,
    default_hidden_patterns,
};
use tempfile::tempdir;

fn filesystem(root: PathBuf, managed: bool) -> Result<(Filesystem, VaultSet), Box<dyn std::error::Error>> {
    filesystem_with_hidden(root, managed, default_hidden_patterns())
}

fn filesystem_with_hidden(
    root: PathBuf,
    managed: bool,
    hidden: Vec<String>,
) -> Result<(Filesystem, VaultSet), Box<dyn std::error::Error>> {
    let vault_root = VaultRoot::try_from(VaultRootInput {
        path: root,
        managed,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![vault_root])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![hidden],
        atomic_write_guard: None,
    })?;
    Ok((filesystem, roots))
}

fn path(roots: &VaultSet, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[test]
fn lists_entries_with_kind_markers() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("readme.md"), "read me")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let result = filesystem.list_directory(&ListDirectoryRequest {
        path: path(&roots, ".")?,
    })?;

    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].name, "notes");
    assert_eq!(result.entries[0].kind, EntryKind::Directory);
    assert_eq!(result.entries[1].name, "readme.md");
    assert_eq!(result.rendered, "[DIR] notes\n[FILE] readme.md");
    Ok(())
}

#[test]
fn lists_sizes_and_sorts_by_size() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("large.md"), "123456789")?;
    std::fs::write(vault.path().join("small.md"), "1")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let result = filesystem.list_directory_with_sizes(&ListDirectoryWithSizesRequest {
        path: path(&roots, ".")?,
        sort_by: SortBy::Size,
    })?;

    assert_eq!(result.entries[0].name, "small.md");
    assert_eq!(result.entries[0].size, Some(1));
    assert_eq!(result.entries[1].name, "large.md");
    assert_eq!(result.entries[1].size, Some(9));
    assert!(result.rendered.contains("[FILE] small.md (1 B)"));
    Ok(())
}

#[test]
fn builds_bounded_tree_and_honours_excludes() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes/deep"))?;
    std::fs::create_dir_all(vault.path().join("private/nested"))?;
    std::fs::create_dir(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join("notes/deep/hidden-by-depth.md"), "deep")?;
    std::fs::write(vault.path().join("notes/visible.md"), "visible")?;
    std::fs::write(vault.path().join("private/nested/secret.md"), "secret")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let tree = filesystem.directory_tree(&DirectoryTreeRequest {
        path: path(&roots, ".")?,
        exclude_patterns: vec!["private/**".to_owned()],
        max_depth: 2,
    })?;

    let serialised = serde_json::to_value(&tree)?;
    assert_eq!(serialised["type"], "dir");
    assert!(serialised.to_string().contains("visible.md"));
    assert!(!serialised.to_string().contains("hidden-by-depth.md"));
    assert!(!serialised.to_string().contains("private"));
    assert!(!serialised.to_string().contains(".contextos"));
    Ok(())
}

#[test]
fn searches_case_insensitive_globs_with_excludes_and_limit() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes/private"))?;
    std::fs::write(vault.path().join("notes/Alpha.MD"), "alpha")?;
    std::fs::write(vault.path().join("notes/beta.md"), "beta")?;
    std::fs::write(vault.path().join("notes/private/secret.md"), "secret")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let results = filesystem.search_files(&SearchFilesRequest {
        path: path(&roots, ".")?,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: vec!["**/private/**".to_owned()],
        max_results: 1,
    })?;

    assert_eq!(results, vec!["notes/Alpha.MD"]);
    Ok(())
}

#[test]
fn reports_metadata_permissions_and_bounded_hash() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let note = vault.path().join("note.md");
    std::fs::write(&note, "hello")?;
    let mut permissions = note.metadata()?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&note, permissions)?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let info = filesystem.file_info(&FileInfoRequest {
        path: path(&roots, "note.md")?,
    })?;

    assert_eq!(info.path, "note.md");
    assert_eq!(info.kind, EntryKind::File);
    assert_eq!(info.size, 5);
    assert!(info.readonly);
    assert_eq!(
        info.content_hash.as_ref().map(<&str>::from),
        Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
    );
    assert!(info.modified.is_some());
    Ok(())
}

#[test]
fn lists_resolved_roots_and_managed_flags() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let (filesystem, _) = filesystem(vault.path().to_path_buf(), false)?;

    let roots = filesystem.list_allowed_directories();

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, dunce::canonicalize(vault.path())?.to_string_lossy());
    assert!(!roots[0].managed);
    Ok(())
}

#[test]
fn list_directory_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let result = filesystem.list_directory(&ListDirectoryRequest {
        path: path(&roots, ".")?,
    })?;

    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].name, "visible.md");
    Ok(())
}

#[test]
fn list_directory_with_sizes_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("secret.md"), "secret")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret.md".to_owned()])?;

    let result = filesystem.list_directory_with_sizes(&ListDirectoryWithSizesRequest {
        path: path(&roots, ".")?,
        sort_by: SortBy::Name,
    })?;

    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].name, "visible.md");
    Ok(())
}

#[test]
fn directory_tree_omits_a_subtree_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("secret/nested"))?;
    std::fs::write(vault.path().join("secret/nested/private.md"), "private")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let tree = filesystem.directory_tree(&DirectoryTreeRequest {
        path: path(&roots, ".")?,
        exclude_patterns: vec![],
        max_depth: 3,
    })?;

    let serialised = serde_json::to_value(&tree)?.to_string();
    assert!(serialised.contains("visible.md"));
    assert!(!serialised.contains("secret"));
    assert!(!serialised.contains("private.md"));
    Ok(())
}

#[test]
fn search_files_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("secret/note.md"), "secret")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let results = filesystem.search_files(&SearchFilesRequest {
        path: path(&roots, ".")?,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: vec![],
        max_results: usize::MAX,
    })?;

    assert_eq!(results, vec!["visible.md"]);
    Ok(())
}

#[test]
fn direct_read_of_a_hidden_path_is_unaffected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("secret/note.md"), "still readable")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let result = filesystem.read_text(&ReadTextRequest {
        path: path(&roots, "secret/note.md")?,
        limit: None,
    })?;

    assert_eq!(result.content, "still readable");
    Ok(())
}

#[test]
fn hidden_count_mismatch_is_rejected_at_construction() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let vault_root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![vault_root])?;

    let result = Filesystem::try_from(FilesystemConfig {
        roots,
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![],
        atomic_write_guard: None,
    });

    assert!(matches!(result, Err(FsError::HiddenCountMismatch { .. })));
    Ok(())
}
