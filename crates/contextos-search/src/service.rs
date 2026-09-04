//! Combined per-vault search service.
//!
//! `VaultSearchService` wires one vault root's text index and link graph
//! behind a single `UpdatesSearch` consumer, so `contextos-mcp` can route
//! every completed mutation to whichever of the two derived indexes are
//! enabled for that vault, and expose one query, status, and rebuild surface
//! over both.
//!
//! Either index can be disabled independently through `[vault.search]`. A
//! disabled capability is represented internally as `None` rather than a
//! stub implementation, so callers observe the stable `index/disabled`
//! failure defined by [`SearchError`] instead of silently degraded
//! behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use contextos_core::{
    OpKind, OperationEvent, OperationWarning, PathError, SystemClock, UpdatesSearch, VaultPath, VaultPathInput,
    VaultRoot, VaultRootId, VaultSet,
};
use contextos_obsidian::{LinkCollection, ObsidianLink};
use serde::Serialize;
use time::OffsetDateTime;

use crate::chunk::Chunk;
use crate::document::relative_display;
use crate::text::IndexEntry;
use crate::{
    CatchUpKind, ChunkSource, DocumentSource, EmbeddingWorker, EmbeddingWorkerConfig, EmbedsText,
    FilesystemChunkSource, FreshnessReport, GraphBackend, GraphDirection, GraphView, IndexedDocument, IndexesText,
    LinkGraph, LinkGraphConfig, PathEmbeddingOutcome, ReadsChunkSource, SearchError, SimilarityQuery, SqliteVecConfig,
    SqliteVecStore, StoresVectors, TantivyIndex, TextHit, TextIndexConfig, TextQuery, TextSearchService,
    TextSyncConfig, chunk_document,
};

/// Construction input for one vault root's combined search service.
pub struct VaultSearchConfig {
    /// Identity of the vault root this service serves.
    pub root_id: VaultRootId,
    /// Resolved filesystem path of the vault root.
    pub root: PathBuf,
    /// Forward-slash relative path prefixes excluded from both indexes.
    pub excludes: Vec<String>,
    /// The vault's derived-state directory (its `.contextos` directory).
    pub state_directory: PathBuf,
    /// Whether the text index is enabled for this vault.
    pub text_enabled: bool,
    /// Whether the link graph is enabled for this vault.
    pub graph_enabled: bool,
    /// The link graph's persistence backend when enabled; ignored when
    /// `graph_enabled` is `false`.
    pub graph_backend: GraphBackend,
    /// Semantic search capability, or `None` when `[vault.search] semantic
    /// = false` for this vault.
    pub semantic: Option<SemanticConfig>,
}

/// Construction input for one vault's semantic search capability: an
/// already-selected embedding provider (config-driven, see
/// `crate::EmbeddingProviderConfig`) and where this vault's vector store
/// lives. Provider construction happens in the composition root, which is
/// the only layer that sees both the vault's TOML configuration and the
/// concrete provider types (mirroring `EmbeddingProviderConfig`'s own
/// module-boundary rationale).
pub struct SemanticConfig {
    pub embedder: Box<dyn EmbedsText>,
    /// Filesystem path of this vault's `.contextos/vectors.db`.
    pub vector_store_path: PathBuf,
}

/// Concrete embedding worker type this service drives: the vector store is
/// always `SqliteVecStore`, the provider is whichever
/// `EmbedsText` the composition root selected from configuration (boxed, so
/// swapping providers is a value, not a type, change), content is read from
/// the real filesystem, and timestamps come from the real clock.
type SemanticWorker = EmbeddingWorker<SqliteVecStore, Box<dyn EmbedsText>, FilesystemChunkSource, SystemClock>;

/// Combined per-vault text index and link graph service.
pub struct VaultSearchService {
    root_id: VaultRootId,
    root: PathBuf,
    excludes: Vec<String>,
    state_directory: PathBuf,
    text: Option<TextSearchService<TantivyIndex>>,
    text_directory: Option<PathBuf>,
    graph: Option<Mutex<LinkGraph>>,
    graph_store_directory: Option<PathBuf>,
    semantic: Option<SemanticWorker>,
}

impl std::fmt::Debug for VaultSearchService {
    /// Reports only the vault identity and which capabilities are enabled:
    /// the wrapped tantivy and link-graph internals do not implement
    /// `Debug`, so this is a hand-written summary rather than a derive.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultSearchService")
            .field("root_id", &self.root_id)
            .field("text_enabled", &self.text.is_some())
            .field("graph_enabled", &self.graph.is_some())
            .field("semantic_enabled", &self.semantic.is_some())
            .finish_non_exhaustive()
    }
}

