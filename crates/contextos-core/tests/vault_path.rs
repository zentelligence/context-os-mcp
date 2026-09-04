use std::path::{Path, PathBuf};

use contextos_core::{PathError, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use tempfile::tempdir;

fn sole_root(path: PathBuf) -> Result<VaultSet, PathError> {
    // `tempdir()` names its directory something like `.tmpjFXxK1` on
    // Windows, whose leading `.` is not a valid URI scheme token, so tests
    // that do not care about the vault name give it an explicit, valid one
    // rather than relying on the temp directory's own basename.
    VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path,
        managed: true,
        name: Some("vault".to_owned()),
    })?])
}

fn two_named_roots(first: (PathBuf, &str), second: (PathBuf, &str)) -> Result<VaultSet, PathError> {
    VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: first.0,
            managed: true,
            name: Some(first.1.to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: second.0,
            managed: true,
            name: Some(second.1.to_owned()),
        })?,
    ])
}

#[test]
fn accepts_relative_path_beneath_sole_root() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    let note = notes.join("welcome.md");
    std::fs::write(&note, "hello")?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes/welcome.md",
    })?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(&note)?);
    assert_eq!(path.relative(), Path::new("notes/welcome.md"));
    Ok(())
}

#[test]
fn accepts_absolute_path_beneath_a_root() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let note = vault.path().join("welcome.md");
    std::fs::write(&note, "hello")?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let raw = note.to_string_lossy();

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: &raw,
    })?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(&note)?);
    Ok(())
}

#[test]
fn allows_a_missing_leaf_for_create_operations() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "new/child/note.md",
    })?;
    let resolved: &Path = (&path).into();

    // The production path (`resolve_with_missing_suffix`) resolves the
    // existing ancestor via `dunce::canonicalize`, then pushes each missing
    // component individually; mirror that here rather than joining a single
    // forward-slash string onto the vault's raw (unresolved) path, which
    // would spuriously mismatch whenever the tempdir's raw path differs
    // from its resolved form (for example under Windows 8.3 short-name
    // generation).
    let expected = dunce::canonicalize(vault.path())?
        .join("new")
        .join("child")
        .join("note.md");
    assert_eq!(resolved, expected);
    Ok(())
}

#[test]
fn rejects_parent_traversal_before_filesystem_access() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes/../../outside.md",
    });

    assert!(matches!(result, Err(PathError::Traversal { .. })));
    assert_eq!(result.map_err(|error| error.code()), Err("path/outside-root"));
    Ok(())
}

#[test]
fn rejects_windows_backslash_traversal_on_every_platform() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: r"notes\..\..\outside.md",
    });

    assert!(matches!(result, Err(PathError::Traversal { .. })));
    Ok(())
}

#[test]
fn rejects_windows_verbatim_prefixes_on_every_platform() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: r"\\?\C:\vault\note.md",
    });

    assert!(matches!(result, Err(PathError::WindowsVerbatim { .. })));
    Ok(())
}

#[test]
fn rejects_windows_alternate_data_streams_on_every_platform() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md:secret",
    });

    assert!(matches!(result, Err(PathError::AlternateDataStream { .. })));
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn rejects_windows_drive_paths_on_non_windows_hosts() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: r"C:\vault\note.md",
    });

    assert!(matches!(result, Err(PathError::UnsupportedWindowsPath { .. })));
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn rejects_windows_unc_paths_on_non_windows_hosts() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: r"\\server\share\note.md",
    });

    assert!(matches!(result, Err(PathError::UnsupportedWindowsPath { .. })));
    Ok(())
}

#[test]
fn rejects_absolute_path_outside_every_root() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let outside = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let raw = outside.path().join("secret.md").to_string_lossy().into_owned();

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: &raw,
    });

    assert!(matches!(result, Err(PathError::OutsideRoot { .. })));
    Ok(())
}

#[test]
fn requires_absolute_paths_when_more_than_one_root_is_configured() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: first.path().to_path_buf(),
            managed: true,
            name: Some("first".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: second.path().to_path_buf(),
            managed: true,
            name: Some("second".to_owned()),
        })?,
    ])?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "ambiguous.md",
    });

    assert!(matches!(result, Err(PathError::AmbiguousRoot { .. })));
    Ok(())
}

