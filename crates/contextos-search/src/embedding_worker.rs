//! The embedding queue and background worker, decoupled from the
//! write-pipeline event stream.
//!
//! `EmbeddingWorker` accepts "this path changed" signals through
//! [`EmbeddingWorker::enqueue`] and processes them one at a time through
//! [`EmbeddingWorker::process_one`] or exhaustively through
//! [`EmbeddingWorker::drain`]. Processing chunks the path's current content
//! (via [`crate::chunk_document`]), skips any chunk whose content hash is
//! unchanged since the last build (via [`StoresVectors::existing_hash`]),
//! embeds only new or changed chunks (via the injected [`EmbedsText`]), and
//! upserts them (via the injected [`StoresVectors`]).
//!
//! This module deliberately stops short of wiring a real scheduling loop or
//! the write pipeline's actual `OperationEvent` stream: those are a later
//! composition-root concern (Stage 4 and beyond). Every method here is
//! synchronous and driven explicitly by the caller: a test calling
//! `process_one`/`drain` directly today, and eventually a real background
//! task that calls the same methods on a timer or in response to real
//! events. Determinism therefore needs no scheduler abstraction beyond the
//! already-established [`contextos_core::Clock`]: timestamps come from the
//! injected clock, and *when* to call `process_one` is left to the caller,
//! exactly as the plan's "do not wire into contextos-mcp's real event
//! routing yet" instruction requires.
//!
//! A single path's provider or store failure never stops the worker or
//! loses other queued paths: [`EmbeddingWorker::process_path`] failures
//! surface as a [`PathEmbeddingOutcome::Failed`] carrying a typed
//! [`contextos_core::OperationWarning`] (reusing the same
//! `From<SearchError> for OperationWarning` conversion the rest of this
//! crate's services use), never a panic and never a dropped queue entry.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use contextos_core::{Clock, OperationWarning, VaultPath, VaultPathInput, VaultRoot, VaultSet};
use time::OffsetDateTime;

use crate::{Chunk, ChunkSource, EmbedsText, SearchError, StoresVectors, VectorRecord, chunk_document};

/// Port for reading a queued path's current markdown content, injected so
/// worker tests need not touch real files (though a real filesystem
/// implementation, [`FilesystemChunkSource`], is also provided and matches
/// this workspace's preference for real integration-test filesystems).
pub trait ReadsChunkSource: Send + Sync {
    /// Returns the current content at `path` (forward-slash relative to the
    /// vault root), or `None` when the file no longer exists: the signal
    /// the worker uses to delete stored vectors for a removed path instead
    /// of embedding it.
    ///
    /// # Errors
    ///
    /// Returns a typed [`SearchError`] when the path exists but cannot be
    /// read.
    fn read(&self, path: &str) -> Result<Option<String>, SearchError>;
}

/// Real filesystem-backed [`ReadsChunkSource`]: `path` is joined onto
/// `root` and read as UTF-8 text. A missing file is reported as `None`,
/// matching [`ReadsChunkSource::read`]'s "removed" signal; any other I/O
/// failure is a typed error.
pub struct FilesystemChunkSource {
    root: PathBuf,
}