impl TryFrom<VaultSearchConfig> for VaultSearchService {
    type Error = SearchError;

    fn try_from(value: VaultSearchConfig) -> Result<Self, Self::Error> {
        let (text, text_directory) = if value.text_enabled {
            let directory = value.state_directory.join("index");
            let index = TantivyIndex::try_from(TextIndexConfig {
                directory: directory.clone(),
            })?;
            let service = TextSearchService::from(TextSyncConfig {
                root_id: value.root_id,
                root: value.root.clone(),
                excludes: value.excludes.clone(),
                index,
            });
            (Some(service), Some(directory))
        } else {
            (None, None)
        };

        let (graph, graph_store_directory) = if value.graph_enabled {
            let store_directory = value.state_directory.join("graph");
            match LinkGraph::try_from(LinkGraphConfig {
                store_directory: store_directory.clone(),
                backend: value.graph_backend,
            }) {
                Ok(graph) => (Some(Mutex::new(graph)), Some(store_directory)),
                // Another process (a second server instance, or a concurrent
                // `contextos index` run) already holds this store's
                // exclusive lock: degrade to the same disabled
                // representation `graph_enabled = false` produces rather
                // than failing this vault's whole search service, so the
                // rest of its tools stay available.
                Err(SearchError::GraphLocked { path }) => {
                    tracing::warn!(
                        code = "index/locked",
                        path = %path,
                        "link graph store is held by another process; disabling the link graph \
                         for this vault"
                    );
                    (None, None)
                }
                Err(error) => return Err(error),
            }
        } else {
            (None, None)
        };

        let semantic = value
            .semantic
            .map(|config| build_semantic_worker(&value.root, config))
            .transpose()?;

        Ok(Self {
            root_id: value.root_id,
            root: value.root,
            excludes: value.excludes,
            state_directory: value.state_directory,
            text,
            text_directory,
            graph,
            graph_store_directory,
            semantic,
        })
    }
}

/// Text embedded once at construction, only for providers whose
/// [`EmbedsText::dimension`] is not known from configuration alone (an
/// arbitrary openai-compatible endpoint): the resulting vector's length
/// becomes the vector store's fixed dimension. Mirrors
/// `FastembedLocal`'s own internal dimension probe, generalised here so
/// every provider works uniformly regardless of whether it already knows
/// its dimension.
const DIMENSION_PROBE_TEXT: &str = "contextos-dimension-probe";

fn build_semantic_worker(root: &Path, config: SemanticConfig) -> Result<SemanticWorker, SearchError> {
    let SemanticConfig {
        embedder,
        vector_store_path,
    } = config;
    let dimension = if let Some(dimension) = embedder.dimension() {
        dimension
    } else {
        let probe = Chunk::query(DIMENSION_PROBE_TEXT);
        let vectors = embedder.embed(std::slice::from_ref(&probe))?;
        vectors
            .first()
            .map(Vec::len)
            .ok_or_else(|| SearchError::EmbeddingShapeMismatch {
                reason: "embedding provider returned no vector for the dimension probe".to_owned(),
            })?
    };
    let store = SqliteVecStore::try_from(SqliteVecConfig {
        path: vector_store_path,
        dimension,
    })?;
    EmbeddingWorker::try_from(EmbeddingWorkerConfig {
        root: root.to_path_buf(),
        store,
        embedder,
        content: FilesystemChunkSource::new(root.to_path_buf()),
        clock: SystemClock,
    })
}

impl UpdatesSearch for VaultSearchService {
    /// Applies the event to whichever of the text index and link graph are
    /// enabled. Both are attempted regardless of whether the first fails;
    /// the first failure becomes the returned warning.
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let text_result = match &self.text {
            Some(text) => text.update(event),
            None => Ok(()),
        };
        let graph_result = self.update_graph(event);
        self.enqueue_semantic(event);
        text_result.and(graph_result)
    }
}

impl VaultSearchService {
    /// Runs a full-text query after reconciling the text index against the
    /// current filesystem state.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] when text search is disabled
    /// for this vault, an invalid-query error for unparsable syntax or
    /// filter values, and a storage error when the index cannot be read.
    pub fn query_text(&self, request: &TextQuery<'_>) -> Result<(Vec<TextHit>, FreshnessReport), SearchError> {
        let text = self.text.as_ref().ok_or(SearchError::TextDisabled)?;
        let freshness = text.refresh()?;
        let hits = text.index().query(request)?;
        Ok((hits, freshness))
    }

