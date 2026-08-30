//! FR-53, D-04: the embedding queue and background worker, decoupled from
//! the event stream. Every test drives the worker synchronously
//! (`process_one` / `drain`) through an injected `contextos_core::Clock`,
//! never a real thread or a real sleep.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::Clock;
use contextos_search::{
    Chunk, EmbeddingWorker, EmbeddingWorkerConfig, PathEmbeddingOutcome, ReadsChunkSource,
    SearchError, SqliteVecConfig, SqliteVecStore, StoresVectors,
};
use tempfile::tempdir;
use time::OffsetDateTime;

/// A fixed clock advanced explicitly by the test, never by wall-clock time.
#[derive(Clone)]
struct StepClock {
    tick: std::sync::Arc<AtomicUsize>,
}

impl StepClock {
    fn new() -> Self {
        Self {
            tick: std::sync::Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Clock for StepClock {
    fn now(&self) -> OffsetDateTime {
        let tick = self.tick.fetch_add(1, Ordering::SeqCst);
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(i64::try_from(tick).unwrap_or(0))
    }
}

/// An in-memory content source, so worker tests never need real files:
/// paths map directly to canned markdown text, and a path missing from the
/// map behaves as "deleted".
#[derive(Default)]
struct MapContentSource {
    content: Mutex<BTreeMap<String, String>>,
}

impl MapContentSource {
    fn new(entries: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
        Self {
            content: Mutex::new(
                entries
                    .into_iter()
                    .map(|(path, text)| (path.to_owned(), text.to_owned()))
                    .collect(),
            ),
        }
    }

    fn remove(&self, path: &str) {
        self.content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
    }

    fn set(&self, path: &str, text: &str) {
        self.content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_owned(), text.to_owned());
    }
}

impl ReadsChunkSource for MapContentSource {
    fn read(&self, path: &str) -> Result<Option<String>, SearchError> {
        Ok(self
            .content
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .cloned())
    }
}

/// A deterministic embedder that counts calls per chunk identity, so tests
/// can assert the hash-unchanged skip actually avoided re-embedding.
struct SpyEmbedder {
    calls: Mutex<Vec<(String, usize)>>,
    fail_on: Option<&'static str>,
}

impl SpyEmbedder {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_on: None,
        }
    }

    fn failing_on(path: &'static str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_on: Some(path),
        }
    }

    fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl contextos_search::EmbedsText for SpyEmbedder {
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        if let Some(fail_path) = self.fail_on
            && chunks.iter().any(|chunk| chunk.path() == fail_path)
        {
            return Err(SearchError::EmbeddingTransport {
                endpoint: "spy://fail".to_owned(),
                reason: "forced test failure".to_owned(),
            });
        }
        let mut calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut vectors = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            calls.push((chunk.path().to_owned(), chunk.ordinal()));
            // Deterministic, distinguishable-enough vector per chunk index.
            let component = f32::from(u8::try_from(chunk.ordinal() % 250).unwrap_or(0));
            vectors.push(vec![component, 1.0, 0.0, 0.0]);
        }
        Ok(vectors)
    }

    fn dimension(&self) -> Option<usize> {
        Some(4)
    }
}

fn sqlite_store(directory: &tempfile::TempDir) -> Result<SqliteVecStore, Box<dyn Error>> {
    Ok(SqliteVecStore::try_from(SqliteVecConfig {
        path: directory.path().join("vectors.db"),
        dimension: 4,
    })?)
}

/// A temporary directory whose basename is a valid RFC 3986 scheme token
/// (starts with an ASCII letter), unlike the bare `tempdir()` default,
/// which on this platform yields a leading-dot name. `EmbeddingWorkerConfig`
/// takes a raw root path and derives its `VaultRoot` name from the
/// directory's basename, so the fixture must control that basename here.
fn vault_root_dir() -> Result<tempfile::TempDir, Box<dyn Error>> {
    Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
}

type TestWorker = EmbeddingWorker<SqliteVecStore, SpyEmbedder, MapContentSource, StepClock>;

fn worker(
    root: &tempfile::TempDir,
    db_directory: &tempfile::TempDir,
    embedder: SpyEmbedder,
    content: MapContentSource,
) -> Result<TestWorker, Box<dyn Error>> {
    let store = sqlite_store(db_directory)?;
    Ok(EmbeddingWorker::try_from(EmbeddingWorkerConfig {
        root: root.path().to_path_buf(),
        store,
        embedder,
        content,
        clock: StepClock::new(),
    })?)
}