impl FilesystemChunkSource {
    /// Constructs a reader rooted at `root` (the vault root).
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl ReadsChunkSource for FilesystemChunkSource {
    fn read(&self, path: &str) -> Result<Option<String>, SearchError> {
        let absolute = self.root.join(path);
        match std::fs::read_to_string(&absolute) {
            Ok(content) => Ok(Some(content)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SearchError::DocumentRead {
                path: path.to_owned(),
                source,
            }),
        }
    }
}

/// Outcome of processing one queued path.
#[derive(Clone, Debug, PartialEq)]
pub enum PathEmbeddingOutcome {
    /// The path's content was read, chunked, and any new or changed chunks
    /// were embedded and upserted.
    Embedded {
        path: String,
        /// Chunks embedded because they were new or their content hash had
        /// changed.
        embedded: usize,
        /// Chunks skipped because their content hash matched what was
        /// already stored.
        skipped: usize,
    },
    /// The path no longer exists; every chunk previously stored for it was
    /// deleted.
    Removed { path: String },
    /// Reading, chunking, embedding, or storing failed for this path. Other
    /// queued paths are unaffected: this outcome ends processing of `path`
    /// only.
    Failed { path: String, warning: OperationWarning },
}

/// Staleness-visibility counters for a Stage 4 `query_index_status` to
/// report from, without this crate building that tool itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbeddingWorkerStatus {
    /// Distinct paths currently queued, not yet processed.
    pub pending: usize,
    /// Paths successfully processed (embedded or removed) since
    /// construction.
    pub processed: usize,
    /// Paths whose processing failed since construction.
    pub failed: usize,
    /// When the last `process_one` call completed, per the injected
    /// [`Clock`]. `None` until the first path has been processed.
    pub last_processed_at: Option<OffsetDateTime>,
}

/// Trusted construction input for [`EmbeddingWorker`].
pub struct EmbeddingWorkerConfig<S, E, R, C> {
    /// Resolved filesystem path of the vault root. Used only to
    /// build the throwaway [`VaultPath`]s [`chunk_document`] requires; the
    /// worker does not otherwise touch the filesystem itself (content comes
    /// from `content`).
    pub root: PathBuf,
    pub store: S,
    pub embedder: E,
    pub content: R,
    pub clock: C,
}

/// FIFO of distinct pending path signals, de-duplicated: enqueuing a path
/// already queued (and not yet dequeued) is a no-op, so a burst of change
/// signals for the same path before it is processed collapses to one
/// processing pass over its latest content.
#[derive(Default)]
struct QueueState {
    order: VecDeque<String>,
    members: BTreeSet<String>,
    closed: bool,
}

/// Embedding queue and background worker.
pub struct EmbeddingWorker<S, E, R, C> {
    roots: VaultSet,
    store: S,
    embedder: E,
    content: R,
    clock: C,
    queue: Mutex<QueueState>,
    stats: Mutex<EmbeddingWorkerStatus>,
}