    /// Returns nodes and edges reachable from `from` within `depth` hops.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, [`SearchError::InvalidDepth`] when `depth`
    /// is outside `1..=4`, and [`SearchError::UnknownNote`] when `from` is
    /// not in the graph.
    pub fn graph_neighbours(
        &self,
        from: &str,
        depth: u32,
        direction: GraphDirection,
    ) -> Result<GraphView, SearchError> {
        let graph = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        Self::locked(graph).neighbours(from, depth, direction)
    }

    /// Returns the notes and edges that link or embed directly to `from`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, and [`SearchError::UnknownNote`] when
    /// `from` is not in the graph.
    pub fn graph_backlinks(&self, from: &str) -> Result<GraphView, SearchError> {
        let graph = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        Self::locked(graph).backlinks(from)
    }

    /// Returns the shortest path between `from` and `to`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, and [`SearchError::UnknownNote`] when
    /// `from` or `to` is not in the graph.
    pub fn graph_path(&self, from: &str, to: &str, direction: GraphDirection) -> Result<GraphView, SearchError> {
        let graph = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        Self::locked(graph).path_between(from, to, direction)
    }

    /// Returns every real note with neither incoming nor outgoing edges.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault.
    pub fn graph_orphans(&self) -> Result<GraphView, SearchError> {
        let graph = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        Self::locked(graph).orphans()
    }

