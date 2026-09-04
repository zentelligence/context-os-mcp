mod support;

use contextos_search::{
    DocumentSource, IndexedDocument, IndexesText, TantivyIndex, TextIndexConfig, TextQuery,
};
use serde_json::{Map, Value};
use support::{document, timestamp, vault_note};

fn index_at(directory: &std::path::Path) -> Result<TantivyIndex, Box<dyn std::error::Error>> {
    Ok(TantivyIndex::try_from(TextIndexConfig {
        directory: directory.join(".contextos").join("index"),
    })?)
}

fn plain_query<'a>(query: &'a str, fields: &'a Map<String, Value>) -> TextQuery<'a> {
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
fn fr_50_plain_term_query_returns_ranked_hit_with_heading_snippet()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "notes/search.md",
            "# Search Engine\n\nGeneral overview prose.\n\n## Details\n\nThe tantivy engine indexes markdown quickly.\n",
        )?,
        document(&vault, "notes/cooking.md", "# Cooking\n\nRisotto needs patience.\n")?,
        document(&vault, "journal/day.md", "# Day\n\nWalked the dog.\n")?,
    ])?;

    let no_fields = Map::new();
    let hits = index.query(&plain_query("tantivy", &no_fields))?;

    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.path, "notes/search.md");
    assert!(hit.score > 0.0);
    assert_eq!(hit.title, "Search Engine");
    assert!(hit.snippet.contains("<b>tantivy</b>"));
    assert!(hit.snippet.contains("Details"));
    assert_eq!(hit.modified, timestamp()?);
    Ok(())
}

#[test]
fn fr_50_title_match_outranks_single_body_mention() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "guides/tantivy.md",
            "---\ntitle: Tantivy Guide\n---\n\nIndexing library notes.\n",
        )?,
        document(
            &vault,
            "notes/mention.md",
            "# Notes\n\nWe evaluated tantivy briefly alongside several other options during review.\n",
        )?,
    ])?;

    let no_fields = Map::new();
    let hits = index.query(&plain_query("tantivy", &no_fields))?;

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, "guides/tantivy.md");
    assert!(!hits[0].snippet.is_empty());
    Ok(())
}

#[test]
fn fr_50_path_prefix_filter_scopes_to_component_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "notes/widget.md",
            "# Widget\n\nThe gadget register.\n",
        )?,
        document(
            &vault,
            "archive/widget.md",
            "# Old Widget\n\nThe gadget register.\n",
        )?,
    ])?;
    let no_fields = Map::new();

    let scoped = index.query(&TextQuery {
        query: "gadget",
        path_prefix: Some("notes"),
        exclude_paths: &[],
        tags: &[],
        fields: &no_fields,
        limit: 20,
    })?;
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].path, "notes/widget.md");

    let partial_component = index.query(&TextQuery {
        query: "gadget",
        path_prefix: Some("no"),
        exclude_paths: &[],
        tags: &[],
        fields: &no_fields,
        limit: 20,
    })?;
    assert!(partial_component.is_empty());
    Ok(())
}

#[test]
fn fr_116_exclude_paths_filter_scopes_out_matching_segments()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "notes/widget.md",
            "# Widget\n\nThe gadget register.\n",
        )?,
        document(
            &vault,
            "archive/widget.md",
            "# Old Widget\n\nThe gadget register.\n",
        )?,
        // Merely starts with the same characters as the excluded segment,
        // but is not that segment itself: must NOT be excluded.
        document(
            &vault,
            "archived-notes/widget.md",
            "# Archived Notes Widget\n\nThe gadget register.\n",
        )?,
    ])?;
    let no_fields = Map::new();

    let excluding_archive = index.query(&TextQuery {
        query: "gadget",
        path_prefix: None,
        exclude_paths: &["archive".to_owned()],
        tags: &[],
        fields: &no_fields,
        limit: 20,
    })?;
    let mut paths: Vec<&str> = excluding_archive
        .iter()
        .map(|hit| hit.path.as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["archived-notes/widget.md", "notes/widget.md"]);

    let excluding_multiple = index.query(&TextQuery {
        query: "gadget",
        path_prefix: None,
        exclude_paths: &["archive".to_owned(), "archived-notes".to_owned()],
        tags: &[],
        fields: &no_fields,
        limit: 20,
    })?;
    assert_eq!(excluding_multiple.len(), 1);
    assert_eq!(excluding_multiple[0].path, "notes/widget.md");

    // Composes with `path_prefix`: excluding a segment that already sits
    // outside the included prefix changes nothing.
    let combined = index.query(&TextQuery {
        query: "gadget",
        path_prefix: Some("notes"),
        exclude_paths: &["archive".to_owned()],
        tags: &[],
        fields: &no_fields,
        limit: 20,
    })?;
    assert_eq!(combined.len(), 1);
    assert_eq!(combined[0].path, "notes/widget.md");
    Ok(())
}

