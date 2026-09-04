use std::path::PathBuf;

use contextos_obsidian::{LinkCollection, ObsidianLink};
use contextos_search::{CatchUpKind, GraphBackend, GraphDirection, GraphEdgeKind, LinkGraph, LinkGraphConfig};

/// Every backend runs the same behaviour suite, which must pass identically
/// under each one.
const BACKENDS: [GraphBackend; 3] = [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite];

/// Builds a fresh `LinkGraph` backed by `dir/.contextos/graph`, under the
/// given `backend`.
fn graph_at(dir: &tempfile::TempDir, backend: GraphBackend) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    let store_directory: PathBuf = dir.path().join(".contextos").join("graph");
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend,
    })?)
}

/// Extracts outgoing wikilinks from a markdown snippet.
fn links(markdown: &str) -> Result<Vec<ObsidianLink>, Box<dyn std::error::Error>> {
    Ok(LinkCollection::try_from(markdown)?.outgoing().to_vec())
}

#[test]
fn neighbours_bounded_by_depth_and_direction() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        // Build the chain from the far end so every link target already
        // exists as a real note when it is wired, avoiding phantom-upgrade
        // mechanics.
        graph.upsert_note("d.md", "D", &[])?;
        graph.upsert_note("c.md", "C", &links("[[d]]\n")?)?;
        graph.upsert_note("b.md", "B", &links("[[c]]\n")?)?;
        graph.upsert_note("a.md", "A", &links("[[b]]\n")?)?;
        graph.upsert_note("e.md", "E", &links("[[a]]\n")?)?;

        let out_one = graph.neighbours("a.md", 1, GraphDirection::Out)?;
        let mut paths: Vec<&str> = out_one.nodes.iter().map(|node| node.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.md", "b.md"], "backend {backend:?}");

        let out_two = graph.neighbours("a.md", 2, GraphDirection::Out)?;
        let paths: Vec<&str> = out_two.nodes.iter().map(|node| node.path.as_str()).collect();
        assert!(paths.contains(&"c.md"), "backend {backend:?}");
        assert!(!paths.contains(&"d.md"), "backend {backend:?}");

        let in_one = graph.neighbours("a.md", 1, GraphDirection::In)?;
        let mut paths: Vec<&str> = in_one.nodes.iter().map(|node| node.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.md", "e.md"], "backend {backend:?}");

        let both_one = graph.neighbours("a.md", 1, GraphDirection::Both)?;
        let mut paths: Vec<&str> = both_one.nodes.iter().map(|node| node.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.md", "b.md", "e.md"], "backend {backend:?}");
        let mut edge_pairs: Vec<(&str, &str)> = both_one
            .edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect();
        edge_pairs.sort_unstable();
        assert_eq!(
            edge_pairs,
            vec![("a.md", "b.md"), ("e.md", "a.md")],
            "backend {backend:?}"
        );
    }

    Ok(())
}

#[test]
fn depth_outside_bounds_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;
        graph.upsert_note("a.md", "A", &[])?;

        let Err(too_shallow) = graph.neighbours("a.md", 0, GraphDirection::Out) else {
            return Err(format!("backend {backend:?}: expected depth 0 to be rejected").into());
        };
        assert_eq!(too_shallow.code(), "index/invalid-query", "backend {backend:?}");

        let Err(too_deep) = graph.neighbours("a.md", 5, GraphDirection::Out) else {
            return Err(format!("backend {backend:?}: expected depth 5 to be rejected").into());
        };
        assert_eq!(too_deep.code(), "index/invalid-query", "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn unknown_note_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        let Err(error) = graph.neighbours("missing.md", 1, GraphDirection::Out) else {
            return Err(format!("backend {backend:?}: expected an unknown note to be rejected").into());
        };
        assert_eq!(error.code(), "path/not-found", "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn backlinks_reports_incoming_kinds() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("b.md", "B", &[])?;
        graph.upsert_note("a.md", "A", &links("[[b]]\n")?)?;
        graph.upsert_note("c.md", "C", &links("![[b]]\n")?)?;

        let view = graph.backlinks("b.md")?;
        let mut edges: Vec<(&str, &str, GraphEdgeKind)> = view
            .edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str(), edge.kind))
            .collect();
        edges.sort_by_key(|(from, _, _)| *from);
        assert_eq!(
            edges,
            vec![
                ("a.md", "b.md", GraphEdgeKind::Link),
                ("c.md", "b.md", GraphEdgeKind::Embed),
            ],
            "backend {backend:?}"
        );
    }

    Ok(())
}

