mod support;

use std::fs;

use contextos_core::{
    OpKind, OperationEvent, Origin, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot,
    VaultRootId, VaultRootInput, VaultSet,
};
use contextos_search::{
    FakeEmbedder, GraphBackend, GraphDirection, RebuildProgress, RebuildTarget, SearchError,
    SemanticConfig, SemanticQuery, TextQuery, VaultSearchConfig, VaultSearchService,
};
use serde_json::Map;
use support::{document, vault_note};
use time::OffsetDateTime;

/// A temporary directory whose basename is a valid RFC 3986 scheme token
/// (starts with an ASCII letter), unlike the bare `tempfile::tempdir()`
/// default, which on this platform yields a leading-dot name.
/// `VaultSearchConfig` takes a raw root path and rebuild/refresh paths
/// derive their `VaultRoot` name from the directory's basename, so the
/// fixture must control that basename here.
fn vault_dir() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

fn service_at(
    vault: &tempfile::TempDir,
    text_enabled: bool,
    graph_enabled: bool,
) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![],
        state_directory: vault.path().join(".contextos"),
        text_enabled,
        graph_enabled,
        graph_backend: GraphBackend::Fjall,
        semantic: None,
    })?)
}

/// A service with text and graph disabled but semantic search enabled
/// through a deterministic [`FakeEmbedder`], for `query_semantic` and
/// semantic status/rebuild contract tests.
fn semantic_service_at(
    vault: &tempfile::TempDir,
) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![],
        state_directory: vault.path().join(".contextos"),
        text_enabled: false,
        graph_enabled: false,
        graph_backend: GraphBackend::Fjall,
        semantic: Some(SemanticConfig {
            embedder: Box::new(FakeEmbedder::default()),
            vector_store_path: vault.path().join(".contextos/vectors.db"),
        }),
    })?)
}

/// D-06: `fjall` holds an exclusive lock on a graph store's directory for
/// as long as it is open, so a second `VaultSearchService` constructed over
/// the same vault while the first is still live cannot open the link graph.
/// This must degrade the second service's graph capability to disabled
/// (the same representation `graph_enabled = false` already produces)
/// rather than failing the whole service's construction, so a second
/// connector instance still gets a working server with every other
/// capability intact.
#[test]
fn graph_store_locked_by_another_instance_degrades_to_disabled()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let first = service_at(&vault, false, true)?;
    let second = service_at(&vault, false, true)?;

    assert!(
        first.status()?.graph.enabled,
        "the first instance should hold the graph store lock and keep the link graph enabled"
    );
    assert!(
        !second.status()?.graph.enabled,
        "the second instance should degrade to a disabled link graph rather than fail entirely"
    );

    drop(first);
    Ok(())
}

/// Builds a `VaultPath` for `relative` without writing or overwriting file
/// content, unlike `support::vault_note`.
fn vault_path(
    vault: &tempfile::TempDir,
    relative: &str,
) -> Result<VaultPath, Box<dyn std::error::Error>> {
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

fn apply(
    service: &VaultSearchService,
    event: &OperationEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(()) = service.update(event) else {
        return Err("expected the combined search update to succeed".into());
    };
    Ok(())
}

fn plain_query(query: &str) -> TextQuery<'_> {
    static NO_FIELDS: std::sync::OnceLock<Map<String, serde_json::Value>> =
        std::sync::OnceLock::new();
    TextQuery {
        query,
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: NO_FIELDS.get_or_init(Map::new),
        limit: 20,
    }
}

#[test]
fn fr_50_fr_51_query_text_indexes_through_the_combined_service()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    let (_roots, path) = vault_note(&vault, "notes/alpha.md", "# Alpha\n\nGadget prose.\n")?;

    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let (hits, freshness) = service.query_text(&plain_query("gadget"))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/alpha.md");
    assert_eq!(freshness.scanned, 1);
    Ok(())
}