#[test]
fn new_document_chunks_are_embedded_and_upserted() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("note.md", "# Title\n\nSome prose here.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("note.md");
    let outcome = worker.process_one().ok_or("expected one queued path")?;

    match outcome {
        PathEmbeddingOutcome::Embedded { path, embedded, .. } => {
            assert_eq!(path, "note.md");
            assert!(embedded > 0);
        }
        other => return Err(format!("expected Embedded outcome, got {other:?}").into()),
    }
    assert_eq!(worker.embedder().call_count(), 1);
    Ok(())
}

#[test]
fn hash_unchanged_chunks_are_not_re_embedded() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("note.md", "# Title\n\nSome prose here.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("note.md");
    worker.process_one().ok_or("expected first pass")?;
    let calls_after_first = worker.embedder().call_count();
    assert!(calls_after_first > 0);

    // Re-queue the same, unchanged path.
    worker.enqueue("note.md");
    let outcome = worker.process_one().ok_or("expected second pass")?;
    match outcome {
        PathEmbeddingOutcome::Embedded {
            embedded, skipped, ..
        } => {
            assert_eq!(embedded, 0, "unchanged chunks must not be re-embedded");
            assert!(skipped > 0);
        }
        other => return Err(format!("expected Embedded outcome, got {other:?}").into()),
    }
    assert_eq!(
        worker.embedder().call_count(),
        calls_after_first,
        "embedder must not have been called again"
    );
    Ok(())
}

#[test]
fn changed_content_is_re_embedded() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("note.md", "# Title\n\nOriginal prose.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("note.md");
    worker.process_one().ok_or("expected first pass")?;
    let calls_after_first = worker.embedder().call_count();

    worker
        .content()
        .set("note.md", "# Title\n\nCompletely different prose now.\n");
    worker.enqueue("note.md");
    let outcome = worker.process_one().ok_or("expected second pass")?;
    match outcome {
        PathEmbeddingOutcome::Embedded { embedded, .. } => {
            assert!(embedded > 0, "changed content must be re-embedded");
        }
        other => return Err(format!("expected Embedded outcome, got {other:?}").into()),
    }
    assert!(worker.embedder().call_count() > calls_after_first);
    Ok(())
}

#[test]
fn a_shrunk_document_prunes_trailing_ordinals() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    // Two headings, each with enough prose to force two distinct sections
    // (and so two chunks, ordinals 0 and 1).
    let content = MapContentSource::new([(
        "note.md",
        "# First\n\nFirst section prose.\n\n# Second\n\nSecond section prose.\n",
    )]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("note.md");
    worker.process_one().ok_or("expected first pass")?;
    assert!(worker.store().existing_hash("note.md", 0)?.is_some());
    assert!(worker.store().existing_hash("note.md", 1)?.is_some());

    // The document shrinks to one section: re-chunking now produces only
    // ordinal 0, so ordinal 1's previously stored chunk is now stale.
    worker
        .content()
        .set("note.md", "# First\n\nFirst section prose.\n");
    worker.enqueue("note.md");
    worker.process_one().ok_or("expected second pass")?;

    assert!(worker.store().existing_hash("note.md", 0)?.is_some());
    assert_eq!(
        worker.store().existing_hash("note.md", 1)?,
        None,
        "the shrunk document's trailing ordinal must be pruned, not left orphaned"
    );
    Ok(())
}

#[test]
fn a_deleted_path_removes_its_stored_vectors() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("note.md", "# Title\n\nSome prose here.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("note.md");
    worker.process_one().ok_or("expected first pass to embed")?;

    worker.content().remove("note.md");
    worker.enqueue("note.md");
    let outcome = worker
        .process_one()
        .ok_or("expected second pass to remove")?;
    match outcome {
        PathEmbeddingOutcome::Removed { path } => assert_eq!(path, "note.md"),
        other => return Err(format!("expected Removed outcome, got {other:?}").into()),
    }
    assert_eq!(worker.store().existing_hash("note.md", 0)?, None);
    Ok(())
}

