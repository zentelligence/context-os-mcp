mod support;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use contextos_core::{
    OpKind, OperationEvent, Origin, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput,
    VaultSet,
};
use contextos_search::{
    DocumentSource, FreshnessReport, IndexedDocument, IndexesText, TantivyIndex, TextIndexConfig, TextQuery,
    TextSearchService, TextSyncConfig,
};
use serde_json::Map;
use support::{document, timestamp, vault_note};
use time::OffsetDateTime;

/// A temporary directory whose basename is a valid RFC 3986 scheme token
/// (starts with an ASCII letter), unlike the bare `tempfile::tempdir()`
/// default, which on this platform yields a leading-dot name.
/// `TextSyncConfig` takes a raw root path and `refresh()` derives its
/// `VaultRoot` name from the directory's basename, so the fixture must
/// control that basename here.
fn vault_dir() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

fn service_at(
    vault: &tempfile::TempDir,
    excludes: Vec<String>,
) -> Result<TextSearchService<TantivyIndex>, Box<dyn std::error::Error>> {
    let index = TantivyIndex::try_from(TextIndexConfig {
        directory: vault.path().join(".contextos").join("index"),
    })?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?
    .path()
    .to_path_buf();
    let root_id = VaultRootId::try_from(0_usize)?;
    Ok(TextSearchService::from(TextSyncConfig {
        root_id,
        root,
        excludes,
        index,
    }))
}

/// Builds a `VaultPath` for `relative` without writing or overwriting file
/// content, unlike `support::vault_note`.
fn vault_path(vault: &tempfile::TempDir, relative: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    Ok(VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: relative,
    })?)
}

fn write_event(kind: OpKind, paths: Vec<VaultPath>) -> OperationEvent {
    OperationEvent {
        kind,
        paths,
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "test".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// Applies an event and asserts the update succeeded, converting the
/// non-`std::error::Error` `OperationWarning` into a boxed test error.
fn apply(service: &TextSearchService<TantivyIndex>, event: &OperationEvent) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(()) = service.update(event) else {
        return Err("expected the search update to succeed".into());
    };
    Ok(())
}

fn plain_query<'a>(query: &'a str, fields: &'a Map<String, serde_json::Value>) -> TextQuery<'a> {
    TextQuery {
        query,
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields,
        limit: 20,
    }
}

#[test]
fn update_event_indexes_written_note() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;
    let (_roots, path) = vault_note(&vault, "notes/alpha.md", "# Alpha\n\nGadget prose.\n")?;

    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let no_fields = Map::new();
    let hits = service.index().query(&plain_query("gadget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/alpha.md");
    Ok(())
}

#[test]
fn update_handles_move_and_delete_events() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;
    let (_roots, a_path) = vault_note(&vault, "notes/a.md", "# A\n\nWidget prose.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![a_path.clone()]))?;

    let absolute_a = vault.path().join("notes/a.md");
    let absolute_b = vault.path().join("notes/b.md");
    fs::rename(&absolute_a, &absolute_b)?;
    let b_path = vault_path(&vault, "notes/b.md")?;
    apply(&service, &write_event(OpKind::Move, vec![a_path, b_path.clone()]))?;

    let no_fields = Map::new();
    let hits = service.index().query(&plain_query("widget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/b.md");

    fs::remove_file(&absolute_b)?;
    apply(&service, &write_event(OpKind::Delete, vec![b_path]))?;

    let gone = service.index().query(&plain_query("widget", &no_fields))?;
    assert!(gone.is_empty());
    Ok(())
}

#[test]
fn external_edit_bypassing_events_is_reindexed_on_refresh() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;
    let (_roots, path) = vault_note(&vault, "notes/item.md", "# Item\n\nOriginal gadget prose.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let absolute = vault.path().join("notes/item.md");
    fs::write(&absolute, "# Item\n\nReplacement widget prose.\n")?;
    let file = std::fs::File::options().write(true).open(&absolute)?;
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_770_100_000))?;
    drop(file);

    let report = service.refresh()?;
    assert_eq!(report.reindexed, 1);

    let no_fields = Map::new();
    let fresh = service.index().query(&plain_query("widget", &no_fields))?;
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].path, "notes/item.md");
    let stale = service.index().query(&plain_query("gadget", &no_fields))?;
    assert!(stale.is_empty());
    Ok(())
}

