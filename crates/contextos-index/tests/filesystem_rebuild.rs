use contextos_core::{
    MaintainsIndexes, OpKind, OperationEvent, Origin, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_index::{IndexError, IndexService, IndexServiceConfig, IndexServiceError};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl contextos_core::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-07-18 18:30:00 +10:00)
    }
}

#[test]
fn to_fr_22_rebuilds_a_real_directory_through_the_write_pipeline() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(
        notes.join("index.md"),
        concat!(
            "# Bespoke Notes\n\nOperator introduction.\n\n",
            "<!-- contextos:index:begin -->\n",
            "| Item | Summary |\n| --- | --- |\n",
            "| [keep.md](keep.md) | Hand summary |\n",
            "<!-- contextos:index:end -->\n\nOperator footer.\n",
        ),
    )?;
    std::fs::write(notes.join("keep.md"), "# Keep\n\nExisting note.\n")?;
    std::fs::write(
        notes.join("new-note.md"),
        "---\ntitle: New Note\n---\nA new note exists.\n",
    )?;
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
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes",
    })?;

    let events = service.rebuild(&directory, Origin::Tool("vault_index_rebuild".to_owned()))?;
    let rebuilt = std::fs::read_to_string(notes.join("index.md"))?;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].origin, Origin::Internal("index".to_owned()));
    assert!(rebuilt.starts_with("# Bespoke Notes\n\nOperator introduction.\n\n"));
    assert!(rebuilt.ends_with("\n\nOperator footer.\n"));
    assert!(rebuilt.contains("| [keep.md](keep.md) | Hand summary |"));
    assert!(rebuilt.contains("| [new-note.md](new-note.md) | New Note: A new note exists. <!-- auto --> |"));
    assert!(!rebuilt.contains("| [index.md](index.md)"));
    std::fs::rename(notes.join("index.md"), notes.join("_index.md"))?;
    let preview = service.rebuild_report(&directory, Origin::Tool("vault_index_rebuild".to_owned()), true)?;
    assert_eq!(preview.indexes_updated, 1);
    assert!(notes.join("_index.md").exists());
    assert!(!notes.join("index.md").exists());
    Ok(())
}

#[test]
fn rebuild_renames_legacy_underscore_index_before_reconciliation() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(
        notes.join("_index.md"),
        concat!(
            "# Bespoke Notes\n\nOperator introduction.\n\n",
            "<!-- contextos:index:begin -->\n",
            "| Item | Summary |\n| --- | --- |\n",
            "| [keep.md](keep.md) | Hand summary |\n",
            "<!-- contextos:index:end -->\n\nOperator footer.\n",
        ),
    )?;
    std::fs::write(notes.join("keep.md"), "# Keep\n\nExisting note.\n")?;
    std::fs::write(notes.join("new-note.md"), "# New Note\n")?;
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
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes",
    })?;

    let events = service.rebuild(&directory, Origin::Tool("vault_index_rebuild".to_owned()))?;
    let rebuilt = std::fs::read_to_string(notes.join("index.md"))?;

    assert!(!notes.join("_index.md").exists());
    assert!(rebuilt.starts_with("# Bespoke Notes\n\nOperator introduction.\n\n"));
    assert!(rebuilt.ends_with("\n\nOperator footer.\n"));
    assert!(rebuilt.contains("| [keep.md](keep.md) | Hand summary |"));
    assert!(rebuilt.contains("| [new-note.md](new-note.md) | New Note <!-- auto --> |"));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, OpKind::Move);
    assert_eq!(events[0].paths[0].relative(), std::path::Path::new("notes/_index.md"));
    assert_eq!(events[0].paths[1].relative(), std::path::Path::new("notes/index.md"));
    assert!(
        events
            .iter()
            .all(|event| event.origin == Origin::Internal("index".to_owned()))
    );
    Ok(())
}

#[test]
fn rebuild_rejects_legacy_index_collision_without_changing_either_file() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(notes.join("_index.md"), "legacy bytes\n")?;
    std::fs::write(notes.join("index.md"), "current bytes\n")?;
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
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes",
    })?;

    let Err(error) = service.rebuild(&directory, Origin::Tool("vault_index_rebuild".to_owned())) else {
        return Err(std::io::Error::other("colliding index names did not require operator resolution").into());
    };

    assert_eq!(error.code(), "index/legacy-conflict");
    assert_eq!(std::fs::read(notes.join("_index.md"))?, b"legacy bytes\n");
    assert_eq!(std::fs::read(notes.join("index.md"))?, b"current bytes\n");
    Ok(())
}