#[test]
fn path_between_finds_shortest_route() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("d.md", "D", &[])?;
        graph.upsert_note("b.md", "B", &links("[[d]]\n")?)?;
        graph.upsert_note("e.md", "E", &links("[[d]]\n")?)?;
        graph.upsert_note("c.md", "C", &links("[[e]]\n")?)?;
        graph.upsert_note("a.md", "A", &links("[[b]]\n[[c]]\n")?)?;
        graph.upsert_note("z.md", "Z", &[])?;

        let view = graph.path_between("a.md", "d.md", GraphDirection::Out)?;
        let mut paths: Vec<&str> = view.nodes.iter().map(|node| node.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["a.md", "b.md", "d.md"], "backend {backend:?}");
        let mut edge_pairs: Vec<(&str, &str)> = view
            .edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect();
        edge_pairs.sort_unstable();
        assert_eq!(
            edge_pairs,
            vec![("a.md", "b.md"), ("b.md", "d.md")],
            "backend {backend:?}"
        );

        let unreachable = graph.path_between("a.md", "z.md", GraphDirection::Out)?;
        assert!(unreachable.nodes.is_empty(), "backend {backend:?}");
        assert!(unreachable.edges.is_empty(), "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn orphans_lists_unlinked_notes_only() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("linked-b.md", "B", &[])?;
        graph.upsert_note("linked-a.md", "A", &links("[[linked-b]]\n")?)?;
        graph.upsert_note("lone.md", "Lone", &[])?;
        graph.upsert_note("has-phantom.md", "Has Phantom", &links("[[ghost]]\n")?)?;

        let view = graph.orphans()?;
        let paths: Vec<&str> = view.nodes.iter().map(|node| node.path.as_str()).collect();
        assert_eq!(paths, vec!["lone.md"], "backend {backend:?}");
        assert!(view.edges.is_empty(), "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn unresolved_link_becomes_phantom_then_resolves() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("a.md", "A", &links("[[ghost]]\n")?)?;

        let view = graph.neighbours("a.md", 1, GraphDirection::Out)?;
        let Some(ghost) = view.nodes.iter().find(|node| node.path == "ghost") else {
            return Err(format!("backend {backend:?}: expected a phantom node for the unresolved target").into());
        };
        assert!(ghost.phantom, "backend {backend:?}");
        assert_eq!(
            graph.unresolved_targets("a.md")?,
            vec!["ghost".to_owned()],
            "backend {backend:?}"
        );

        graph.upsert_note("ghost", "Ghost", &[])?;

        let view = graph.neighbours("a.md", 1, GraphDirection::Out)?;
        let Some(ghost) = view.nodes.iter().find(|node| node.path == "ghost") else {
            return Err(format!("backend {backend:?}: expected the resolved ghost node to remain reachable").into());
        };
        assert!(!ghost.phantom, "backend {backend:?}");
        assert!(graph.unresolved_targets("a.md")?.is_empty(), "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn bare_name_resolves_to_nested_note() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("notes/alpha.md", "Alpha", &[])?;
        graph.upsert_note("b.md", "B", &links("[[alpha]]\n")?)?;

        let view = graph.neighbours("b.md", 1, GraphDirection::Out)?;
        let mut paths: Vec<&str> = view.nodes.iter().map(|node| node.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["b.md", "notes/alpha.md"], "backend {backend:?}");
        assert!(view.nodes.iter().all(|node| !node.phantom), "backend {backend:?}");
        assert_eq!(view.edges.len(), 1, "backend {backend:?}");
        assert_eq!(view.edges[0].from, "b.md", "backend {backend:?}");
        assert_eq!(view.edges[0].to, "notes/alpha.md", "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn upsert_replaces_previous_outgoing_links() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("b.md", "B", &[])?;
        graph.upsert_note("c.md", "C", &[])?;
        graph.upsert_note("a.md", "A", &links("[[b]]\n")?)?;

        let before = graph.backlinks("b.md")?;
        assert_eq!(before.edges.len(), 1, "backend {backend:?}");

        graph.upsert_note("a.md", "A", &links("[[c]]\n")?)?;

        let after_b = graph.backlinks("b.md")?;
        assert!(after_b.edges.is_empty(), "backend {backend:?}");
        let after_c = graph.backlinks("c.md")?;
        assert_eq!(after_c.edges.len(), 1, "backend {backend:?}");
        assert_eq!(after_c.edges[0].from, "a.md", "backend {backend:?}");
    }

    Ok(())
}

#[test]
fn remove_note_keeps_phantom_when_referenced() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;

        graph.upsert_note("b.md", "B", &[])?;
        graph.upsert_note("a.md", "A", &links("[[b]]\n")?)?;

        graph.remove_note("b.md")?;

        let view = graph.neighbours("a.md", 1, GraphDirection::Out)?;
        let Some(b_node) = view.nodes.iter().find(|node| node.path == "b.md") else {
            return Err(format!("backend {backend:?}: expected the phantom b.md node to remain reachable").into());
        };
        assert!(b_node.phantom, "backend {backend:?}");

        graph.remove_note("a.md")?;

        let orphans = graph.orphans()?;
        assert!(orphans.nodes.is_empty(), "backend {backend:?}");
        let Err(error) = graph.neighbours("b.md", 1, GraphDirection::Out) else {
            return Err(format!("backend {backend:?}: expected the pruned phantom b.md node to be unknown").into());
        };
        assert_eq!(error.code(), "path/not-found", "backend {backend:?}");
    }

    Ok(())
}