#[test]
fn refresh_reconciles_new_and_deleted_files() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;

    let gone_content = "# Gone\n\nStale prose.\n";
    let (_roots, gone_path) = vault_note(&vault, "notes/gone.md", gone_content)?;
    let gone_document = IndexedDocument::from(DocumentSource {
        path: &gone_path,
        content: gone_content,
        modified: timestamp()?,
    });
    service.index().index(&[gone_document])?;
    fs::remove_file(vault.path().join("notes/gone.md"))?;

    vault_note(&vault, "notes/new.md", "# New\n\nFresh widget prose.\n")?;

    let report = service.refresh()?;
    assert_eq!(
        report,
        FreshnessReport {
            scanned: 1,
            reindexed: 1,
            removed: 1,
        }
    );

    let no_fields = Map::new();
    let found = service.index().query(&plain_query("widget", &no_fields))?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "notes/new.md");
    let stale = service.index().query(&plain_query("stale", &no_fields))?;
    assert!(stale.is_empty());
    Ok(())
}

#[test]
fn second_refresh_reindexes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;
    vault_note(&vault, "alpha.md", "# Alpha\n\nFirst note.\n")?;
    vault_note(&vault, "beta.md", "# Beta\n\nSecond note.\n")?;

    let first = service.refresh()?;
    assert_eq!(first.scanned, 2);
    assert_eq!(first.reindexed, 2);
    assert_eq!(first.removed, 0);

    let second = service.refresh()?;
    assert_eq!(second.reindexed, 0);
    assert_eq!(second.removed, 0);
    assert_eq!(second.scanned, 2);
    Ok(())
}

#[test]
fn refresh_ignores_excluded_and_non_markdown() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let excludes = vec![".obsidian".to_owned(), ".contextos".to_owned(), "memory/log".to_owned()];
    let service = service_at(&vault, excludes)?;

    vault_note(&vault, "notes/keep.md", "# Keep\n\nSearchable widget prose.\n")?;
    vault_note(&vault, ".obsidian/plug.md", "# Plugin\n\nHidden widget prose.\n")?;
    vault_note(&vault, "memory/log/2026/07/19.md", "# Log\n\nHidden widget prose.\n")?;
    vault_note(&vault, "notes/data.txt", "Hidden widget prose.\n")?;

    let report = service.refresh()?;
    assert_eq!(report.scanned, 1);
    assert_eq!(report.reindexed, 1);
    assert_eq!(report.removed, 0);

    let no_fields = Map::new();
    let hits = service.index().query(&plain_query("widget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/keep.md");
    Ok(())
}

#[test]
fn unreadable_document_degrades_to_warning() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, vec![])?;

    let good = document(&vault, "notes/good.md", "# Good\n\nReadable widget prose.\n")?;
    service.index().index(&[good])?;

    let bad_absolute = vault.path().join("notes/bad.md");
    let parent: &Path = bad_absolute.parent().ok_or("missing parent directory")?;
    fs::create_dir_all(parent)?;
    fs::write(&bad_absolute, [0x66, 0x6f, 0x80, 0x6f])?;
    let bad_path = vault_path(&vault, "notes/bad.md")?;

    let Err(warning) = service.update(&write_event(OpKind::Create, vec![bad_path])) else {
        return Err("expected update to degrade to a warning".into());
    };
    assert_eq!(warning.code, "index/storage");

    let no_fields = Map::new();
    let hits = service.index().query(&plain_query("widget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/good.md");
    Ok(())
}