#[test]
fn fr_50_query_text_hit_title_matches_the_derived_document_title()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    let expected = document(
        &vault,
        "alpha.md",
        "# Alpha Heading\n\nSearchable content.\n",
    )?;
    let path = vault_path(&vault, "alpha.md")?;
    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let (hits, _freshness) = service.query_text(&plain_query("searchable"))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, expected.title());
    Ok(())
}

#[test]
fn fr_116_query_text_respects_exclude_paths_filter() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    let (_roots, current_path) = vault_note(&vault, "notes/gadget.md", "# Gadget\n\nProse.\n")?;
    let (_roots, superseded_path) =
        vault_note(&vault, "notes/old/gadget.md", "# Old Gadget\n\nProse.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![current_path]))?;
    apply(
        &service,
        &write_event(OpKind::Create, vec![superseded_path]),
    )?;

    let (hits, _freshness) = service.query_text(&TextQuery {
        query: "prose",
        path_prefix: None,
        exclude_paths: &["notes/old".to_owned()],
        tags: &[],
        fields: &Map::new(),
        limit: 20,
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/gadget.md");
    Ok(())
}

#[test]
fn text_disabled_reports_the_stable_disabled_error() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, false, true)?;

    let Err(error) = service.query_text(&plain_query("anything")) else {
        return Err("expected query_text to fail when text search is disabled".into());
    };
    assert_eq!(error.code(), "index/disabled");
    assert!(matches!(error, SearchError::TextDisabled));
    Ok(())
}

#[test]
fn semantic_disabled_reports_the_stable_disabled_error() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;

    let Err(error) = service.query_semantic(&SemanticQuery {
        query: "anything",
        limit: 10,
        path_prefix: None,
        exclude_paths: &[],
    }) else {
        return Err("expected query_semantic to fail when semantic search is disabled".into());
    };
    assert_eq!(error.code(), "index/disabled");
    assert!(matches!(error, SearchError::SemanticUnavailable));
    Ok(())
}

#[test]
fn graph_disabled_reports_the_stable_disabled_error() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, false)?;

    let Err(error) = service.graph_orphans() else {
        return Err("expected graph_orphans to fail when the link graph is disabled".into());
    };
    assert_eq!(error.code(), "index/disabled");
    assert!(matches!(error, SearchError::GraphDisabled));
    Ok(())
}

#[test]
fn fr_52_graph_updates_through_create_move_and_delete_events()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;

    // b.md is created first so the wikilink target already exists as a real
    // note when a.md is wired, avoiding phantom-upgrade mechanics (mirroring
    // the link_graph.rs contract tests for the same reason).
    let (_roots, b_path) = vault_note(&vault, "b.md", "# B\n\nno links\n")?;
    let (_roots, a_path) = vault_note(&vault, "a.md", "# A\n\n[[b]]\n")?;
    apply(&service, &write_event(OpKind::Create, vec![b_path.clone()]))?;
    apply(&service, &write_event(OpKind::Create, vec![a_path.clone()]))?;

    let neighbours = service.graph_neighbours("a.md", 1, GraphDirection::Out)?;
    let mut paths: Vec<&str> = neighbours
        .nodes
        .iter()
        .map(|node| node.path.as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["a.md", "b.md"]);

    let backlinks = service.graph_backlinks("b.md")?;
    assert_eq!(
        backlinks.nodes.iter().filter(|n| n.path == "a.md").count(),
        1
    );

    let path_view = service.graph_path("a.md", "b.md", GraphDirection::Out)?;
    assert_eq!(path_view.edges.len(), 1);

    // Move a.md to c.md: c.md should now link to b.md, and a.md should
    // disappear from the graph.
    fs::rename(vault.path().join("a.md"), vault.path().join("c.md"))?;
    let c_path = vault_path(&vault, "c.md")?;
    apply(
        &service,
        &write_event(OpKind::Move, vec![a_path, c_path.clone()]),
    )?;
    let moved_backlinks = service.graph_backlinks("b.md")?;
    let moved_paths: Vec<&str> = moved_backlinks
        .nodes
        .iter()
        .map(|node| node.path.as_str())
        .collect();
    assert!(moved_paths.contains(&"c.md"));
    assert!(!moved_paths.contains(&"a.md"));

    // Delete b.md: it should no longer report as an orphan target, and
    // c.md's link becomes unresolved (phantom), not an error.
    fs::remove_file(vault.path().join("b.md"))?;
    apply(&service, &write_event(OpKind::Delete, vec![b_path]))?;
    let orphans_after = service.graph_orphans()?;
    assert!(!orphans_after.nodes.iter().any(|node| node.path == "b.md"));
    Ok(())
}