/// `StableDiGraph` permits parallel edges, and `wire_links` adds one edge
/// per parsed `ObsidianLink`, so a note that links to the same target twice
/// legitimately produces two distinct `Link` edges (matching Obsidian's own
/// treatment: it renders such a note as linking to that target twice). A
/// key-value persistence layer keyed only by `(from, to, kind)` would
/// collapse both writes into one record and silently lose the second edge
/// on reopen; this test targets exactly that failure mode, for every
/// backend.
#[test]
fn parallel_links_survive_persistence_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    for backend in BACKENDS {
        let dir = tempfile::tempdir()?;
        let store_directory = dir.path().join(".contextos").join("graph");

        {
            let mut graph = LinkGraph::try_from(LinkGraphConfig {
                store_directory: store_directory.clone(),
                backend,
            })?;
            let notes = vec![
                ("b.md".to_owned(), "B".to_owned(), Vec::new()),
                ("a.md".to_owned(), "A".to_owned(), links("[[b]]\n[[b]]\n")?),
            ];
            graph.rebuild(&notes)?;
        }

        let mut reopened = LinkGraph::try_from(LinkGraphConfig {
            store_directory: store_directory.clone(),
            backend,
        })?;
        assert!(!reopened.needs_rebuild(), "backend {backend:?}");
        let view = reopened.neighbours("a.md", 1, GraphDirection::Out)?;
        let link_edges: Vec<_> = view
            .edges
            .iter()
            .filter(|edge| edge.from == "a.md" && edge.to == "b.md")
            .collect();
        assert_eq!(
            link_edges.len(),
            2,
            "backend {backend:?}: expected both parallel [[b]] links to survive a persist/reopen \
             round trip, got {link_edges:?}"
        );
    }

    Ok(())
}