    /// Returns the top `request.limit` chunks by cosine similarity to
    /// `request.query`'s embedding, optionally scoped to
    /// `request.path_prefix` and with `request.exclude_paths` removed.
    ///
    /// A hit whose source document changed since it was last embedded (so
    /// re-chunking it no longer produces a chunk at the stored ordinal) is
    /// silently skipped rather than returned with stale or missing text;
    /// this staleness is visible through
    /// [`VaultSearchService::status`]'s semantic `stale_estimate` instead.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search is
    /// disabled for this vault, and a typed embedding or storage error when
    /// the query cannot be embedded or the vector store cannot be read.
    pub fn query_semantic(&self, request: &SemanticQuery<'_>) -> Result<Vec<SemanticHit>, SearchError> {
        let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
        if request.limit == 0 {
            return Ok(Vec::new());
        }

        let probe = Chunk::query(request.query);
        let vectors = semantic.embedder().embed(std::slice::from_ref(&probe))?;
        let vector = vectors.first().ok_or_else(|| SearchError::EmbeddingShapeMismatch {
            reason: "embedding provider returned no vector for the query text".to_owned(),
        })?;

        let hits = semantic.store().similar(&SimilarityQuery {
            vector,
            k: request.limit,
            path_prefix: request.path_prefix,
            exclude_paths: request.exclude_paths,
        })?;

        let mut results = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(text) = self.reread_chunk_text(semantic, &hit.path, hit.ordinal)? {
                results.push(SemanticHit {
                    path: hit.path,
                    chunk: text,
                    score: hit.score,
                    heading_context: hit.heading_context,
                });
            }
        }
        Ok(results)
    }

    /// Returns per-index document counts, staleness, and last-build time.
    ///
    /// `stale_estimate` is computed with a read-only scan: it never
    /// reindexes. Call [`VaultSearchService::rebuild`] to reconcile.
    ///
    /// # Errors
    ///
    /// Returns a storage error when an enabled index cannot be read, or
    /// when the vault cannot be scanned.
    pub fn status(&self) -> Result<IndexStatusReport, SearchError> {
        let text = match &self.text {
            Some(text) => {
                let entries = text.index().entries()?;
                let documents = entries.len();
                let stale_estimate = self.text_stale_estimate(&entries)?;
                let last_build = self
                    .text_directory
                    .as_deref()
                    .map(newest_mtime_under)
                    .transpose()?
                    .flatten();
                TextIndexStatus {
                    enabled: true,
                    documents,
                    stale_estimate,
                    last_build,
                }
            }
            None => TextIndexStatus::default(),
        };

        let graph = match &self.graph {
            Some(graph) => {
                let mut locked = Self::locked(graph);
                let view = locked.full_view()?;
                let needs_rebuild = locked.needs_rebuild();
                let sync_status = locked.sync_status();
                drop(locked);
                let last_build = self
                    .graph_store_directory
                    .as_deref()
                    .map(newest_mtime_under)
                    .transpose()?
                    .flatten();
                GraphIndexStatus {
                    enabled: true,
                    nodes: view.nodes.len(),
                    edges: view.edges.len(),
                    needs_rebuild,
                    last_build,
                    generation: sync_status.map(|status| status.generation),
                    last_catch_up: sync_status.and_then(|status| status.last_catch_up),
                }
            }
            None => GraphIndexStatus::default(),
        };

        let semantic = match &self.semantic {
            Some(semantic) => {
                let stats = semantic.store().stats()?;
                let worker_status = semantic.status();
                SemanticIndexStatus {
                    enabled: true,
                    documents: stats.documents,
                    chunks: stats.chunks,
                    stale_estimate: worker_status.pending,
                    last_build: worker_status.last_processed_at,
                }
            }
            None => SemanticIndexStatus::default(),
        };

        Ok(IndexStatusReport {
            state_directory: self.state_directory.clone(),
            text,
            graph,
            semantic,
        })
    }

    /// This vault's derived-state directory: the parent of
    /// the text index, link-graph cache, and semantic vector store, so an
    /// operator can confirm a separately invoked `contextos index` and this
    /// running server resolve to the same on-disk store rather than two
    /// silently diverged ones.
    #[must_use]
    pub fn state_directory(&self) -> &Path {
        &self.state_directory
    }

    /// Whether semantic search is enabled for this vault: the
    /// cheap, in-memory check a caller uses to decide whether there is
    /// anything to drain, without the I/O `status`'s live store query
    /// costs.
    #[must_use]
    pub fn semantic_enabled(&self) -> bool {
        self.semantic.is_some()
    }

    /// Rebuilds the selected index (or both) from a full vault scan.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] or [`SearchError::GraphDisabled`]
    /// when the requested index is disabled for this vault,
    /// [`SearchError::SemanticUnavailable`] for [`RebuildTarget::Semantic`],
    /// and a storage error when the vault cannot be scanned or an index
    /// cannot be written.
    pub fn rebuild(&self, target: RebuildTarget) -> Result<RebuildReport, SearchError> {
        self.rebuild_with_progress(target, &mut |_| {})
    }

    /// As [`Self::rebuild`], but invokes `on_progress` as each phase starts,
    /// completes, or (for the semantic phase) processes one more document,
    /// so a caller driving a long-running rebuild can surface live activity
    /// instead of a single silent wait.
    ///
    /// # Errors
    ///
    /// Same as [`Self::rebuild`].
    pub fn rebuild_with_progress(
        &self,
        target: RebuildTarget,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<RebuildReport, SearchError> {
        self.rebuild_with_budget(target, None, on_progress)
    }

    /// As [`Self::rebuild_with_progress`], but the semantic phase (the only
    /// phase costly enough for this to matter, text and graph are each a
    /// single fast pass) stops once `budget` has elapsed since it began,
    /// leaving anything still queued for a later call to pick up rather
    /// than blocking past a caller's own request timeout. `None` drains
    /// exhaustively, matching [`Self::rebuild_with_progress`]. Text and
    /// graph rebuilds always run to completion regardless of `budget`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::rebuild`].
    pub fn rebuild_with_budget(
        &self,
        target: RebuildTarget,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<RebuildReport, SearchError> {
        match target {
            RebuildTarget::Text => Ok(RebuildReport {
                text: Some(self.rebuild_text_reporting(on_progress)?),
                graph: None,
                semantic: None,
            }),
            RebuildTarget::Graph => Ok(RebuildReport {
                text: None,
                graph: Some(self.rebuild_graph_reporting(on_progress)?),
                semantic: None,
            }),
            RebuildTarget::All => {
                let text = Some(self.rebuild_text_reporting(on_progress)?);
                let graph = Some(self.rebuild_graph_reporting(on_progress)?);
                let semantic = match &self.semantic {
                    Some(_) => Some(self.rebuild_semantic(budget, on_progress)?),
                    None => None,
                };
                Ok(RebuildReport { text, graph, semantic })
            }
            RebuildTarget::Semantic => Ok(RebuildReport {
                text: None,
                graph: None,
                semantic: Some(self.rebuild_semantic(budget, on_progress)?),
            }),
        }
    }
}

impl VaultSearchService {
    fn locked(graph: &Mutex<LinkGraph>) -> std::sync::MutexGuard<'_, LinkGraph> {
        graph.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update_graph(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let Some(graph) = &self.graph else {
            return Ok(());
        };
        match event.kind {
            OpKind::Delete => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.remove_graph_note(graph, path)?;
                    }
                }
            }
            OpKind::Move => {
                if let Some(from) = event.paths.first()
                    && from.root_id() == self.root_id
                {
                    self.remove_graph_note(graph, from)?;
                }
                if let Some(to) = event.paths.get(1)
                    && to.root_id() == self.root_id
                {
                    self.sync_graph_note(graph, to)?;
                }
            }
            OpKind::Create | OpKind::Modify | OpKind::Restore => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.sync_graph_note(graph, path)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn remove_graph_note(&self, graph: &Mutex<LinkGraph>, path: &VaultPath) -> Result<(), OperationWarning> {
        let relative = relative_display(path);
        if !self.in_scope(&relative) {
            return Ok(());
        }
        Self::locked(graph)
            .remove_note(&relative)
            .map_err(OperationWarning::from)
    }

    fn sync_graph_note(&self, graph: &Mutex<LinkGraph>, path: &VaultPath) -> Result<(), OperationWarning> {
        let relative = relative_display(path);
        if !self.in_scope(&relative) {
            return Ok(());
        }

        let absolute = self.root.join(&relative);
        let is_file = fs::metadata(&absolute).is_ok_and(|metadata| metadata.is_file());
        if !is_file {
            return Self::locked(graph)
                .remove_note(&relative)
                .map_err(OperationWarning::from);
        }

        let content = fs::read_to_string(&absolute).map_err(|source| {
            OperationWarning::from(SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })
        })?;
        let modified = fs::metadata(&absolute)
            .and_then(|metadata| metadata.modified())
            .map(OffsetDateTime::from)
            .map_err(|source| {
                OperationWarning::from(SearchError::DocumentRead {
                    path: relative.clone(),
                    source,
                })
            })?;
        let title = IndexedDocument::from(DocumentSource {
            path,
            content: &content,
            modified,
        })
        .title()
        .to_owned();

        // A parse failure degrades to skipping the graph update for this
        // file rather than failing the whole mutation: the note's text is
        // still indexed and readable even when its wikilink syntax is
        // malformed.
        let Ok(collection) = LinkCollection::try_from(content.as_str()) else {
            return Ok(());
        };
        let links = collection.outgoing();

        Self::locked(graph)
            .upsert_note(&relative, &title, links)
            .map_err(OperationWarning::from)
    }

    fn in_scope(&self, relative: &str) -> bool {
        is_markdown(relative) && !is_excluded(&self.excludes, relative)
    }

    /// Signals every in-scope path touched by `event` to the semantic
    /// embedding queue, unconditionally of `event.kind`: unlike the link
    /// graph, the embedding worker itself decides add versus remove by
    /// checking whether the path still exists when it processes the signal
    /// (`EmbeddingWorker::process_path`), so no per-`OpKind` branching is
    /// needed here. Enqueuing is infallible and never blocks or fails the
    /// write pipeline; actual embedding happens later, off this call.
    fn enqueue_semantic(&self, event: &OperationEvent) {
        let Some(semantic) = &self.semantic else {
            return;
        };
        for path in &event.paths {
            if path.root_id() == self.root_id {
                let relative = relative_display(path);
                if self.in_scope(&relative) {
                    semantic.enqueue(relative);
                }
            }
        }
    }

    /// Re-reads and re-chunks `path`'s current content to recover the prose
    /// for the chunk at `ordinal`: the vector store holds only vectors and
    /// identifying metadata, never chunk text (see the `vector_store`
    /// module docs). Returns `None` when the path no longer exists or no
    /// longer produces a chunk at `ordinal` (the document changed since it
    /// was last embedded); [`VaultSearchService::query_semantic`] skips
    /// such a hit rather than returning stale or missing text.
    fn reread_chunk_text(
        &self,
        semantic: &SemanticWorker,
        path: &str,
        ordinal: usize,
    ) -> Result<Option<String>, SearchError> {
        let Some(content) = semantic.content().read(path)? else {
            return Ok(None);
        };
        let roots = resolve_single_root_set(&self.root)?;
        let vault_path = VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: path,
        })
        .map_err(|source| SearchError::EmbeddingPathInvalid {
            path: path.to_owned(),
            source,
        })?;
        let chunks = chunk_document(ChunkSource {
            path: &vault_path,
            content: &content,
        });
        Ok(chunks
            .into_iter()
            .find(|chunk| chunk.ordinal() == ordinal)
            .map(|chunk| chunk.text().to_owned()))
    }

    fn text_stale_estimate(&self, entries: &[IndexEntry]) -> Result<usize, SearchError> {
        let stored: BTreeMap<&str, OffsetDateTime> = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.modified))
            .collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stale = 0_usize;

        for (relative, absolute) in self.collect_markdown_files()? {
            seen.insert(relative.clone());
            let Ok(metadata) = fs::metadata(&absolute) else {
                continue;
            };
            let Ok(modified) = metadata.modified().map(OffsetDateTime::from) else {
                continue;
            };
            match stored.get(relative.as_str()) {
                Some(stored_modified) if *stored_modified == modified => {}
                _ => stale = stale.saturating_add(1),
            }
        }
        for path in stored.keys() {
            if !seen.contains(*path) {
                stale = stale.saturating_add(1);
            }
        }
        Ok(stale)
    }

    fn rebuild_text(&self) -> Result<FreshnessReport, SearchError> {
        self.text.as_ref().ok_or(SearchError::TextDisabled)?.refresh()
    }

    fn rebuild_text_reporting(
        &self,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<FreshnessReport, SearchError> {
        on_progress(RebuildProgress::TextStarted);
        let report = self.rebuild_text()?;
        on_progress(RebuildProgress::TextComplete);
        Ok(report)
    }

    fn rebuild_graph(&self) -> Result<GraphRebuildReport, SearchError> {
        let graph = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        let notes = self.collect_graph_notes()?;
        let notes_scanned = notes.len();
        let mut locked = Self::locked(graph);
        locked.rebuild(&notes)?;
        let view = locked.full_view()?;
        Ok(GraphRebuildReport {
            notes_scanned,
            nodes: view.nodes.len(),
            edges: view.edges.len(),
        })
    }

    fn rebuild_graph_reporting(
        &self,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<GraphRebuildReport, SearchError> {
        on_progress(RebuildProgress::GraphStarted);
        let report = self.rebuild_graph()?;
        on_progress(RebuildProgress::GraphComplete);
        Ok(report)
    }

    /// Enqueues every in-scope markdown file not already queued, then
    /// drains the queue synchronously. Re-embedding is still gated on
    /// content hash change only, so an unchanged chunk is skipped even
    /// during a rebuild rather than forced
    /// through the embedding provider again; this mirrors
    /// [`EmbeddingWorker`]'s existing hash-skip behaviour rather than
    /// introducing a second, forced-reembed code path.
    ///
    /// The vault walk and enqueue only happen when the queue is currently
    /// empty. `EmbeddingWorker::enqueue` dedupes against paths still
    /// queued, but not against paths a previous, budget-truncated call
    /// already dequeued and processed (those are gone from its queue
    /// entirely); unconditionally re-walking on every call would silently
    /// revive every already-completed path, making the reported `remaining`
    /// oscillate around the vault's total file count instead of trending
    /// to zero across repeated calls with the same target. Skipping the
    /// reseed while work is still queued costs nothing: any file changed
    /// mid-pass is already caught by the live write-pipeline's own
    /// `enqueue` calls, independent of this walk.
    fn rebuild_semantic(
        &self,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<SemanticRebuildReport, SearchError> {
        let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
        if semantic.status().pending == 0 {
            for (relative, _absolute) in self.collect_markdown_files()? {
                semantic.enqueue(relative);
            }
        }
        self.drain_semantic_outcomes(budget, on_progress)
    }

    /// Drains only whatever is currently queued for this vault's semantic
    /// index (paths enqueued through the live write pipeline,
    /// [`VaultSearchService::update`], or a prior call), without ever
    /// performing [`Self::rebuild_semantic`]'s "queue empty -> walk the
    /// whole vault" catch-up scan.
    ///
    /// Suited to a caller that runs on a short, frequent interval (a
    /// background maintenance task) rather than an occasional explicit
    /// operator action: repeatedly re-walking and re-hashing every file in
    /// the vault on every idle tick is a real, serious cost this avoids
    /// entirely. A caller that also needs to discover content
    /// never routed through the live write pipeline (edited outside this
    /// server, for example directly in Obsidian) still needs an
    /// occasional [`Self::rebuild`]/[`Self::rebuild_with_budget`] call to
    /// find it.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search
    /// is disabled for this vault, and a storage error when the vector
    /// store or embedding provider fails.
    pub fn drain_semantic_queue(&self, budget: Option<time::Duration>) -> Result<SemanticRebuildReport, SearchError> {
        self.drain_semantic_outcomes(budget, &mut |_| {})
    }

    /// Shared by [`Self::rebuild_semantic`] (which may first enqueue the
    /// whole vault) and [`Self::drain_semantic_queue`] (which never does):
    /// drains whatever is queued right now and aggregates the outcomes.
    fn drain_semantic_outcomes(
        &self,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<SemanticRebuildReport, SearchError> {
        let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
        let total = semantic.status().pending;

        let mut report = SemanticRebuildReport::default();
        let outcomes = match budget {
            Some(budget) => semantic.drain_until(budget),
            None => semantic.drain(),
        };
        for outcome in outcomes {
            report.paths_scanned = report.paths_scanned.saturating_add(1);
            on_progress(RebuildProgress::SemanticProgress {
                completed: report.paths_scanned,
                total,
            });
            match outcome {
                PathEmbeddingOutcome::Embedded { embedded, skipped, .. } => {
                    report.embedded = report.embedded.saturating_add(embedded);
                    report.skipped = report.skipped.saturating_add(skipped);
                }
                PathEmbeddingOutcome::Removed { .. } => {}
                PathEmbeddingOutcome::Failed { .. } => {
                    report.failed = report.failed.saturating_add(1);
                }
            }
        }
        report.remaining = semantic.status().pending;
        Ok(report)
    }

    fn collect_markdown_files(&self) -> Result<Vec<(String, PathBuf)>, SearchError> {
        let mut files = Vec::new();
        walk_markdown(&self.root, &self.root, &self.excludes, &mut files)?;
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files)
    }

    fn collect_graph_notes(&self) -> Result<Vec<(String, String, Vec<ObsidianLink>)>, SearchError> {
        let roots = resolve_single_root_set(&self.root)?;
        let mut notes = Vec::new();
        for (relative, absolute) in self.collect_markdown_files()? {
            let content = fs::read_to_string(&absolute).map_err(|source| SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })?;
            let metadata = fs::metadata(&absolute).map_err(|source| SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })?;
            let modified =
                metadata
                    .modified()
                    .map(OffsetDateTime::from)
                    .map_err(|source| SearchError::DocumentRead {
                        path: relative.clone(),
                        source,
                    })?;
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &relative,
            })
            .map_err(|source| SearchError::IndexDirectory {
                path: relative.clone(),
                source: std::io::Error::other(source),
            })?;
            let title = IndexedDocument::from(DocumentSource {
                path: &path,
                content: &content,
                modified,
            })
            .title()
            .to_owned();
            // As in `sync_graph_note`, a wikilink parse failure degrades to
            // an empty link set for this note rather than failing the
            // whole rebuild.
            let links = LinkCollection::try_from(content.as_str())
                .map(|collection| collection.outgoing().to_vec())
                .unwrap_or_default();
            notes.push((relative, title, links));
        }
        Ok(notes)
    }
}