#[test]
fn fr_52_malformed_wikilink_syntax_degrades_without_failing_the_update()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    // An unterminated wikilink is rejected by the markdown parser; the
    // combined service must still succeed (skip only the graph mutation).
    let (_roots, path) = vault_note(&vault, "broken.md", "# Broken\n\n[[unterminated\n")?;

    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let (hits, _freshness) = service.query_text(&plain_query("broken"))?;
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn fr_55_status_reports_document_and_graph_counts() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    let (_roots, b_path) = vault_note(&vault, "b.md", "# B\n\nno links\n")?;
    let (_roots, a_path) = vault_note(&vault, "a.md", "# A\n\n[[b]]\n")?;
    apply(&service, &write_event(OpKind::Create, vec![b_path]))?;
    apply(&service, &write_event(OpKind::Create, vec![a_path]))?;

    let status = service.status()?;
    assert!(status.text.enabled);
    assert_eq!(status.text.documents, 2);
    assert_eq!(status.text.stale_estimate, 0);
    assert!(status.graph.enabled);
    assert_eq!(status.graph.nodes, 2);
    assert_eq!(status.graph.edges, 1);
    assert!(!status.semantic.enabled);
    Ok(())
}

#[test]
fn fr_55_status_reports_the_resolved_state_directory() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;

    let status = service.status()?;

    assert_eq!(status.state_directory, vault.path().join(".contextos"));
    assert_eq!(service.state_directory(), vault.path().join(".contextos"));
    Ok(())
}

#[test]
fn fr_55_rebuild_reconciles_external_edits_and_reports_zero_staleness_afterwards()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    vault_note(&vault, "a.md", "# A\n\n[[b]]\n")?;
    vault_note(&vault, "b.md", "# B\n\nno links\n")?;

    // Nothing was routed through `update`, so the indexes start empty and
    // report full staleness until a rebuild reconciles them.
    let before = service.status()?;
    assert_eq!(before.text.documents, 0);
    assert_eq!(before.text.stale_estimate, 2);

    let report = service.rebuild(RebuildTarget::All)?;
    let text_report = report.text.ok_or("expected a text rebuild report")?;
    assert_eq!(text_report.reindexed, 2);
    let graph_report = report.graph.ok_or("expected a graph rebuild report")?;
    assert_eq!(graph_report.nodes, 2);
    assert_eq!(graph_report.edges, 1);

    let after = service.status()?;
    assert_eq!(after.text.documents, 2);
    assert_eq!(after.text.stale_estimate, 0);
    assert!(!after.graph.needs_rebuild);
    Ok(())
}

#[test]
fn fr_55_rebuild_with_progress_reports_text_and_graph_phase_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;
    vault_note(&vault, "a.md", "# A\n\n[[b]]\n")?;
    vault_note(&vault, "b.md", "# B\n\nno links\n")?;

    let mut observed = Vec::new();
    service.rebuild_with_progress(RebuildTarget::All, &mut |update| observed.push(update))?;

    assert_eq!(
        observed,
        vec![
            RebuildProgress::TextStarted,
            RebuildProgress::TextComplete,
            RebuildProgress::GraphStarted,
            RebuildProgress::GraphComplete,
        ]
    );
    Ok(())
}