/// `fjall`-specific: exercises the "an incompatible stored format forces a
/// full rebuild" contract by writing a `format_version` the running
/// `LinkGraph` does not recognise directly into the metadata keyspace,
/// using `fjall` itself rather than any `LinkGraph` API. Kept backend-
/// specific because simulating on-disk corruption this way is necessarily
/// backend-specific; `fr_108_switching_backend_flags_rebuild_without_data_loss`
/// below covers the same "untrustworthy store yields a rebuild flag, not an
/// error" contract in a way that is backend-agnostic.
#[test]
fn store_round_trips_and_flags_rebuild() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    {
        let mut graph = LinkGraph::try_from(LinkGraphConfig {
            store_directory: store_directory.clone(),
            backend: GraphBackend::Fjall,
        })?;
        assert!(graph.needs_rebuild());

        let notes = vec![
            ("b.md".to_owned(), "B".to_owned(), Vec::new()),
            ("a.md".to_owned(), "A".to_owned(), links("[[b]]\n")?),
        ];
        graph.rebuild(&notes)?;
        assert!(!graph.needs_rebuild());
    }

    let mut reopened = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Fjall,
    })?;
    assert!(!reopened.needs_rebuild());
    let view = reopened.neighbours("a.md", 1, GraphDirection::Out)?;
    let mut paths: Vec<&str> = view.nodes.iter().map(|node| node.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["a.md", "b.md"]);
    drop(reopened);

    // Simulate an incompatible store: write a `format_version` the running
    // `LinkGraph` does not recognise directly into the metadata keyspace,
    // using `fjall` itself rather than any `LinkGraph` API. This exercises
    // the same "format id changes forces a full rebuild" contract the
    // former JSON cache's `CACHE_FORMAT` bump provided.
    {
        let database = fjall::Database::builder(&store_directory).open()?;
        let metadata = database.keyspace("metadata", fjall::KeyspaceCreateOptions::default)?;
        let mut batch = database.batch().durability(Some(fjall::PersistMode::SyncAll));
        batch.insert(&metadata, "format_version", 999u32.to_be_bytes());
        batch.commit()?;
    }
    let mut corrupt = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Fjall,
    })?;
    assert!(corrupt.needs_rebuild());
    assert!(corrupt.orphans()?.nodes.is_empty());

    let fresh_dir = tempfile::tempdir()?;
    let fresh = graph_at(&fresh_dir, GraphBackend::Fjall)?;
    assert!(fresh.needs_rebuild());

    Ok(())
}

/// Switching a vault's configured `graph_backend` between runs performs no
/// migration. Each backend lives at its own location under the shared
/// store directory (`fjall`'s own directory contents, `sqlite`'s
/// `graph.sqlite3`, `serde`'s `graph.json`), so reopening under a different
/// backend simply finds nothing there yet and comes up flagged for rebuild,
/// the same recoverable-derived-state contract a corrupted or deleted store
/// already has; it is never an error, and it never silently returns a
/// partial graph.
#[test]
fn switching_backend_flags_rebuild_without_data_loss() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    {
        let mut fjall_graph = LinkGraph::try_from(LinkGraphConfig {
            store_directory: store_directory.clone(),
            backend: GraphBackend::Fjall,
        })?;
        let notes = vec![("a.md".to_owned(), "A".to_owned(), Vec::new())];
        fjall_graph.rebuild(&notes)?;
        assert!(!fjall_graph.needs_rebuild());
    }

    // Reopening the same directory under `sqlite` finds no `graph.sqlite3`
    // there yet: flagged for rebuild, not an error, and not a crash.
    let mut sqlite_graph = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    assert!(sqlite_graph.needs_rebuild());
    assert!(sqlite_graph.orphans()?.nodes.is_empty());

    // Rebuilding under the new backend recovers a working graph.
    let notes = vec![("z.md".to_owned(), "Z".to_owned(), Vec::new())];
    sqlite_graph.rebuild(&notes)?;
    assert!(!sqlite_graph.needs_rebuild());
    drop(sqlite_graph);

    let mut reopened_sqlite = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    assert!(!reopened_sqlite.needs_rebuild());
    assert_eq!(reopened_sqlite.orphans()?.nodes.len(), 1);
    drop(reopened_sqlite);

    // The original `fjall` store, sharing the same directory throughout,
    // was never touched by the `sqlite` backend's opens, rebuild, or
    // writes: switching back finds it exactly as it was left.
    let mut reopened_fjall = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Fjall,
    })?;
    assert!(!reopened_fjall.needs_rebuild());
    let view = reopened_fjall.neighbours("a.md", 1, GraphDirection::Out)?;
    assert_eq!(view.nodes.len(), 1);
    assert_eq!(view.nodes[0].path, "a.md");

    Ok(())
}

