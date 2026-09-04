use std::path::PathBuf;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{
    Filesystem, FilesystemConfig, FsError, FsLimits, LineRange, ReadLimit, ReadManyRequest, ReadTextRequest,
    default_hidden_patterns,
};
use tempfile::tempdir;

fn fixture(
    root: PathBuf,
    max_read_bytes: u64,
    max_batch_files: usize,
) -> Result<(Filesystem, VaultSet), Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: root,
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes,
            max_batch_files,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    Ok((filesystem, roots))
}

fn vault_path<'a>(roots: &'a VaultSet, raw: &'a str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[test]
fn reads_utf8_text_and_returns_total_lines_and_sha256() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "one\ntwo\nthree\n")?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 1024, 50)?;

    let result = filesystem.read_text(&ReadTextRequest {
        path: vault_path(&roots, "note.md")?,
        limit: None,
    })?;

    assert_eq!(result.content, "one\ntwo\nthree\n");
    assert_eq!(result.line_count, 3);
    assert_eq!(
        <&str>::from(&result.content_hash),
        "b6285c57e8797db5d4c51c80d6f11938afda9b11c6a003549709189e9b4b92a2"
    );
    assert!(!result.truncated);
    Ok(())
}

#[test]
fn applies_head_tail_and_inclusive_line_range() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "one\ntwo\nthree\n")?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 1024, 50)?;
    let path = vault_path(&roots, "note.md")?;

    let head = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: Some(ReadLimit::Head(2)),
    })?;
    let tail = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: Some(ReadLimit::Tail(2)),
    })?;
    let range = filesystem.read_text(&ReadTextRequest {
        path,
        limit: Some(ReadLimit::Range(LineRange::try_from((2, 3))?)),
    })?;

    assert_eq!(head.content, "one\ntwo\n");
    assert_eq!(tail.content, "two\nthree\n");
    assert_eq!(range.content, "two\nthree\n");
    assert!(head.truncated);
    assert!(tail.truncated);
    assert!(range.truncated);
    assert_eq!(head.line_count, 3);
    Ok(())
}

#[test]
fn requires_a_limiter_for_large_files() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("large.md"), "one\ntwo\nthree\n")?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 4, 50)?;
    let path = vault_path(&roots, "large.md")?;

    let unlimited = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: None,
    });
    let limited = filesystem.read_text(&ReadTextRequest {
        path,
        limit: Some(ReadLimit::Head(1)),
    })?;

    assert!(matches!(unlimited, Err(FsError::TooLarge { .. })));
    assert_eq!(limited.content, "one\n");
    assert!(limited.truncated);
    Ok(())
}

#[test]
fn applies_the_limit_of_the_selected_vault_root() -> Result<(), Box<dyn std::error::Error>> {
    let restrictive_vault = tempdir()?;
    let permissive_vault = tempdir()?;
    let restrictive_file = restrictive_vault.path().join("note.md");
    let permissive_file = permissive_vault.path().join("note.md");
    std::fs::write(&restrictive_file, "12345678")?;
    std::fs::write(&permissive_file, "12345678")?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: restrictive_vault.path().to_path_buf(),
            managed: true,
            name: Some("restrictive".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: permissive_vault.path().to_path_buf(),
            managed: true,
            name: Some("permissive".to_owned()),
        })?,
    ])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![
            FsLimits {
                max_read_bytes: 4,
                max_batch_files: 50,
            },
            FsLimits {
                max_read_bytes: 16,
                max_batch_files: 50,
            },
        ],
        hidden: vec![default_hidden_patterns(), default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let restrictive_path = restrictive_file.to_string_lossy();
    let permissive_path = permissive_file.to_string_lossy();

    let restrictive_result = filesystem.read_text(&ReadTextRequest {
        path: vault_path(&roots, &restrictive_path)?,
        limit: None,
    });
    let permissive_result = filesystem.read_text(&ReadTextRequest {
        path: vault_path(&roots, &permissive_path)?,
        limit: None,
    })?;

    assert!(matches!(restrictive_result, Err(FsError::TooLarge { maximum: 4, .. })));
    assert_eq!(permissive_result.content, "12345678");
    Ok(())
}

#[test]
fn applies_batch_limits_per_selected_vault_root() -> Result<(), Box<dyn std::error::Error>> {
    let restrictive_vault = tempdir()?;
    let permissive_vault = tempdir()?;
    let first_file = permissive_vault.path().join("first.md");
    let second_file = permissive_vault.path().join("second.md");
    std::fs::write(&first_file, "first")?;
    std::fs::write(&second_file, "second")?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(VaultRootInput {
            path: restrictive_vault.path().to_path_buf(),
            managed: true,
            name: Some("restrictive".to_owned()),
        })?,
        VaultRoot::try_from(VaultRootInput {
            path: permissive_vault.path().to_path_buf(),
            managed: true,
            name: Some("permissive".to_owned()),
        })?,
    ])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![
            FsLimits {
                max_read_bytes: 1024,
                max_batch_files: 1,
            },
            FsLimits {
                max_read_bytes: 1024,
                max_batch_files: 2,
            },
        ],
        hidden: vec![default_hidden_patterns(), default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let first_path = first_file.to_string_lossy();
    let second_path = second_file.to_string_lossy();

    assert_eq!(filesystem.batch_capacity(), 3);
    let results = filesystem.read_many(ReadManyRequest {
        paths: vec![vault_path(&roots, &first_path)?, vault_path(&roots, &second_path)?],
    })?;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.error.is_none()));
    Ok(())
}

#[test]
fn rejects_binary_content() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("binary.dat"), [0_u8, 159, 146, 150])?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 1024, 50)?;

    let result = filesystem.read_text(&ReadTextRequest {
        path: vault_path(&roots, "binary.dat")?,
        limit: None,
    });

    assert!(matches!(result, Err(FsError::Binary { .. })));
    assert_eq!(result.map_err(|error| error.code()), Err("io/binary"));
    Ok(())
}

#[test]
fn batch_isolates_per_file_failures() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("present.md"), "available")?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 1024, 50)?;

    let result = filesystem.read_many(ReadManyRequest {
        paths: vec![vault_path(&roots, "present.md")?, vault_path(&roots, "missing.md")?],
    })?;

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].content.as_deref(), Some("available"));
    assert!(result[0].error.is_none());
    assert!(result[1].content.is_none());
    assert_eq!(result[1].error.as_ref().map(|error| error.code), Some("path/not-found"));
    Ok(())
}

#[test]
fn rejects_batch_over_configured_limit() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let (filesystem, roots) = fixture(vault.path().to_path_buf(), 1024, 1)?;

    let result = filesystem.read_many(ReadManyRequest {
        paths: vec![vault_path(&roots, "one.md")?, vault_path(&roots, "two.md")?],
    });

    assert!(matches!(result, Err(FsError::BatchTooLarge { .. })));
    Ok(())
}

#[test]
fn rejects_a_validated_path_from_another_vault_set() -> Result<(), Box<dyn std::error::Error>> {
    let configured = tempdir()?;
    let outside = tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "private")?;
    let (filesystem, _) = fixture(configured.path().to_path_buf(), 1024, 50)?;
    let outside_roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: outside.path().to_path_buf(),
        managed: true,
        name: Some("outside".to_owned()),
    })?])?;

    let result = filesystem.read_text(&ReadTextRequest {
        path: vault_path(&outside_roots, "secret.md")?,
        limit: None,
    });

    assert!(matches!(result, Err(FsError::OutsideRoot { .. })));
    Ok(())
}