#[test]
fn fr_55_semantic_rebuild_with_progress_reports_one_update_per_document()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    vault_note(&vault, "a.md", "# A\n\nFirst note.\n")?;
    vault_note(&vault, "b.md", "# B\n\nSecond note.\n")?;
    vault_note(&vault, "c.md", "# C\n\nThird note.\n")?;

    let mut observed = Vec::new();
    service.rebuild_with_progress(RebuildTarget::Semantic, &mut |update| observed.push(update))?;

    assert_eq!(
        observed,
        vec![
            RebuildProgress::SemanticProgress {
                completed: 1,
                total: 3
            },
            RebuildProgress::SemanticProgress {
                completed: 2,
                total: 3
            },
            RebuildProgress::SemanticProgress {
                completed: 3,
                total: 3
            },
        ]
    );
    Ok(())
}

#[test]
fn fr_55_semantic_rebuild_with_a_budget_returns_early_and_a_later_call_finishes_it()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    vault_note(&vault, "a.md", "# A\n\nFirst note.\n")?;
    vault_note(&vault, "b.md", "# B\n\nSecond note.\n")?;
    vault_note(&vault, "c.md", "# C\n\nThird note.\n")?;

    // A zero-second budget still guarantees forward progress (one document
    // embedded), leaving the rest queued rather than blocking to finish
    // them: the mechanism a client with a short request timeout relies on
    // to call `query_index_rebuild` repeatedly instead of hitting its own
    // timeout mid-rebuild.
    let first = service.rebuild_with_budget(
        RebuildTarget::Semantic,
        Some(time::Duration::ZERO),
        &mut |_| {},
    )?;
    let first_report = first.semantic.ok_or("expected a semantic rebuild report")?;
    assert_eq!(first_report.paths_scanned, 1);
    assert_eq!(
        first_report.remaining, 2,
        "the other two documents must still be queued, not lost"
    );

    let status_between_calls = service.status()?;
    assert_eq!(
        status_between_calls.semantic.documents, 1,
        "the one embedded document is already durably queryable"
    );

    // A second, unbudgeted call against the same long-lived service
    // finishes whatever the first call left behind.
    let second = service.rebuild_with_budget(RebuildTarget::Semantic, None, &mut |_| {})?;
    let second_report = second
        .semantic
        .ok_or("expected a semantic rebuild report")?;
    assert_eq!(second_report.remaining, 0);

    let status_after = service.status()?;
    assert_eq!(status_after.semantic.documents, 3);
    assert_eq!(status_after.semantic.stale_estimate, 0);
    Ok(())
}

#[test]
fn fr_55_repeated_budgeted_calls_converge_to_zero_remaining_without_reviving_completed_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    for name in ["a", "b", "c", "d", "e"] {
        vault_note(
            &vault,
            &format!("{name}.md"),
            &format!("# {name}\n\nContent for {name}.\n"),
        )?;
    }

    // Each call has a zero-second budget, so `drain_until`'s "at least
    // one" guarantee means it processes exactly one document per call:
    // the same shape of repeated small-budget polling a client with a
    // short request timeout performs. `remaining` must strictly decrease
    // every call and reach zero after exactly five calls (one per
    // document); a rebuild that re-walks the vault on every call instead
    // of only when the queue is empty would silently revive each
    // just-completed path, and `remaining` would oscillate rather than
    // converge (this reproduces the exact non-convergence reported against
    // a live vault).
    let mut previous_remaining = usize::MAX;
    for call in 1..=5 {
        let report = service.rebuild_with_budget(
            RebuildTarget::Semantic,
            Some(time::Duration::ZERO),
            &mut |_| {},
        )?;
        let semantic_report = report
            .semantic
            .ok_or("expected a semantic rebuild report")?;
        assert_eq!(
            semantic_report.paths_scanned, 1,
            "call {call}: a zero budget must process exactly one document"
        );
        assert!(
            semantic_report.remaining < previous_remaining,
            "call {call}: remaining ({}) did not decrease from the previous call ({previous_remaining})",
            semantic_report.remaining
        );
        previous_remaining = semantic_report.remaining;
    }
    assert_eq!(
        previous_remaining, 0,
        "all five documents must be processed after five calls"
    );

    let status = service.status()?;
    assert_eq!(status.semantic.documents, 5);
    assert_eq!(status.semantic.stale_estimate, 0);
    Ok(())
}

