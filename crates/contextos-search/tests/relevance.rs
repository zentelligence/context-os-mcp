//! FR-50 relevance smoke harness (Phase 4 delivery-plan gate item: "20
//! known-answer queries return the target file top-3").
//!
//! `fr_50_relevance_smoke_set_ranks_targets_top_three` runs the mock vault
//! and query set checked in under `mock/` and is part of the ordinary
//! `cargo test` gate.
//!
//! `acceptance_relevance_against_operator_vault` is the same harness pointed
//! at an operator-supplied vault copy for the acceptance step described in
//! `mock/README.md`. It is `#[ignore]`d and returns `Ok(())` without
//! indexing anything when either environment variable is unset, so it never
//! depends on the operator's home directory or network access.

use std::fs;
use std::path::{Path, PathBuf};

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_search::{
    DocumentSource, IndexedDocument, IndexesText, TantivyIndex, TextIndexConfig, TextQuery,
};
use serde_json::{Map, Value};
use time::OffsetDateTime;

/// One known-answer entry from `queries.json` (the `note` field is
/// documentation for humans and is not read by the harness).
struct KnownAnswerQuery {
    query: String,
    target: String,
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mock")
}

/// A fixed, deterministic modification time for every indexed document.
fn fixed_modified() -> Result<OffsetDateTime, Box<dyn std::error::Error>> {
    Ok(OffsetDateTime::from_unix_timestamp(1_774_000_000)?)
}

/// Copies `source` into `destination`, skipping any entry whose name appears
/// in `skip_names`. `mock/` holds the fixture's own `README.md` and
/// `queries.json` alongside the vault content it describes, so those two
/// are excluded here rather than being indexed as if they were vault notes.
fn copy_dir_recursive(
    source: &Path,
    destination: &Path,
    skip_names: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if skip_names.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let file_type = entry.file_type()?;
        let target = destination.join(&name);
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target, skip_names)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Recursively collects every `.md` file under `root`, skipping any
/// directory named in `skip_dirs` wherever it appears in the tree.
fn collect_markdown_files(
    root: &Path,
    skip_dirs: &[&str],
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if skip_dirs.contains(&name.as_ref()) {
                    continue;
                }
                pending.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("md")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Joins path components with forward slashes regardless of host platform.
fn slash_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Indexes every markdown file under `root` (skipping `skip_dirs`) into a
/// fresh `TantivyIndex` stored under `index_directory`.
fn index_vault(
    root: &Path,
    skip_dirs: &[&str],
    index_directory: &Path,
) -> Result<TantivyIndex, Box<dyn std::error::Error>> {
    let vault_root = VaultRoot::try_from(VaultRootInput {
        path: root.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![vault_root])?;
    let modified = fixed_modified()?;

    let mut documents = Vec::new();
    for absolute in collect_markdown_files(root, skip_dirs)? {
        let relative = absolute
            .strip_prefix(root)
            .map_err(|_| "markdown file escaped the vault root during collection")?;
        let raw = slash_relative(relative);
        let content = fs::read_to_string(&absolute)?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: &raw,
        })?;
        documents.push(IndexedDocument::from(DocumentSource {
            path: &path,
            content: &content,
            modified,
        }));
    }

    let index = TantivyIndex::try_from(TextIndexConfig {
        directory: index_directory.to_path_buf(),
    })?;
    index.index(&documents)?;
    Ok(index)
}

fn load_queries(path: &Path) -> Result<Vec<KnownAnswerQuery>, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let entries = value
        .as_array()
        .ok_or("queries.json must contain a JSON array")?;

    let mut queries = Vec::with_capacity(entries.len());
    for entry in entries {
        let query = entry
            .get("query")
            .and_then(Value::as_str)
            .ok_or("each queries.json entry needs a string \"query\" field")?
            .to_owned();
        let target = entry
            .get("target")
            .and_then(Value::as_str)
            .ok_or("each queries.json entry needs a string \"target\" field")?
            .to_owned();
        queries.push(KnownAnswerQuery { query, target });
    }
    Ok(queries)
}

/// Runs every query with a limit of 20 and no filters, asserting the known
/// target appears in the first three hits. Reports a summary line and, on
/// any regression, a message naming the query, the expected target, and the
/// actual top-3 paths for every failing entry.
fn assert_targets_rank_top_three(
    index: &TantivyIndex,
    queries: &[KnownAnswerQuery],
) -> Result<(), Box<dyn std::error::Error>> {
    let no_fields: Map<String, Value> = Map::new();
    let mut failures = Vec::new();

    for entry in queries {
        let hits = index.query(&TextQuery {
            query: &entry.query,
            path_prefix: None,
            exclude_paths: &[],
            tags: &[],
            fields: &no_fields,
            limit: 20,
        })?;
        let top_three: Vec<&str> = hits.iter().take(3).map(|hit| hit.path.as_str()).collect();
        if !top_three.contains(&entry.target.as_str()) {
            failures.push(format!(
                "query {:?}: expected target {:?} in top-3 but the top-3 was {:?}",
                entry.query, entry.target, top_three
            ));
        }
    }

    let total = queries.len();
    let passed = total - failures.len();
    println!("{passed}/{total} targets in top-3");

    assert!(
        failures.is_empty(),
        "relevance smoke set regressions:\n{}",
        failures.join("\n")
    );
    Ok(())
}

#[test]
fn fr_50_relevance_smoke_set_ranks_targets_top_three() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_root = fixture_dir();
    let queries_path = fixture_root.join("queries.json");

    let vault_copy = tempfile::tempdir()?;
    copy_dir_recursive(
        &fixture_root,
        vault_copy.path(),
        &["README.md", "queries.json"],
    )?;

    let index_directory = tempfile::tempdir()?;
    let index = index_vault(vault_copy.path(), &[], index_directory.path())?;

    let queries = load_queries(&queries_path)?;
    assert_targets_rank_top_three(&index, &queries)?;
    Ok(())
}

#[test]
#[ignore = "operator acceptance: set CONTEXTOS_RELEVANCE_VAULT and CONTEXTOS_RELEVANCE_QUERIES"]
fn acceptance_relevance_against_operator_vault() -> Result<(), Box<dyn std::error::Error>> {
    let (Ok(vault_var), Ok(queries_var)) = (
        std::env::var("CONTEXTOS_RELEVANCE_VAULT"),
        std::env::var("CONTEXTOS_RELEVANCE_QUERIES"),
    ) else {
        return Ok(());
    };

    let vault_root = PathBuf::from(vault_var);
    let queries_path = PathBuf::from(queries_var);
    let skip_dirs = [".contextos", ".git", ".obsidian"];

    let index_directory = tempfile::tempdir()?;
    let index = index_vault(&vault_root, &skip_dirs, index_directory.path())?;

    let queries = load_queries(&queries_path)?;
    assert_targets_rank_top_three(&index, &queries)?;
    Ok(())
}