/// Per-index document counts, staleness, and last-build time.
///
/// `state_directory` is the vault's derived-state directory itself (the
/// parent of the text index, link-graph cache, and semantic vector store),
/// surfaced so an operator can confirm a separately invoked `contextos
/// index` and a running server resolve to the same on-disk store rather
/// than two silently diverged ones.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct IndexStatusReport {
    pub state_directory: PathBuf,
    pub text: TextIndexStatus,
    pub graph: GraphIndexStatus,
    pub semantic: SemanticIndexStatus,
}

/// Status of the text index.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct TextIndexStatus {
    pub enabled: bool,
    pub documents: usize,
    /// A read-only estimate of files missing from the index or whose
    /// modification time no longer matches the stored entry. Computed
    /// without reindexing; call [`VaultSearchService::rebuild`] to
    /// reconcile.
    pub stale_estimate: usize,
    pub last_build: Option<OffsetDateTime>,
}

/// Status of the link graph.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GraphIndexStatus {
    pub enabled: bool,
    pub nodes: usize,
    pub edges: usize,
    pub needs_rebuild: bool,
    pub last_build: Option<OffsetDateTime>,
    /// This vault's link graph's current generation and most recent
    /// cross-instance catch-up kind, or `None` when
    /// the configured `graph_backend` does not track one (`fjall`,
    /// `serde`).
    pub generation: Option<u64>,
    pub last_catch_up: Option<CatchUpKind>,
}