/// The whole reason `sqlite` exists as a backend option: opening two
/// independent `LinkGraph` instances against the same
/// `sqlite`-backed store at once both succeed, unlike `fjall`, whose
/// single-process-exclusive lock rejects the second with
/// `SearchError::GraphLocked` (proven at the service layer by
/// `graph_store_locked_by_another_instance_degrades_to_disabled` in
/// `vault_search_service.rs`; this test proves the positive case at the
/// `LinkGraph` layer directly).
#[test]
fn two_sqlite_backed_instances_open_concurrently() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut first = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let second = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;

    first.upsert_note("a.md", "A", &[])?;
    drop(first);
    drop(second);

    let mut reopened = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;
    assert!(!reopened.needs_rebuild());
    assert_eq!(reopened.neighbours("a.md", 1, GraphDirection::Out)?.nodes.len(), 1);

    Ok(())
}

/// Regression test for a genuine concurrency bug the fixed-scale
/// benchmark (`benches/graph_persistence.rs`) found and
/// `fr_108_two_sqlite_backed_instances_open_concurrently` above did not:
/// that test opens its two instances sequentially, with the second opened
/// only after the first has already fully completed its own open. Two
/// threads racing to open a brand-new `sqlite`-backed store at the same
/// instant, each writing immediately, used to fail with `SQLITE_BUSY`
/// ("database is locked"): `SqliteGraphStore::open`'s `busy_timeout` did
/// not reliably cover the specific race of two connections both switching
/// a brand-new database file to WAL mode and creating its schema at once.
/// Fixed by retrying the whole open-and-initialise sequence, not just a
/// single statement, on that specific error.
#[test]
fn two_sqlite_backed_instances_open_and_write_under_genuine_concurrency() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

    let first_directory = store_directory.clone();
    let first_barrier = std::sync::Arc::clone(&barrier);
    let first_thread = std::thread::spawn(move || -> Result<(), String> {
        first_barrier.wait();
        let mut graph = LinkGraph::try_from(LinkGraphConfig {
            store_directory: first_directory,
            backend: GraphBackend::Sqlite,
        })
        .map_err(|error| error.to_string())?;
        graph.upsert_note("a.md", "A", &[]).map_err(|error| error.to_string())
    });

    let second_directory = store_directory.clone();
    let second_barrier = std::sync::Arc::clone(&barrier);
    let second_thread = std::thread::spawn(move || -> Result<(), String> {
        second_barrier.wait();
        let mut graph = LinkGraph::try_from(LinkGraphConfig {
            store_directory: second_directory,
            backend: GraphBackend::Sqlite,
        })
        .map_err(|error| error.to_string())?;
        graph.upsert_note("b.md", "B", &[]).map_err(|error| error.to_string())
    });

    first_thread.join().map_err(|_| "first thread panicked")??;
    second_thread.join().map_err(|_| "second thread panicked")??;

    let mut reopened = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;
    let mut paths: Vec<String> = reopened.full_view()?.nodes.into_iter().map(|node| node.path).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["a.md".to_owned(), "b.md".to_owned()]);

    Ok(())
}

/// A sibling `sqlite`-backed instance observes another instance's
/// persisted change on its own next call, without reopening. Both
/// instances being able to hold the store open only fixed availability;
/// this is the first proof of cross-instance *consistency*.
#[test]
fn a_sibling_sqlite_instance_observes_a_change_without_reopening() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut writer = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;

    // `reader` sees nothing yet: it opened before `writer` persisted.
    assert!(reader.orphans()?.nodes.is_empty());

    writer.upsert_note("a.md", "A", &[])?;

    // No reopen: `reader` is the same long-lived instance from before the
    // write, exactly the general-connector-instance-plus-per-task-instance
    // shape this store must support.
    let view = reader.neighbours("a.md", 1, GraphDirection::Out)?;
    assert_eq!(view.nodes.len(), 1);
    assert_eq!(view.nodes[0].path, "a.md");
    assert_eq!(
        reader.sync_status().and_then(|status| status.last_catch_up),
        Some(CatchUpKind::Partial),
        "one note, well within the retention window: a partial delta, not a full reload"
    );

    Ok(())
}