#[test]
fn fr_55_semantic_rebuild_is_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = service_at(&vault, true, true)?;

    let Err(error) = service.rebuild(RebuildTarget::Semantic) else {
        return Err("expected a semantic rebuild request to fail".into());
    };
    assert_eq!(error.code(), "index/disabled");
    assert!(matches!(error, SearchError::SemanticUnavailable));
    Ok(())
}

/// Regression test for a real incident: a background caller invoking
/// `rebuild`/`rebuild_with_budget` on every idle tick reintroduces the
/// exact vault-wide re-walk-and-rehash `rebuild_semantic`'s own doc
/// comment warns against, just on a timer instead of per-call. Unlike
/// `rebuild`, `drain_semantic_queue` must never discover an unenqueued
/// file by walking the vault, regardless of how many files exist.
#[test]
fn fr_55_drain_semantic_queue_never_walks_the_vault_to_discover_unqueued_files()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    vault_note(&vault, "note.md", "# Note\n\nStable content.\n")?;

    // Nothing was ever enqueued: no `update` call, no prior `rebuild`.
    let report = service.drain_semantic_queue(None)?;

    assert_eq!(report.paths_scanned, 0);
    assert_eq!(report.embedded, 0);
    assert_eq!(report.remaining, 0);
    assert_eq!(service.status()?.semantic.documents, 0);
    Ok(())
}

#[test]
fn fr_55_drain_semantic_queue_drains_only_what_was_explicitly_enqueued()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, path) = vault_note(&vault, "note.md", "# Note\n\nStable content.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    let report = service.drain_semantic_queue(None)?;

    assert_eq!(report.embedded, 1);
    assert_eq!(report.remaining, 0);
    assert_eq!(service.status()?.semantic.documents, 1);
    Ok(())
}