#[test]
fn a_named_prefix_resolves_against_that_vault_without_an_absolute_path() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(second.path().join("welcome.md"), "hello")?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "second://welcome.md",
    })?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(second.path().join("welcome.md"))?);
    assert_eq!(path.relative(), Path::new("welcome.md"));
    Ok(())
}

#[test]
fn root_returns_the_configured_root_by_id() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "second://.",
    })?;
    let root = roots.root(path.root_id()).ok_or("root should resolve")?;

    assert_eq!(root.name(), "second");
    Ok(())
}

#[test]
fn a_bare_vault_name_selects_that_vaults_root() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from_vault_selector(&roots, "second")?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(second.path())?);
    assert_eq!(path.relative(), Path::new(""));
    Ok(())
}

#[test]
fn a_bare_vault_name_selector_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from_vault_selector(&roots, "SECOND")?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(second.path())?);
    Ok(())
}

#[test]
fn a_non_matching_selector_falls_back_to_ordinary_path_resolution() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(notes.join("welcome.md"), "hello")?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let path = VaultPath::try_from_vault_selector(&roots, "notes/welcome.md")?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(notes.join("welcome.md"))?);
    Ok(())
}

#[test]
fn the_named_prefix_form_still_works_through_the_vault_selector() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(second.path().join("welcome.md"), "hello")?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from_vault_selector(&roots, "second://welcome.md")?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(second.path().join("welcome.md"))?);
    Ok(())
}

#[test]
fn a_named_prefix_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(second.path().join("welcome.md"), "hello")?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "SECOND://welcome.md",
    })?;
    let resolved: &Path = (&path).into();

    assert_eq!(resolved, dunce::canonicalize(second.path().join("welcome.md"))?);
    Ok(())
}

#[test]
fn an_unknown_vault_name_prefix_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "nobody://welcome.md",
    });

    assert!(matches!(result, Err(PathError::UnknownVaultName { .. })));
    assert_eq!(result.map_err(|error| error.code()), Err("path/unknown-vault-name"));
    Ok(())
}

#[test]
fn a_named_prefix_still_rejects_traversal_in_the_remainder() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "second://../../outside.md",
    });

    assert!(matches!(result, Err(PathError::Traversal { .. })));
    Ok(())
}

#[test]
fn a_named_prefix_is_not_misclassified_as_an_alternate_data_stream() -> Result<(), Box<dyn std::error::Error>> {
    // Regression for the specific risk the phase 9 change brief calls out:
    // `validate_windows_input` rejects any raw string containing `:` outside
    // a drive-letter position as `AlternateDataStream`, so a `name://` prefix
    // must be detected and stripped before that check ever sees the raw
    // string, not after.
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(second.path().join("welcome.md"), "hello")?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "second://welcome.md",
    });

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn a_named_prefix_with_no_remainder_is_a_distinct_actionable_error() -> Result<(), Box<dyn std::error::Error>> {
    // Regression: an ordinary `path`-parameter chokepoint call (not the
    // vault-selector entry point) with a bare `{name}://` and no
    // remainder used to fall through to `PathError::Invalid`'s generic
    // "path is empty or contains a null byte" message, which reports the
    // already-stripped empty remainder rather than the caller's actual
    // `{name}://` input and gives no hint that `{name}://.` selects the
    // vault root.
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "second://",
    });

    assert!(matches!(
        result,
        Err(PathError::EmptyNamedPrefixRemainder { ref name }) if name == "second"
    ));
    assert_eq!(result.map_err(|error| error.code()), Err("path/empty-named-prefix"));
    Ok(())
}