/// A removal (not just an upsert) propagates to a sibling instance,
/// the case a naive "diff the current rows" approach would miss, since a
/// plain SQL `DELETE` leaves nothing to diff against; `changelog` rows exist
/// specifically so a removal has its own durable record.
#[test]
fn a_removal_propagates_to_a_sibling_instance() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut writer = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;

    writer.upsert_note("a.md", "A", &[])?;
    assert_eq!(reader.orphans()?.nodes.len(), 1, "reader catches up first");

    writer.remove_note("a.md")?;
    assert!(
        reader.orphans()?.nodes.is_empty(),
        "reader observes the removal too, not just the earlier upsert"
    );

    Ok(())
}

/// A `rebuild` (which clears the store directly, `StoresGraph::clear`,
/// bypassing the ordinary upsert/remove vocabulary) forces a sibling's
/// next catch-up to a full reload rather than a stale partial merge of
/// old-plus-new content. Without the `Reset` marker this would fail
/// differently: the reader would still show `old.md` (never told it was
/// dropped) alongside whatever the rebuild added.
#[test]
fn a_rebuild_forces_a_sibling_to_a_full_reload_not_a_stale_merge() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut writer = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;

    writer.upsert_note("old.md", "Old", &[])?;
    assert_eq!(reader.orphans()?.nodes.len(), 1, "reader catches up first");

    writer.rebuild(&[("new.md".to_owned(), "New".to_owned(), Vec::new())])?;

    let paths: Vec<String> = reader.orphans()?.nodes.into_iter().map(|node| node.path).collect();
    assert_eq!(
        paths,
        vec!["new.md".to_owned()],
        "reader reflects exactly the rebuilt state, old.md genuinely gone, not lingering \
         alongside new.md from a stale partial merge"
    );
    assert_eq!(
        reader.sync_status().and_then(|status| status.last_catch_up),
        Some(CatchUpKind::FullReload),
        "the Reset marker forced the fallback path, not a partial delta"
    );

    Ok(())
}

/// Mutations catch up before applying their own local change, not
/// just queries. Without this, `reader`'s local mutation below would
/// operate on stale in-memory state and its own subsequent read would still
/// miss `writer`'s already-persisted note.
#[test]
fn a_local_mutation_catches_up_before_applying_its_own_change() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut writer = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;

    writer.upsert_note("shared.md", "Shared", &[])?;

    // A local mutation on `reader`, not a query: this alone should be
    // enough to observe `writer`'s already-persisted note.
    reader.upsert_note("mine.md", "Mine", &[])?;

    let mut paths: Vec<String> = reader.orphans()?.nodes.into_iter().map(|node| node.path).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["mine.md".to_owned(), "shared.md".to_owned()]);

    Ok(())
}

/// A sibling that has fallen further behind than the changelog's
/// retention window still ends up correct, via the full-reload fallback,
/// never an error and never a silently incomplete graph. `10_001` must
/// stay above `sqlite_store.rs`'s own private `RETENTION_GENERATIONS`
/// (`10_000`): there is no public constant to reference directly, since the
/// retention window is an internal tuning knob, not part of this crate's
/// public contract.
#[test]
fn a_sibling_past_the_retention_window_falls_back_to_a_full_reload() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut writer = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory,
        backend: GraphBackend::Sqlite,
    })?;

    writer.upsert_note("original.md", "Original", &[])?;
    // Force `writer`'s generation counter well past `reader`'s last-seen
    // value and past the retention window, without ever letting `reader`
    // check in during the process (a long-idle sibling, the case the
    // fallback exists for).
    for i in 0..10_001u32 {
        writer.upsert_note(&format!("churn/{i}.md"), "Churn", &[])?;
    }

    let view = reader.neighbours("original.md", 1, GraphDirection::Out)?;
    assert_eq!(view.nodes.len(), 1, "still correct, via the fallback");
    assert_eq!(
        reader.sync_status().and_then(|status| status.last_catch_up),
        Some(CatchUpKind::FullReload),
        "too far behind for a partial delta to be provably complete"
    );

    Ok(())
}