#[test]
fn fr_50_tag_filter_matches_nested_tags_hierarchically() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "notes/alpha.md",
            "---\ntags:\n  - project/alpha\n---\n\nShared launch checklist.\n",
        )?,
        document(
            &vault,
            "notes/review.md",
            "---\ntags:\n  - review\n---\n\nShared launch checklist.\n",
        )?,
    ])?;
    let no_fields = Map::new();

    let parent_tag = vec!["project".to_owned()];
    let by_parent = index.query(&TextQuery {
        query: "launch",
        path_prefix: None,
        exclude_paths: &[],
        tags: &parent_tag,
        fields: &no_fields,
        limit: 20,
    })?;
    assert_eq!(by_parent.len(), 1);
    assert_eq!(by_parent[0].path, "notes/alpha.md");

    let exact_tag = vec!["review".to_owned()];
    let by_exact = index.query(&TextQuery {
        query: "launch",
        path_prefix: None,
        exclude_paths: &[],
        tags: &exact_tag,
        fields: &no_fields,
        limit: 20,
    })?;
    assert_eq!(by_exact.len(), 1);
    assert_eq!(by_exact[0].path, "notes/review.md");
    Ok(())
}

#[test]
fn fr_50_frontmatter_field_filter_requires_exact_equality() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(
            &vault,
            "zentelligence/plan.md",
            "---\nentity: zentelligence\nregion: New South Wales\n---\n\nQuarterly budget plan.\n",
        )?,
        document(
            &vault,
            "personal/plan.md",
            "---\nentity: personal\n---\n\nQuarterly budget plan.\n",
        )?,
    ])?;

    let mut entity_filter = Map::new();
    entity_filter.insert(
        "entity".to_owned(),
        Value::String("zentelligence".to_owned()),
    );
    let by_entity = index.query(&plain_query("budget", &entity_filter))?;
    assert_eq!(by_entity.len(), 1);
    assert_eq!(by_entity[0].path, "zentelligence/plan.md");

    let mut region_filter = Map::new();
    region_filter.insert(
        "region".to_owned(),
        Value::String("New South Wales".to_owned()),
    );
    let by_region = index.query(&plain_query("budget", &region_filter))?;
    assert_eq!(by_region.len(), 1);
    assert_eq!(by_region[0].path, "zentelligence/plan.md");

    let mut absent_filter = Map::new();
    absent_filter.insert("entity".to_owned(), Value::String("missing".to_owned()));
    let by_absent = index.query(&plain_query("budget", &absent_filter))?;
    assert!(by_absent.is_empty());
    Ok(())
}

#[test]
fn fr_50_invalid_query_syntax_returns_stable_error() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[document(&vault, "notes/a.md", "# A\n\nProse.\n")?])?;

    let no_fields = Map::new();
    let Err(error) = index.query(&plain_query("title:(", &no_fields)) else {
        return Err("expected an invalid-query error".into());
    };
    assert_eq!(error.code(), "index/invalid-query");
    Ok(())
}

#[test]
fn fr_50_remove_deletes_the_stored_document() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[document(
        &vault,
        "notes/gone.md",
        "# Gone\n\nEphemeral gadget.\n",
    )?])?;

    index.remove("notes/gone.md")?;

    let no_fields = Map::new();
    let hits = index.query(&plain_query("gadget", &no_fields))?;
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn fr_51_reindexing_a_path_replaces_the_previous_document() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[document(
        &vault,
        "notes/item.md",
        "# Item\n\nOriginal gadget.\n",
    )?])?;
    index.index(&[document(
        &vault,
        "notes/item.md",
        "# Item\n\nReplacement widget.\n",
    )?])?;

    let no_fields = Map::new();
    let stale = index.query(&plain_query("gadget", &no_fields))?;
    assert!(stale.is_empty());
    let fresh = index.query(&plain_query("widget", &no_fields))?;
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].path, "notes/item.md");
    Ok(())
}

#[test]
fn fr_50_limit_caps_the_result_count() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let index = index_at(vault.path())?;
    index.index(&[
        document(&vault, "a.md", "# A\n\nCommon topic one.\n")?,
        document(&vault, "b.md", "# B\n\nCommon topic two.\n")?,
        document(&vault, "c.md", "# C\n\nCommon topic three.\n")?,
    ])?;

    let no_fields = Map::new();
    let hits = index.query(&TextQuery {
        query: "common",
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: &no_fields,
        limit: 2,
    })?;
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn fr_50_second_instance_opens_for_reads_while_first_stays_alive()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let first = index_at(vault.path())?;
    first.index(&[document(&vault, "notes/a.md", "# A\n\nGadget prose.\n")?])?;

    // Opening a second handle onto the same directory must not require the
    // exclusive tantivy IndexWriter lock: that lock is only needed for the
    // duration of an actual write, not for the lifetime of a connection
    // (D-09 follow-up: a second server process against the same vault must
    // not fail outright just because a first process is already open).
    let second = index_at(vault.path())?;
    let no_fields = Map::new();
    let hits = second.query(&plain_query("gadget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    drop(first);
    Ok(())
}

#[test]
fn fr_50_second_instance_can_write_while_first_instance_is_idle()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let first = index_at(vault.path())?;
    let second = index_at(vault.path())?;

    second.index(&[document(&vault, "notes/b.md", "# B\n\nWidget prose.\n")?])?;

    let no_fields = Map::new();
    let hits = second.query(&plain_query("widget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    drop(first);
    Ok(())
}

#[test]
fn fr_50_index_persists_across_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# Durable\n\nPersistent gadget entry.\n";
    let (_roots, path) = vault_note(&vault, "notes/durable.md", content)?;
    let stored = IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified: timestamp()?,
    });

    {
        let index = index_at(vault.path())?;
        index.index(std::slice::from_ref(&stored))?;
    }

    let reopened = index_at(vault.path())?;
    let no_fields = Map::new();
    let hits = reopened.query(&plain_query("gadget", &no_fields))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/durable.md");
    Ok(())
}