/// Status of the semantic index: `documents` and `chunks`
/// count currently stored vectors; `stale_estimate` is the embedding
/// worker's pending-queue length (paths signalled but not yet processed),
/// matching [`TextIndexStatus::stale_estimate`]'s read-only-estimate
/// contract without a second full-vault scan; `last_build` is when the
/// worker last finished processing a path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SemanticIndexStatus {
    pub enabled: bool,
    pub documents: usize,
    pub chunks: usize,
    pub stale_estimate: usize,
    pub last_build: Option<OffsetDateTime>,
}

/// Selects which index [`VaultSearchService::rebuild`] repopulates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildTarget {
    Text,
    Graph,
    Semantic,
    All,
}

/// One progress update from [`VaultSearchService::rebuild_with_progress`].
///
/// `Text`/`Graph` only ever emit their `Started`/`Complete` pair (each
/// phase is fast enough that finer granularity isn't useful); `Semantic`
/// emits one `Progress` update per document as it completes, since local
/// CPU embedding of a large vault is the phase long enough that a caller
/// needs visibility into it while it runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildProgress {
    TextStarted,
    TextComplete,
    GraphStarted,
    GraphComplete,
    SemanticProgress { completed: usize, total: usize },
}

/// Outcome of a full graph rebuild from a vault scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GraphRebuildReport {
    pub notes_scanned: usize,
    pub nodes: usize,
    pub edges: usize,
}