/// `fjall` and `serde` are deliberately unaffected by propagation
/// (`fjall`'s exclusive lock means a sibling can never be concurrently
/// open; `serde` already loses data outright under concurrent instances,
/// a different problem not solved here). `sync_status` reports `None`
/// for both, confirmed directly rather than assumed from the design alone.
#[test]
fn fjall_and_serde_report_no_sync_status() -> Result<(), Box<dyn std::error::Error>> {
    for backend in [GraphBackend::Fjall, GraphBackend::Serde] {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;
        graph.upsert_note("a.md", "A", &[])?;
        assert_eq!(
            graph.sync_status(),
            None,
            "backend {backend:?}: propagation status is meaningless for this backend"
        );
    }

    Ok(())
}

/// Child process for
/// `fr_110_genuine_two_process_propagation_observed_by_an_already_open_sibling`
/// below: a no-op under a normal `cargo test` run (the env vars it needs are
/// absent), doing real work only when spawned explicitly as a genuinely
/// separate OS process, the same pattern `contextos-fs/tests/mutate.rs`'s
/// `fr_14_trash_delete_child`/`nfr_03_process_kill_child` already establish
/// in this workspace.
#[test]
fn genuine_two_process_child_writer() -> Result<(), Box<dyn std::error::Error>> {
    let (Some(store_directory), Some(note_path)) = (
        std::env::var_os("CONTEXTOS_PROPAGATION_TEST_DIR"),
        std::env::var_os("CONTEXTOS_PROPAGATION_TEST_NOTE"),
    ) else {
        return Ok(());
    };
    let mut graph = LinkGraph::try_from(LinkGraphConfig {
        store_directory: PathBuf::from(store_directory),
        backend: GraphBackend::Sqlite,
    })?;
    graph.upsert_note(&note_path.to_string_lossy(), "Child", &[])?;
    Ok(())
}

/// Closing the one gap flagged elsewhere as "what this benchmark does not
/// establish": every
/// other propagation test in this file uses two `LinkGraph` instances
/// within one OS process (threads or plain sequential opens), a faithful
/// proxy since `sqlite`'s WAL locking is connection-scoped, not
/// thread-scoped, but a proxy nonetheless. This test uses a genuinely
/// separate OS process (`Command::new(std::env::current_exe())`, real
/// process boundary, no shared address space) as the writer, and confirms
/// the parent test's own already-open `LinkGraph` — the long-lived
/// "general connector instance" role in the motivating Cowork scenario —
/// observes that external process's write on its own next call, with no
/// reopen.
#[test]
fn genuine_two_process_propagation_observed_by_an_already_open_sibling() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store_directory = dir.path().join(".contextos").join("graph");

    let mut reader = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.clone(),
        backend: GraphBackend::Sqlite,
    })?;
    assert!(
        reader.orphans()?.nodes.is_empty(),
        "reader opened before the external process wrote anything"
    );

    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "genuine_two_process_child_writer"])
        .env("CONTEXTOS_PROPAGATION_TEST_DIR", &store_directory)
        .env("CONTEXTOS_PROPAGATION_TEST_NOTE", "from-another-process.md")
        .status()?;
    assert!(status.success(), "child writer process exited cleanly");

    // No reopen: `reader` is the exact same `LinkGraph` instance from
    // before the child process ran, in this same OS process throughout.
    // The write that just happened came from a genuinely different
    // process, not a thread sharing this one's address space.
    let view = reader.orphans()?;
    assert_eq!(
        view.nodes.len(),
        1,
        "reader observed a write from a genuinely separate OS process, without reopening"
    );
    assert_eq!(view.nodes[0].path, "from-another-process.md");

    Ok(())
}