#[test]
fn an_absolute_remainder_must_still_fall_within_the_named_root() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(first.path().join("secret.md"), "private")?;
    let roots = two_named_roots(
        (first.path().to_path_buf(), "first"),
        (second.path().to_path_buf(), "second"),
    )?;
    let raw = format!("second://{}", first.path().join("secret.md").to_string_lossy());

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: &raw,
    });

    assert!(matches!(result, Err(PathError::OutsideRoot { .. })));
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_file_symlink_escape() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let vault = tempdir()?;
    let outside = tempdir()?;
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "private")?;
    symlink(&secret, vault.path().join("escape.md"))?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "escape.md",
    });

    assert!(matches!(result, Err(PathError::SymlinkEscape { .. })));
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_nested_directory_symlink_escape() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let vault = tempdir()?;
    let outside = tempdir()?;
    symlink(outside.path(), vault.path().join("linked"))?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "linked/new/note.md",
    });

    assert!(matches!(result, Err(PathError::SymlinkEscape { .. })));
    Ok(())
}

#[cfg(windows)]
#[test]
fn rejects_file_symlink_escape_on_windows() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::windows::fs::symlink_file;

    let vault = tempdir()?;
    let outside = tempdir()?;
    let secret = outside.path().join("secret.md");
    std::fs::write(&secret, "private")?;
    symlink_file(&secret, vault.path().join("escape.md"))?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "escape.md",
    });

    assert!(matches!(result, Err(PathError::SymlinkEscape { .. })));
    Ok(())
}

#[test]
fn vault_name_defaults_to_the_root_directorys_basename() -> Result<(), Box<dyn std::error::Error>> {
    // A bare `tempdir()` is unsuitable here: `tempfile` names it something
    // like `.tmpjFXxK1` on Windows, whose leading `.` is not a valid URI
    // scheme token, so the vault root itself needs a deliberately valid
    // name rather than the temp directory's own random one.
    let parent = tempdir()?;
    let vault = parent.path().join("myvault");
    std::fs::create_dir(&vault)?;
    let root = VaultRoot::try_from(vault.clone())?;

    assert_eq!(root.name(), "myvault");
    Ok(())
}

#[test]
fn an_explicit_vault_name_overrides_the_default() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("mine".to_owned()),
    })?;

    assert_eq!(root.name(), "mine");
    Ok(())
}

#[test]
fn an_invalid_explicit_vault_name_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;

    let result = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("my_vault".to_owned()),
    });

    assert!(matches!(result, Err(PathError::InvalidName { .. })));
    assert_eq!(result.map_err(|error| error.code()), Err("path/invalid-vault-name"));
    Ok(())
}

#[test]
fn an_invalid_default_derived_name_is_rejected_not_sanitised() -> Result<(), Box<dyn std::error::Error>> {
    // `tempdir()`'s own basename starts with `.` on Windows, which is not a
    // valid URI scheme token; this pins the "no silent sanitisation"
    // behaviour the default derivation deliberately does not have.
    let vault = tempdir()?;

    let result = VaultRoot::try_from(vault.path().to_path_buf());

    assert!(matches!(result, Err(PathError::InvalidName { .. })));
    Ok(())
}

#[test]
fn duplicate_vault_names_are_rejected_at_startup() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;

    let result = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: first.path().to_path_buf(),
            managed: true,
            name: Some("mine".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: second.path().to_path_buf(),
            managed: true,
            name: Some("Mine".to_owned()),
        })?,
    ]);

    assert!(matches!(result, Err(PathError::DuplicateName { .. })));
    assert_eq!(result.map_err(|error| error.code()), Err("path/duplicate-vault-name"));
    Ok(())
}

#[test]
fn root_by_name_finds_the_configured_root_case_insensitively() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: first.path().to_path_buf(),
            managed: true,
            name: Some("mine".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: second.path().to_path_buf(),
            managed: true,
            name: Some("family".to_owned()),
        })?,
    ])?;

    let (id, root) = roots.root_by_name("FAMILY").ok_or("family vault is configured")?;

    assert_eq!(root.name(), "family");
    assert_eq!(roots.root_by_name("family").map(|(found_id, _)| found_id), Some(id));
    assert!(roots.root_by_name("nobody").is_none());
    Ok(())
}

#[cfg(windows)]
#[test]
fn rejects_nested_directory_symlink_escape_on_windows() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::windows::fs::symlink_dir;

    let vault = tempdir()?;
    let outside = tempdir()?;
    symlink_dir(outside.path(), vault.path().join("linked"))?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let result = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "linked/new/note.md",
    });

    assert!(matches!(result, Err(PathError::SymlinkEscape { .. })));
    Ok(())
}