/// Outcome of a full semantic rebuild from a vault scan.
/// `embedded` and `skipped` count individual chunks (a chunk is skipped
/// when its content hash is unchanged since the last build); `failed`
/// counts whole paths whose processing failed. `remaining` is the number of
/// paths still queued once this call returned: nonzero only when a budget
/// (see [`VaultSearchService::rebuild_with_budget`]) cut the rebuild short,
/// and the signal a caller uses to decide whether to call again.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SemanticRebuildReport {
    pub paths_scanned: usize,
    pub embedded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub remaining: usize,
}

/// Combined outcome of one [`VaultSearchService::rebuild`] call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RebuildReport {
    pub text: Option<FreshnessReport>,
    pub graph: Option<GraphRebuildReport>,
    pub semantic: Option<SemanticRebuildReport>,
}

/// One `query_semantic` request: `limit` of `0` returns no hits,
/// without error, without embedding the query or reading the store,
/// matching [`crate::vector_store::SimilarityQuery::k`]'s own `0`
/// convention.
#[derive(Clone, Copy, Debug)]
pub struct SemanticQuery<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub path_prefix: Option<&'a str>,
    /// See [`crate::vector_store::SimilarityQuery::exclude_paths`].
    pub exclude_paths: &'a [String],
}

/// One ranked `query_semantic` hit. `chunk` is the chunk's prose,
/// re-read from its source document at query time: the vector store never
/// holds chunk text itself (see the `vector_store` module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHit {
    pub path: String,
    pub chunk: String,
    pub score: f32,
    pub heading_context: Vec<String>,
}