#[test]
fn fr_53_query_semantic_returns_the_matching_chunk_with_score_and_heading_context()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, path) = vault_note(
        &vault,
        "notes/alpha.md",
        "# Alpha\n\nGadget prose about widgets.\n",
    )?;
    apply(&service, &write_event(OpKind::Create, vec![path]))?;

    // `update` only enqueues; a rebuild forces the queued path through the
    // deterministic fake embedder and into the store before querying it.
    service.rebuild(RebuildTarget::Semantic)?;

    // The fake embedder is a deterministic hash of the chunk's exact text,
    // so querying with that same text yields an unambiguous top score.
    let hits = service.query_semantic(&SemanticQuery {
        query: "Gadget prose about widgets.",
        limit: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/alpha.md");
    assert_eq!(hits[0].chunk, "Gadget prose about widgets.");
    assert_eq!(hits[0].heading_context, vec!["Alpha".to_owned()]);
    assert!((hits[0].score - 1.0).abs() < 1e-4);
    Ok(())
}

#[test]
fn fr_53_query_semantic_respects_path_prefix_filter() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, a_path) = vault_note(&vault, "a/note.md", "# A\n\nContent in a.\n")?;
    let (_roots, b_path) = vault_note(&vault, "b/note.md", "# B\n\nContent in b.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![a_path]))?;
    apply(&service, &write_event(OpKind::Create, vec![b_path]))?;
    service.rebuild(RebuildTarget::Semantic)?;

    let hits = service.query_semantic(&SemanticQuery {
        query: "Content in a.",
        limit: 10,
        path_prefix: Some("a"),
        exclude_paths: &[],
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a/note.md");
    Ok(())
}

#[test]
fn fr_116_query_semantic_respects_exclude_paths_filter() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, a_path) = vault_note(&vault, "a/note.md", "# A\n\nContent in a.\n")?;
    let (_roots, b_path) = vault_note(&vault, "b/note.md", "# B\n\nContent in a.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![a_path]))?;
    apply(&service, &write_event(OpKind::Create, vec![b_path]))?;
    service.rebuild(RebuildTarget::Semantic)?;

    let hits = service.query_semantic(&SemanticQuery {
        query: "Content in a.",
        limit: 10,
        path_prefix: None,
        exclude_paths: &["b".to_owned()],
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a/note.md");
    Ok(())
}

#[test]
fn fr_53_query_semantic_limit_zero_returns_no_hits() -> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, path) = vault_note(&vault, "note.md", "# Note\n\nSome content.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![path]))?;
    service.rebuild(RebuildTarget::Semantic)?;

    let hits = service.query_semantic(&SemanticQuery {
        query: "Some content.",
        limit: 0,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn fr_53_query_semantic_skips_a_hit_whose_source_changed_since_it_was_embedded()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, path) = vault_note(&vault, "note.md", "# Note\n\nOriginal content.\n")?;
    apply(&service, &write_event(OpKind::Create, vec![path.clone()]))?;
    service.rebuild(RebuildTarget::Semantic)?;

    // The stored chunk's ordinal no longer exists once the section shrinks
    // to nothing: `query_semantic` must skip it rather than error or return
    // stale text.
    fs::write(vault.path().join("note.md"), "# Note\n")?;

    let hits = service.query_semantic(&SemanticQuery {
        query: "Original content.",
        limit: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn fr_55_status_reports_semantic_document_and_chunk_counts_and_staleness()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    let (_roots, path) = vault_note(
        &vault,
        "note.md",
        "# Note\n\nOne paragraph.\n\n# Second\n\nAnother paragraph.\n",
    )?;

    let queued = service.status()?;
    assert!(queued.semantic.enabled);
    assert_eq!(queued.semantic.documents, 0);
    assert_eq!(queued.semantic.chunks, 0);
    assert_eq!(queued.semantic.stale_estimate, 0);

    apply(&service, &write_event(OpKind::Create, vec![path]))?;
    let after_enqueue = service.status()?;
    assert_eq!(
        after_enqueue.semantic.stale_estimate, 1,
        "one path is queued but not yet processed"
    );

    let report = service.rebuild(RebuildTarget::Semantic)?;
    let semantic_report = report
        .semantic
        .ok_or("expected a semantic rebuild report")?;
    assert_eq!(semantic_report.paths_scanned, 1);
    assert_eq!(semantic_report.embedded, 2);
    assert_eq!(semantic_report.skipped, 0);
    assert_eq!(semantic_report.failed, 0);

    let after_rebuild = service.status()?;
    assert_eq!(after_rebuild.semantic.documents, 1);
    assert_eq!(after_rebuild.semantic.chunks, 2);
    assert_eq!(after_rebuild.semantic.stale_estimate, 0);
    assert!(after_rebuild.semantic.last_build.is_some());
    Ok(())
}

#[test]
fn fr_53_fr_55_rebuild_skips_unchanged_chunks_on_a_second_pass()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = vault_dir()?;
    let service = semantic_service_at(&vault)?;
    vault_note(&vault, "note.md", "# Note\n\nStable content.\n")?;

    let first = service.rebuild(RebuildTarget::Semantic)?;
    let first_report = first.semantic.ok_or("expected a semantic rebuild report")?;
    assert_eq!(first_report.embedded, 1);
    assert_eq!(first_report.skipped, 0);

    let second = service.rebuild(RebuildTarget::Semantic)?;
    let second_report = second
        .semantic
        .ok_or("expected a semantic rebuild report")?;
    assert_eq!(second_report.embedded, 0);
    assert_eq!(second_report.skipped, 1);
    Ok(())
}