impl<S, E, R, C> std::fmt::Debug for EmbeddingWorker<S, E, R, C> {
    /// Reports only the queue and processing counters: `S`, `E`, `R`, and
    /// `C` are not required to implement `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingWorker")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl<S, E, R, C> TryFrom<EmbeddingWorkerConfig<S, E, R, C>> for EmbeddingWorker<S, E, R, C> {
    type Error = SearchError;

    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingPathInvalid`] when `root` cannot be
    /// resolved to an existing directory.
    fn try_from(value: EmbeddingWorkerConfig<S, E, R, C>) -> Result<Self, Self::Error> {
        let roots = resolve_single_root_set(&value.root)?;
        Ok(Self {
            roots,
            store: value.store,
            embedder: value.embedder,
            content: value.content,
            clock: value.clock,
            queue: Mutex::new(QueueState::default()),
            stats: Mutex::new(EmbeddingWorkerStatus::default()),
        })
    }
}

impl<S, E, R, C> EmbeddingWorker<S, E, R, C> {
    /// Returns the injected vector store.
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Returns the injected embedding provider.
    pub const fn embedder(&self) -> &E {
        &self.embedder
    }

    /// Returns the injected content source.
    pub const fn content(&self) -> &R {
        &self.content
    }

    /// Queues `path` for processing, unless it is already queued or the
    /// queue has been closed by [`EmbeddingWorker::shutdown`] (in which
    /// case the signal is dropped: no more work is accepted after a
    /// graceful shutdown has begun).
    pub fn enqueue(&self, path: impl Into<String>) {
        let path = path.into();
        let mut queue = self.locked_queue();
        if queue.closed {
            return;
        }
        if queue.members.insert(path.clone()) {
            queue.order.push_back(path);
        }
    }

    /// Reports whether [`EmbeddingWorker::shutdown`] has closed the queue.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.locked_queue().closed
    }

    /// Returns the current staleness-visibility counters.
    #[must_use]
    pub fn status(&self) -> EmbeddingWorkerStatus {
        let mut status = self.locked_stats().clone();
        status.pending = self.locked_queue().order.len();
        status
    }

    fn locked_queue(&self) -> MutexGuard<'_, QueueState> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn locked_stats(&self) -> MutexGuard<'_, EmbeddingWorkerStatus> {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn dequeue(&self) -> Option<String> {
        let mut queue = self.locked_queue();
        let path = queue.order.pop_front()?;
        queue.members.remove(&path);
        Some(path)
    }
}

impl<S, E, R, C> EmbeddingWorker<S, E, R, C>
where
    S: StoresVectors,
    E: EmbedsText,
    R: ReadsChunkSource,
    C: Clock,
{
    /// Processes one queued path, if any, and records the outcome in
    /// [`EmbeddingWorker::status`]. Returns `None` when the queue is empty.
    ///
    /// A processing failure for this path is captured as
    /// [`PathEmbeddingOutcome::Failed`], never propagated as an error or a
    /// panic: the caller decides what to do with a failed path (log it,
    /// retry by enqueuing it again, surface it as an
    /// [`contextos_core::OperationWarning`]).
    pub fn process_one(&self) -> Option<PathEmbeddingOutcome> {
        let path = self.dequeue()?;
        let outcome = self.process_path(&path);
        self.record_outcome(&outcome);
        Some(outcome)
    }

    /// Repeatedly calls [`EmbeddingWorker::process_one`] until the queue is
    /// empty, returning every outcome in processing order. One path's
    /// failure does not stop this loop: every other queued path is still
    /// attempted.
    pub fn drain(&self) -> Vec<PathEmbeddingOutcome> {
        let mut outcomes = Vec::new();
        while let Some(outcome) = self.process_one() {
            outcomes.push(outcome);
        }
        outcomes
    }

    /// As [`Self::drain`], but stops once `budget` has elapsed since the
    /// call began, leaving anything still queued for a later call to pick
    /// up: the mechanism a caller with a bounded request window (an MCP
    /// tool call that must return before its own client's timeout) uses to
    /// make partial progress and resume next time, rather than either
    /// blocking past that window or losing queued work. Always processes at
    /// least one queued path before the budget is checked, so a budget
    /// shorter than a single path's processing time still guarantees
    /// forward progress instead of returning empty-handed forever.
    pub fn drain_until(&self, budget: time::Duration) -> Vec<PathEmbeddingOutcome> {
        let deadline = self.clock.now() + budget;
        let mut outcomes = Vec::new();
        while let Some(outcome) = self.process_one() {
            outcomes.push(outcome);
            if self.clock.now() >= deadline {
                break;
            }
        }
        outcomes
    }

    /// Models graceful shutdown: closes the queue to new
    /// [`EmbeddingWorker::enqueue`] calls, then drains every path already
    /// queued before returning, so a shutdown never silently drops pending
    /// embedding work.
    pub fn shutdown(&self) -> Vec<PathEmbeddingOutcome> {
        self.locked_queue().closed = true;
        self.drain()
    }

    fn record_outcome(&self, outcome: &PathEmbeddingOutcome) {
        let mut stats = self.locked_stats();
        stats.last_processed_at = Some(self.clock.now());
        match outcome {
            PathEmbeddingOutcome::Failed { .. } => stats.failed = stats.failed.saturating_add(1),
            PathEmbeddingOutcome::Embedded { .. } | PathEmbeddingOutcome::Removed { .. } => {
                stats.processed = stats.processed.saturating_add(1);
            }
        }
    }

    fn process_path(&self, path: &str) -> PathEmbeddingOutcome {
        match self.content.read(path) {
            Ok(None) => match self.store.delete(path) {
                Ok(()) => PathEmbeddingOutcome::Removed { path: path.to_owned() },
                Err(error) => Self::failed(path, error),
            },
            Ok(Some(content)) => self.embed_path(path, &content),
            Err(error) => Self::failed(path, error),
        }
    }

    fn embed_path(&self, path: &str, content: &str) -> PathEmbeddingOutcome {
        let vault_path = match VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: path,
        }) {
            Ok(vault_path) => vault_path,
            Err(source) => {
                return Self::failed(
                    path,
                    SearchError::EmbeddingPathInvalid {
                        path: path.to_owned(),
                        source,
                    },
                );
            }
        };
        let chunks = chunk_document(ChunkSource {
            path: &vault_path,
            content,
        });

        let mut to_embed: Vec<Chunk> = Vec::new();
        let mut skipped = 0_usize;
        for chunk in &chunks {
            match self.store.existing_hash(chunk.path(), chunk.ordinal()) {
                Ok(Some(existing)) if &existing == chunk.content_hash() => {
                    skipped = skipped.saturating_add(1);
                }
                Ok(_) => to_embed.push(chunk.clone()),
                Err(error) => return Self::failed(path, error),
            }
        }

        let embedded = to_embed.len();
        if !to_embed.is_empty() {
            let vectors = match self.embedder.embed(&to_embed) {
                Ok(vectors) => vectors,
                Err(error) => return Self::failed(path, error),
            };
            if vectors.len() != to_embed.len() {
                return Self::failed(
                    path,
                    SearchError::EmbeddingShapeMismatch {
                        reason: format!(
                            "embedder returned {} vectors for {} chunks",
                            vectors.len(),
                            to_embed.len()
                        ),
                    },
                );
            }
            let records: Vec<VectorRecord<'_>> = to_embed
                .iter()
                .zip(vectors.iter())
                .map(|(chunk, vector)| VectorRecord {
                    path: chunk.path(),
                    ordinal: chunk.ordinal(),
                    heading_context: chunk.heading_context(),
                    content_hash: chunk.content_hash(),
                    vector,
                })
                .collect();
            if let Err(error) = self.store.upsert(&records) {
                return Self::failed(path, error);
            }
        }

        // A shrunk document (fewer chunks than its previous embedding)
        // must not leave trailing ordinals from the longer version behind
        // as stale, orphaned rows: prune anything at or beyond the current
        // chunk count. Harmless, and typically a no-op, when the chunk
        // count grew or stayed the same.
        if let Err(error) = self.store.prune_ordinals_at_or_beyond(path, chunks.len()) {
            return Self::failed(path, error);
        }

        PathEmbeddingOutcome::Embedded {
            path: path.to_owned(),
            embedded,
            skipped,
        }
    }

    fn failed(path: &str, error: SearchError) -> PathEmbeddingOutcome {
        PathEmbeddingOutcome::Failed {
            path: path.to_owned(),
            warning: OperationWarning::from(error),
        }
    }
}

/// Builds a single-root `VaultSet` from `root`, used to construct the
/// throwaway `VaultPath`s `chunk_document` requires. Mirrors
/// `service::resolve_single_root_set` and `sync::resolve_single_root_set`,
/// duplicated here rather than shared, matching this crate's established
/// convention of keeping each already-delivered module unchanged.
fn resolve_single_root_set(root: &Path) -> Result<VaultSet, SearchError> {
    let vault_root = VaultRoot::try_from(root.to_path_buf()).map_err(|source| SearchError::EmbeddingPathInvalid {
        path: root.display().to_string(),
        source,
    })?;
    VaultSet::try_from(vec![vault_root]).map_err(|source| SearchError::EmbeddingPathInvalid {
        path: root.display().to_string(),
        source,
    })
}