#[test]
fn completed_file_event_reconciles_its_parent_and_returns_internal_event() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(notes.join("existing.md"), "# Existing\n")?;
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
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    std::fs::write(notes.join("added.md"), "# Added\n")?;
    let added = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes/added.md",
    })?;

    let events = service
        .reconcile(&OperationEvent {
            kind: OpKind::Create,
            paths: vec![added],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Created notes/added.md (8 bytes)".to_owned(),
            at: datetime!(2026-07-18 18:30:00 +10:00),
        })
        .map_err(|warning| warning.message)?;
    let rebuilt = std::fs::read_to_string(notes.join("index.md"))?;

    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|event| event.origin == Origin::Internal("index".to_owned()))
    );
    assert!(rebuilt.contains("| [added.md](added.md) | Added <!-- auto --> |"));
    assert!(rebuilt.contains("| [existing.md](existing.md) | Existing <!-- auto --> |"));
    Ok(())
}

#[test]
fn rebuild_uses_the_selected_identity_in_a_multi_vault_set() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::write(second.path().join("note.md"), "# Second Vault\n")?;
    let first_root = VaultRoot::try_from(VaultRootInput {
        path: first.path().to_path_buf(),
        managed: true,
        name: Some("first".to_owned()),
    })?;
    let second_root = VaultRoot::try_from(VaultRootInput {
        path: second.path().to_path_buf(),
        managed: true,
        name: Some("second".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![first_root, second_root.clone()])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![
            FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            },
            FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            },
        ],
        hidden: vec![default_hidden_patterns(), default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root: second_root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    let absolute = second.path().to_string_lossy().into_owned();
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: &absolute,
    })?;

    service.rebuild(&directory, Origin::Tool("vault_index_rebuild".to_owned()))?;

    assert!(!first.path().join("index.md").exists());
    assert!(second.path().join("index.md").exists());
    Ok(())
}

/// Regression: a rebuild failure over invalid YAML frontmatter used to
/// surface only the parser's line/column, with no indication of which
/// file among the scanned subtree was actually at fault. `IndexError::
/// Frontmatter` now carries the offending file's vault-relative path
/// (discovered live against an operator vault: a Templater-style
/// `title: {{Placeholder}}` frontmatter value, which opens an unquoted
/// YAML flow mapping at the first `{`).
#[test]
fn a_frontmatter_parse_failure_during_rebuild_names_the_offending_file() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let notes = vault.path().join("notes");
    std::fs::create_dir(&notes)?;
    std::fs::write(notes.join("ok.md"), "# Fine\n\nNothing wrong here.\n")?;
    std::fs::write(notes.join("broken.md"), "---\ntitle: {{Placeholder}}\n---\nBody.\n")?;
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
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: Vec::new(),
    })?;
    let directory = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "notes",
    })?;

    let Err(error) = service.rebuild(&directory, Origin::Tool("vault_index_rebuild".to_owned())) else {
        return Err("broken.md's frontmatter should fail the rebuild".into());
    };
    let IndexServiceError::Index(IndexError::Frontmatter { path, .. }) = error else {
        return Err(format!("expected IndexError::Frontmatter, got: {error:?}").into());
    };
    assert_eq!(path, std::path::Path::new("notes/broken.md"));
    Ok(())
}

/// `fs_delete_file` may only treat a directory's managed
/// `index.md`/`_index.md` as ignorable content when this service would
/// actually recreate it, i.e. the directory belongs to this service's own
/// root and is not excluded from index maintenance. `manages_directory`
/// is the query the tool handler uses to decide that, so it must agree
/// with `reconcile_event`'s own exclusion and root checks exactly.
#[test]
fn manages_directory_reports_index_maintenance_scope() -> Result<(), Box<dyn std::error::Error>> {
    let vault_a = tempdir()?;
    let vault_b = tempdir()?;
    std::fs::create_dir(vault_a.path().join("notes"))?;
    std::fs::create_dir(vault_a.path().join("archive"))?;
    let root_a = VaultRoot::try_from(VaultRootInput {
        path: vault_a.path().to_path_buf(),
        managed: true,
        name: Some("vault-a".to_owned()),
    })?;
    let root_b = VaultRoot::try_from(VaultRootInput {
        path: vault_b.path().to_path_buf(),
        managed: true,
        name: Some("vault-b".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root_a.clone(), root_b.clone()])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![
            FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            },
            FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            },
        ],
        hidden: vec![default_hidden_patterns(), default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: filesystem.clone(),
        clock: FixedClock,
    });
    let service = IndexService::try_from(IndexServiceConfig {
        root: root_a,
        roots: roots.clone(),
        reader: filesystem,
        writer,
        excluded: vec!["archive".to_owned()],
    })?;

    let managed = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "vault-a://notes",
    })?;
    let excluded = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "vault-a://archive",
    })?;
    let other_root = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "vault-b://.",
    })?;

    assert!(service.manages_directory(&managed));
    assert!(!service.manages_directory(&excluded));
    assert!(!service.manages_directory(&other_root));
    Ok(())
}