/// Reports whether `relative` is an in-scope markdown path: it must end
/// with `.md` and must not fall under any configured exclude prefix. This
/// mirrors `sync::in_scope`, duplicated here (rather than shared) to keep
/// the already-delivered `sync` module unchanged.
fn is_markdown(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_excluded(excludes: &[String], relative: &str) -> bool {
    excludes.iter().any(|prefix| {
        relative == prefix.as_str()
            || relative
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn forward_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn walk_markdown(
    root: &Path,
    directory: &Path,
    excludes: &[String],
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), SearchError> {
    let entries = fs::read_dir(directory).map_err(|source| SearchError::DocumentRead {
        path: directory.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SearchError::DocumentRead {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(root) else {
            continue;
        };
        let relative = forward_slash(relative_path);
        if is_excluded(excludes, &relative) {
            continue;
        }
        let file_type = entry.file_type().map_err(|source| SearchError::DocumentRead {
            path: path.display().to_string(),
            source,
        })?;
        if file_type.is_dir() {
            walk_markdown(root, &path, excludes, files)?;
        } else if file_type.is_file() && is_markdown(&relative) {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Builds a single-root `VaultSet` from the already-resolved vault root,
/// used to construct throwaway `VaultPath`s for files discovered by the
/// rebuild walk.
fn resolve_single_root_set(root: &Path) -> Result<VaultSet, SearchError> {
    let vault_root = VaultRoot::try_from(root.to_path_buf()).map_err(|source| root_set_error(root, source))?;
    VaultSet::try_from(vec![vault_root]).map_err(|source| root_set_error(root, source))
}

fn root_set_error(root: &Path, source: PathError) -> SearchError {
    SearchError::IndexDirectory {
        path: root.display().to_string(),
        source: std::io::Error::other(source),
    }
}

/// Returns the newest modification time of any file under `dir`, or `None`
/// when `dir` is missing or contains no files.
fn newest_mtime_under(dir: &Path) -> Result<Option<OffsetDateTime>, SearchError> {
    let mut newest: Option<OffsetDateTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SearchError::DocumentRead {
                    path: current.display().to_string(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| SearchError::DocumentRead {
                path: current.display().to_string(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| SearchError::DocumentRead {
                path: path.display().to_string(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let modified = OffsetDateTime::from(modified);
            if newest.is_none_or(|current_newest| modified > current_newest) {
                newest = Some(modified);
            }
        }
    }
    Ok(newest)
}