#[test]
fn a_failing_path_does_not_stop_processing_of_other_queued_paths() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([
        ("bad.md", "# Bad\n\nThis path's embedding always fails.\n"),
        ("good.md", "# Good\n\nThis path embeds fine.\n"),
    ]);
    let worker = worker(
        &root,
        &db_directory,
        SpyEmbedder::failing_on("bad.md"),
        content,
    )?;

    worker.enqueue("bad.md");
    worker.enqueue("good.md");

    let outcomes = worker.drain();
    assert_eq!(outcomes.len(), 2);

    let bad_outcome = outcomes
        .iter()
        .find(|outcome| matches!(outcome, PathEmbeddingOutcome::Failed { path, .. } if path == "bad.md"))
        .ok_or("bad.md must surface a Failed outcome, not stop the worker")?;
    if let PathEmbeddingOutcome::Failed { warning, .. } = bad_outcome {
        assert_eq!(warning.code, "embedding/network");
    }

    let good_outcome = outcomes
        .iter()
        .find(|outcome| matches!(outcome, PathEmbeddingOutcome::Embedded { path, .. } if path == "good.md"))
        .ok_or("good.md must still be processed after bad.md failed")?;
    if let PathEmbeddingOutcome::Embedded { embedded, .. } = good_outcome {
        assert!(*embedded > 0);
    }

    let status = worker.status();
    assert_eq!(status.processed, 1);
    assert_eq!(status.failed, 1);
    Ok(())
}

#[test]
fn drain_processes_everything_queued_before_returning() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([
        ("one.md", "# One\n\nFirst note.\n"),
        ("two.md", "# Two\n\nSecond note.\n"),
        ("three.md", "# Three\n\nThird note.\n"),
    ]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("one.md");
    worker.enqueue("two.md");
    worker.enqueue("three.md");

    assert_eq!(worker.status().pending, 3);
    let outcomes = worker.drain();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(worker.status().pending, 0);
    assert_eq!(worker.status().processed, 3);
    Ok(())
}

#[test]
fn drain_until_guarantees_at_least_one_item_then_stops_at_the_budget() -> Result<(), Box<dyn Error>>
{
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([
        ("one.md", "# One\n\nFirst note.\n"),
        ("two.md", "# Two\n\nSecond note.\n"),
        ("three.md", "# Three\n\nThird note.\n"),
    ]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;
    worker.enqueue("one.md");
    worker.enqueue("two.md");
    worker.enqueue("three.md");

    // A zero-second budget still guarantees forward progress: at least one
    // queued path is processed before the deadline is checked, so a caller
    // can never be starved even when a single item's cost already exceeds
    // the whole budget.
    let outcomes = worker.drain_until(time::Duration::ZERO);
    assert_eq!(
        outcomes.len(),
        1,
        "a zero budget must still process exactly one queued path"
    );
    assert_eq!(worker.status().pending, 2);
    Ok(())
}

#[test]
fn drain_until_leaves_the_rest_queued_for_a_later_call_to_finish() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([
        ("one.md", "# One\n\nFirst note.\n"),
        ("two.md", "# Two\n\nSecond note.\n"),
        ("three.md", "# Three\n\nThird note.\n"),
    ]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;
    worker.enqueue("one.md");
    worker.enqueue("two.md");
    worker.enqueue("three.md");

    let first_pass = worker.drain_until(time::Duration::ZERO);
    assert_eq!(first_pass.len(), 1);
    assert_eq!(worker.status().pending, 2);

    // A resumed call against the same worker finishes whatever the first
    // pass left behind, exactly as a second `query_index_rebuild` call
    // against the same long-lived `VaultSearchService` would.
    let second_pass = worker.drain_until(time::Duration::seconds(3600));
    assert_eq!(second_pass.len(), 2);
    assert_eq!(worker.status().pending, 0);
    assert_eq!(worker.status().processed, 3);
    Ok(())
}

#[test]
fn shutdown_closes_the_queue_and_drains_pending_work() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("one.md", "# One\n\nFirst note.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("one.md");
    let outcomes = worker.shutdown();
    assert_eq!(outcomes.len(), 1);

    // The queue is now closed: further enqueue attempts must not silently
    // succeed as if they would be processed.
    assert!(worker.is_closed());
    Ok(())
}

#[test]
fn status_records_last_processed_timestamp_via_the_injected_clock() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("one.md", "# One\n\nFirst note.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    assert_eq!(worker.status().last_processed_at, None);
    worker.enqueue("one.md");
    worker.process_one().ok_or("expected one path processed")?;
    assert!(worker.status().last_processed_at.is_some());
    Ok(())
}

#[test]
fn process_one_returns_none_when_the_queue_is_empty() -> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    assert!(worker.process_one().is_none());
    Ok(())
}

#[test]
fn enqueueing_the_same_path_twice_before_processing_only_queues_it_once()
-> Result<(), Box<dyn Error>> {
    let root = vault_root_dir()?;
    let db_directory = tempdir()?;
    let content = MapContentSource::new([("one.md", "# One\n\nFirst note.\n")]);
    let worker = worker(&root, &db_directory, SpyEmbedder::new(), content)?;

    worker.enqueue("one.md");
    worker.enqueue("one.md");
    assert_eq!(worker.status().pending, 1);
    Ok(())
}
