//! Wikilink graph service.
//!
//! `LinkGraph` builds an in-memory directed graph of vault notes from
//! `contextos-obsidian` wikilink parses. Every real note is a graph node;
//! a wikilink target that does not resolve to a known note becomes a
//! phantom node instead, mirroring Obsidian's own treatment of a link to a
//! note that does not yet exist. The graph is persisted incrementally
//! behind the [`StoresGraph`] trait boundary alongside the vault's other
//! derived state, so it need not be rebuilt from scratch on every server
//! start and so a single-note edit writes only the records that changed
//! rather than the whole graph. The concrete backend is configurable per
//! vault (`GraphBackend`): `fjall` (default, an embedded KV store,
//! single-process-exclusive), `sqlite` (WAL mode, tolerates more than one
//! server process against the same vault), or `serde` (a single JSON file,
//! for a small, starter, or experimental vault).
//!
//! `upsert_note` and `remove_note` apply per-file changes as the write
//! pipeline observes them; `rebuild` repopulates the whole graph from a
//! full vault scan.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use contextos_obsidian::ObsidianLink;
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};

use crate::SearchError;

mod fjall_store;
mod serde_store;
mod sqlite_store;

use fjall_store::FjallGraphStore;
use serde_store::SerdeGraphStore;
use sqlite_store::SqliteGraphStore;

/// The link graph store format identifier. Bumping this forces every
/// existing store to be treated as incompatible, triggering a full
/// rebuild, the same role `CACHE_FORMAT` played for the former JSON cache.
/// Shared across every [`StoresGraph`] backend: it versions the domain
/// record shape, not any one storage engine's own on-disk layout.
const STORE_FORMAT: u32 = 1;

/// One vault note or unresolved wikilink target in the link graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// Forward-slash relative vault path, or the unresolved wikilink
    /// target text as written when `phantom` is `true`.
    pub path: String,
    /// The note's derived title, or the same text as `path` when
    /// `phantom` is `true`.
    pub title: String,
    /// `true` when no real note resolves this path: a stand-in node for a
    /// wikilink target that does not (yet) exist.
    pub phantom: bool,
}

/// The kind of wikilink relationship one graph edge represents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphEdgeKind {
    /// A plain wikilink, `[[target]]`.
    Link,
    /// An embedded wikilink, `![[target]]`.
    Embed,
}

impl GraphEdgeKind {
    /// Returns the lowercase label used for deterministic ordering; this
    /// matches the `#[serde(rename_all = "lowercase")]` wire form.
    const fn label(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Embed => "embed",
        }
    }
}

/// One directed wikilink or embed relationship between two graph nodes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Forward-slash relative path of the linking note.
    pub from: String,
    /// Forward-slash relative path (or phantom path) of the link target.
    pub to: String,
    /// Whether the source wikilink is a plain link or an embed.
    pub kind: GraphEdgeKind,
}

/// An artefact-renderable slice of the link graph.
///
/// Every returned view lists exactly the nodes touched by its edges plus
/// the query's focus node(s). Nodes are sorted by `path`; edges are sorted
/// by `(from, to, kind)`, so repeated queries against an unchanged graph
/// render identically.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The traversal direction for a link graph query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphDirection {
    /// Follow outgoing links and embeds only.
    Out,
    /// Follow incoming links and embeds only.
    In,
    /// Follow both directions.
    Both,
}

impl GraphDirection {
    /// Returns the `petgraph` directions to traverse for this query
    /// direction.
    const fn directions(self) -> &'static [Direction] {
        match self {
            Self::Out => &[Direction::Outgoing],
            Self::In => &[Direction::Incoming],
            Self::Both => &[Direction::Outgoing, Direction::Incoming],
        }
    }
}

/// Trusted location of the link graph's persisted store: a directory each
/// [`GraphBackend`] owns exclusively, laying out its own file(s) inside it
/// (`fjall`'s own embedded-database directory, `sqlite`'s `graph.sqlite3`,
/// `serde`'s `graph.json`), never a single fixed file shared across
/// backends.
#[derive(Clone, Debug)]
pub struct LinkGraphConfig {
    pub store_directory: PathBuf,
    pub backend: GraphBackend,
}

/// The configurable persistence backend for one vault's link graph
/// (`[vault.search] graph_backend`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GraphBackend {
    /// A single JSON file, rewritten whole on every persisted change.
    /// Simplest to inspect by hand; intended for a small, starter, or
    /// experimental vault, not concurrent multi-process use, since a plain
    /// file has no locking of its own.
    Serde,
    /// An embedded `fjall` key-value store, single-process-exclusive.
    /// Fastest for the common case (one server process per vault); a
    /// second process opening the same vault's store instead observes
    /// [`SearchError::GraphLocked`].
    #[default]
    Fjall,
    /// A `rusqlite`-backed `SQLite` database in WAL journal mode with an
    /// explicit busy timeout, so more than one `contextos` process can
    /// hold the same vault's graph store open concurrently. The intended
    /// choice whenever a vault is legitimately opened by more than one
    /// server process from the same operator at once. The only backend
    /// that also tracks a `generation` counter for cross-instance
    /// propagation.
    Sqlite,
}

/// Whether a [`LinkGraph`]'s most recent cross-instance catch-up applied a
/// partial delta or fell back to a full reload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatchUpKind {
    /// Only the records persisted since this instance's last-seen
    /// generation were retrieved and applied: the common-case fast path,
    /// cost proportional to what actually changed.
    Partial,
    /// This instance's last-seen generation had aged out of the store's
    /// retained tombstone history, so the whole in-memory graph was
    /// reloaded from scratch instead: the correctness-preserving fallback
    /// for a reader that has been idle a long time.
    FullReload,
}

/// Cross-instance propagation status for one [`LinkGraph`]. `None` from
/// [`LinkGraph::sync_status`] when the configured
/// backend does not track propagation (`fjall`, `serde`): both are
/// unaffected by design, see [`GraphBackend::Sqlite`]'s own documentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SyncStatus {
    /// This instance's last-seen generation: caught up to at least this
    /// point, possibly further if nothing has changed since.
    pub generation: u64,
    /// The kind of the most recent catch-up this instance actually
    /// performed, or `None` if this instance has not yet needed one (no
    /// sibling write observed since it opened).
    pub last_catch_up: Option<CatchUpKind>,
}

/// The edge weight actually held in the in-memory graph: the public
/// [`GraphEdgeKind`] plus a persisted, process-restart-stable identity used
/// as this edge's key in the `edges` keyspace.
///
/// `petgraph`'s own `EdgeIndex` cannot serve this role: it is an
/// implementation-internal handle whose value depends on insertion and
/// removal order within one `StableDiGraph` instance, not a durable
/// identity. `store_id` is instead assigned from a monotonic counter
/// persisted in the `metadata` keyspace, so it survives a reopen and,
/// critically, stays distinct across parallel edges: a note that links to
/// the same target twice legitimately produces two edges (`StableDiGraph`
/// permits parallel edges, and `wire_links` adds one per parsed link), and
/// a key of only `(from, to, kind)` would collapse both writes into one
/// stored record, silently losing the second edge on reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EdgeRecord {
    kind: GraphEdgeKind,
    store_id: u64,
}

/// Metadata read back from a graph store: the persisted format identifier
/// and the next-edge-id allocator, or `None` (from
/// [`StoresGraph::read_metadata`]) when either is absent, unreadable,
/// malformed, or the stored format does not match [`STORE_FORMAT`]: every
/// case that means the store's node and edge content should not be
/// trusted.
struct StoredGraphMetadata {
    next_edge_id: u64,
    /// The store's generation counter as of this read, `0`
    /// for `fjall`/`serde` (meaningless there, never consulted since
    /// [`LinkGraph`] only checks propagation state for `sqlite`) and for a
    /// `sqlite` store predating this field's introduction (treated
    /// leniently as `0`, the oldest possible generation, rather than as a
    /// format mismatch: an in-place additive migration, not a forced
    /// rebuild, per this store's own migration comment).
    generation: u64,
}

/// One node or edge change to apply to a graph store in one atomic write,
/// carrying the fully resolved current value rather than a reference into
/// [`LinkGraph`]'s own in-memory graph: every [`StoresGraph`] implementation
/// owns only durable storage of the records it is handed, never a second
/// copy of graph structure, traversal, or edge-id-allocation logic, all of
/// which stay in [`LinkGraph`] itself.
#[derive(Debug)]
enum GraphChange {
    UpsertNode(GraphNode),
    RemoveNode(String),
    UpsertEdge {
        store_id: u64,
        edge: GraphEdge,
    },
    RemoveEdge {
        store_id: u64,
    },
    /// Marks that everything before this point was invalidated outside the
    /// normal upsert/remove vocabulary: [`LinkGraph::rebuild`]
    /// clears its store directly (`StoresGraph::clear`) before repopulating
    /// it, so the ordinary changelog has no record of *why* the old state
    /// disappeared. A sibling instance's [`StoresGraph::changes_since`]
    /// call must treat a `Reset` anywhere in its requested range as forcing
    /// a full reload, never a partial merge of old-plus-new content:
    /// applying only the fresh upserts on top of stale in-memory state
    /// would silently keep records the rebuild actually dropped.
    Reset,
}

/// Every node, and every `(store_id, edge)` pair, a [`StoresGraph::load_all`]
/// call read back from its backend.
type GraphRecords = (Vec<GraphNode>, Vec<(u64, GraphEdge)>);

/// A reconstructed in-memory graph, its path index, and its `store_id`
/// index, as [`build_graph`] returns and [`LoadedSnapshot`] carries.
type BuiltGraph = (
    StableDiGraph<GraphNode, EdgeRecord>,
    HashMap<String, NodeIndex>,
    HashMap<u64, EdgeIndex>,
);

/// Everything a [`StoresGraph::changes_since`] call returns: every change
/// persisted strictly after the requested generation, in the same relative
/// order [`LinkGraph::persist_changes`] originally wrote them (node
/// removals, then node upserts, then edge removals, then edge upserts,
/// oldest generation first), so applying them in order never references an
/// edge's endpoint before that endpoint itself has been applied. `generation`
/// is the generation this delta brings the caller up to date with, recorded
/// as the new last-seen value once applied.
struct GraphDelta {
    changes: Vec<GraphChange>,
    generation: u64,
}

/// Backend-agnostic persistence port for one vault's link graph.
/// [`LinkGraph`] holds the in-memory `petgraph` structure and
/// is the sole owner of graph *logic*; every implementation of this trait
/// owns only durable storage of the `GraphNode`/`GraphEdge` records it is
/// handed. Mirrors `vector_store.rs`'s `StoresVectors` shape: `Send + Sync`,
/// `&self` methods over an interior-mutable connection, so a concrete store
/// (for example a `Mutex`-guarded `rusqlite::Connection`) can be shared
/// through a `Box<dyn StoresGraph>` without `LinkGraph` itself needing
/// `&mut` access to it.
trait StoresGraph: Send + Sync {
    /// Reads this store's persisted format identifier and next-edge-id
    /// allocator, or `None` per [`StoredGraphMetadata`]'s documented
    /// all-or-nothing contract.
    fn read_metadata(&self) -> Option<StoredGraphMetadata>;

    /// Reads every currently stored node and `(store_id, edge)` pair, or
    /// `None` when any record cannot be read or decoded: any such problem
    /// aborts the whole load rather than admitting a partially
    /// reconstructed graph, matching the former JSON cache's same
    /// all-or-nothing contract.
    fn load_all(&self) -> Option<GraphRecords>;

    /// Removes every stored node and edge record, leaving metadata
    /// untouched: [`LinkGraph::rebuild`] calls this before repopulating,
    /// then [`StoresGraph::persist`] always rewrites metadata afterwards.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the store cannot be cleared.
    fn clear(&self) -> Result<(), SearchError>;

    /// Applies `changes` and unconditionally (re)writes this store's
    /// format identifier and `next_edge_id` allocator, in one atomic
    /// write, so a call that establishes an empty-but-valid graph (an
    /// empty vault's [`LinkGraph::rebuild`]) still records a store that
    /// reopens as `needs_rebuild() == false`.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the write cannot be committed.
    fn persist(&self, changes: &[GraphChange], next_edge_id: u64) -> Result<(), SearchError>;

    /// Returns this store's current generation counter, or
    /// `None` when the backend does not track propagation (`fjall`,
    /// `serde`, always `None`) or when this store has never been persisted
    /// to yet (the same "cannot be trusted" state [`StoresGraph::read_metadata`]
    /// reports as `None`).
    fn current_generation(&self) -> Option<u64>;

    /// Returns every node/edge upsert or removal persisted strictly after
    /// `since`, in application order, alongside the generation this delta
    /// brings the caller up to date with.
    ///
    /// `None` means either: the backend does not track propagation
    /// (`fjall`, `serde`), or `since` is older than this store's oldest
    /// retained history, so the caller must fall back to
    /// [`StoresGraph::load_all`] (a full reload) instead of a partial
    /// catch-up. Both cases collapse to `None` deliberately: [`LinkGraph`]
    /// already knows its own configured backend and only ever calls this
    /// for `sqlite`, so no caller needs to distinguish "unsupported" from
    /// "too stale" from the return value alone.
    fn changes_since(&self, since: u64) -> Option<GraphDelta>;
}

/// Node and edge identities touched by one call to [`LinkGraph::upsert_note`],
/// [`LinkGraph::remove_note`], or [`LinkGraph::rebuild`], collected as the
/// in-memory graph mutates and applied to the `fjall` store in one batch at
/// the end of the call. This is what makes a single-note edit write only
/// its changed records rather than the whole graph.
#[derive(Default)]
struct PendingChanges {
    upserted_nodes: Vec<NodeIndex>,
    removed_node_keys: Vec<String>,
    upserted_edges: Vec<EdgeIndex>,
    removed_edge_ids: Vec<u64>,
}

/// In-memory wikilink graph over a vault, persisted incrementally behind
/// the [`StoresGraph`] trait boundary alongside the vault's other derived
/// state.
pub struct LinkGraph {
    graph: StableDiGraph<GraphNode, EdgeRecord>,
    index: HashMap<String, NodeIndex>,
    /// `store_id` to edge index, maintained alongside every local or
    /// externally-applied edge add/remove: the sole reason
    /// this exists is applying an externally-sourced
    /// [`GraphChange::RemoveEdge`] in O(1) rather than a linear scan over
    /// every edge in the graph, which would make catch-up cost scale with
    /// total graph size again, defeating the whole point of a partial
    /// delta.
    edge_by_store_id: HashMap<u64, EdgeIndex>,
    store: Box<dyn StoresGraph>,
    /// The backend this instance was opened with. `LinkGraph` keeps this
    /// rather than inferring it from `store`'s trait-object type, so
    /// cross-instance catch-up can cheaply decide whether to
    /// attempt it at all without downcasting `Box<dyn StoresGraph>`.
    backend: GraphBackend,
    next_edge_id: u64,
    needs_rebuild: bool,
    /// The generation this instance has applied changes up to. Meaningless
    /// when `backend` is not [`GraphBackend::Sqlite`],
    /// left at `0` and never consulted in that case.
    last_seen_generation: u64,
    /// The kind of this instance's most recent cross-instance catch-up, or
    /// `None` if it has not yet needed one. Meaningless when `backend` is
    /// not [`GraphBackend::Sqlite`].
    last_catch_up: Option<CatchUpKind>,
}

/// A freshly loaded snapshot from a graph store: everything
/// [`LinkGraph::try_from`] needs on first open, and everything a
/// cross-instance catch-up's full-reload fallback needs to
/// replace an existing instance's in-memory state with. Factored out so
/// both call sites share one loading path rather than two.
struct LoadedSnapshot {
    graph: StableDiGraph<GraphNode, EdgeRecord>,
    index: HashMap<String, NodeIndex>,
    edge_by_store_id: HashMap<u64, EdgeIndex>,
    next_edge_id: u64,
    generation: u64,
}

/// Attempts a full load from `store`: its metadata plus every node/edge
/// record, reconstructed into an in-memory graph. `None` per the same
/// all-or-nothing "cannot be trusted" contract
/// [`StoresGraph::read_metadata`]/[`StoresGraph::load_all`] themselves
/// document (missing, unreadable, malformed, or format-mismatched).
fn load_snapshot(store: &dyn StoresGraph) -> Option<LoadedSnapshot> {
    let metadata = store.read_metadata()?;
    let (graph, index, edge_by_store_id) = store.load_all().and_then(build_graph)?;
    Some(LoadedSnapshot {
        graph,
        index,
        edge_by_store_id,
        next_edge_id: metadata.next_edge_id,
        generation: metadata.generation,
    })
}

impl TryFrom<LinkGraphConfig> for LinkGraph {
    type Error = SearchError;

    /// Opens the graph's configured store.
    ///
    /// A store with no valid metadata yet (freshly created, written by an
    /// incompatible [`STORE_FORMAT`], or simply not yet populated because
    /// the vault's configured [`GraphBackend`] just changed) yields an
    /// empty graph with [`LinkGraph::needs_rebuild`] set, rather than
    /// failing construction: the store is disposable derived state and the
    /// caller can always repopulate it with [`LinkGraph::rebuild`]. A store
    /// that cannot even be opened does fail construction; see
    /// `fjall_store`'s documentation for why this is a deliberate
    /// narrowing of the former JSON cache's "never fails" contract.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the underlying store cannot be opened,
    /// and [`SearchError::GraphLocked`] when the `fjall` backend's store is
    /// already held open by another process.
    fn try_from(value: LinkGraphConfig) -> Result<Self, Self::Error> {
        let store: Box<dyn StoresGraph> = match value.backend {
            GraphBackend::Fjall => Box::new(FjallGraphStore::open(&value.store_directory)?),
            GraphBackend::Sqlite => Box::new(SqliteGraphStore::open(&value.store_directory)?),
            GraphBackend::Serde => Box::new(SerdeGraphStore::open(&value.store_directory)?),
        };

        Ok(match load_snapshot(store.as_ref()) {
            Some(snapshot) => Self {
                graph: snapshot.graph,
                index: snapshot.index,
                edge_by_store_id: snapshot.edge_by_store_id,
                store,
                backend: value.backend,
                next_edge_id: snapshot.next_edge_id,
                needs_rebuild: false,
                last_seen_generation: snapshot.generation,
                last_catch_up: None,
            },
            None => Self {
                graph: StableDiGraph::new(),
                index: HashMap::new(),
                edge_by_store_id: HashMap::new(),
                store,
                backend: value.backend,
                next_edge_id: 0,
                needs_rebuild: true,
                last_seen_generation: 0,
                last_catch_up: None,
            },
        })
    }
}

/// Reconstructs a [`StableDiGraph`], its path index, and its `store_id`
/// index from the raw records a [`StoresGraph::load_all`] call returned, or
/// `None` when an edge references a node path absent from `nodes`: the same
/// all-or-nothing load contract [`StoresGraph::load_all`] itself documents,
/// just completed one level up once both node and edge records are in hand.
/// Shared by every backend so this reconstruction is written once, not
/// duplicated per implementation.
fn build_graph(records: GraphRecords) -> Option<BuiltGraph> {
    let (nodes, edges) = records;
    let mut graph = StableDiGraph::new();
    let mut index = HashMap::new();
    for node in nodes {
        let path = node.path.clone();
        let idx = graph.add_node(node);
        index.insert(path, idx);
    }
    let mut edge_by_store_id = HashMap::new();
    for (store_id, edge) in edges {
        let from = *index.get(&edge.from)?;
        let to = *index.get(&edge.to)?;
        let edge_idx = graph.add_edge(
            from,
            to,
            EdgeRecord {
                kind: edge.kind,
                store_id,
            },
        );
        edge_by_store_id.insert(store_id, edge_idx);
    }
    Some((graph, index, edge_by_store_id))
}

impl LinkGraph {
    /// Reports whether the graph was constructed from a missing,
    /// unreadable, corrupt, or format-mismatched cache and should be
    /// repopulated from a full vault scan.
    #[must_use]
    pub const fn needs_rebuild(&self) -> bool {
        self.needs_rebuild
    }

    /// Clears the rebuild flag, marking the graph as reflecting a
    /// complete, trustworthy scan of the vault.
    pub fn mark_rebuilt(&mut self) {
        self.needs_rebuild = false;
    }

    /// Upserts a real note into the graph, replacing its previously
    /// recorded outgoing links.
    ///
    /// An existing phantom node at `path` is upgraded to a real note in
    /// place, so edges other notes already hold to it keep pointing at the
    /// same node. Every prior outgoing edge of this note is dropped before
    /// the supplied `links` are wired: an [`ObsidianLink`] with `embed` set
    /// becomes a [`GraphEdgeKind::Embed`] edge, otherwise a
    /// [`GraphEdgeKind::Link`] edge. A link with an empty `target` (a
    /// heading- or block-only link such as `[[#heading]]`) carries no note
    /// destination and is ignored. Self-links are permitted.
    ///
    /// Wikilink targets resolve in Obsidian's spirit: an exact relative
    /// path match, then the target with `.md` appended, then (for a
    /// slash-free target) the known real note whose file stem equals the
    /// target, breaking any tie by the lexicographically smallest path,
    /// deterministic stand-in for Obsidian's shortest-path rule. An
    /// unresolved target creates or reuses a phantom node keyed by the
    /// target text as written.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted
    /// to its store.
    pub fn upsert_note(
        &mut self,
        path: &str,
        title: &str,
        links: &[ObsidianLink],
    ) -> Result<(), SearchError> {
        self.catch_up()?;
        let mut changes = PendingChanges::default();
        let node_idx = self.ensure_real_node(path, title, &mut changes);
        self.clear_outgoing(node_idx, &mut changes);
        self.wire_links(node_idx, links, &mut changes);
        self.persist_changes(&changes, false)
    }

    /// Removes a note from the graph.
    ///
    /// Its outgoing edges are dropped first. A node still referenced by
    /// another note's outgoing edge becomes a phantom (its title reset to
    /// its path) rather than disappearing, matching Obsidian's treatment
    /// of a broken link; otherwise the node is removed outright. Any
    /// phantom node left with no remaining edges is then pruned.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted
    /// to its store.
    pub fn remove_note(&mut self, path: &str) -> Result<(), SearchError> {
        self.catch_up()?;
        let mut changes = PendingChanges::default();
        if let Some(&idx) = self.index.get(path) {
            self.clear_outgoing(idx, &mut changes);
            let has_incoming = self
                .graph
                .edges_directed(idx, Direction::Incoming)
                .next()
                .is_some();
            if has_incoming {
                if let Some(node) = self.graph.node_weight_mut(idx) {
                    node.phantom = true;
                    path.clone_into(&mut node.title);
                }
                changes.upserted_nodes.push(idx);
            } else if let Some(node) = self.graph.remove_node(idx) {
                self.index.remove(&node.path);
                changes.removed_node_keys.push(node.path);
            }
        }
        self.prune_phantom_orphans(&mut changes);
        self.persist_changes(&changes, false)
    }

    /// Rebuilds the entire graph from a full vault scan.
    ///
    /// Clears all existing state, then upserts every `(path, title,
    /// links)` triple in two passes: the first creates every real note
    /// node so the second pass's bare-name link resolution can see the
    /// complete set of vault notes regardless of scan order. Clears the
    /// rebuild flag and persists once at the end.
    ///
    /// Does not catch up first, unlike every other public method: a
    /// rebuild is about to replace this instance's entire state
    /// from a fresh vault scan regardless of what a sibling has persisted,
    /// so catching up first would be pointless work immediately discarded.
    /// The `reset` record `persist_changes` writes below is what keeps a
    /// *sibling's* next catch-up correct despite this instance skipping its
    /// own.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the store cannot be cleared, or the
    /// rebuilt graph cannot be persisted.
    pub fn rebuild(
        &mut self,
        notes: &[(String, String, Vec<ObsidianLink>)],
    ) -> Result<(), SearchError> {
        self.graph.clear();
        self.index.clear();
        self.edge_by_store_id.clear();
        self.next_edge_id = 0;
        self.store.clear()?;

        let mut changes = PendingChanges::default();
        for (path, title, _links) in notes {
            self.ensure_real_node(path, title, &mut changes);
        }
        for (path, _title, links) in notes {
            if let Some(&node_idx) = self.index.get(path.as_str()) {
                self.wire_links(node_idx, links, &mut changes);
            }
        }

        self.mark_rebuilt();
        self.persist_changes(&changes, true)
    }

    /// Returns nodes and edges reachable from `from` within `depth` hops
    /// following `direction`.
    ///
    /// The returned view lists every node discovered during the traversal
    /// (including `from` itself) plus every edge walked to reach it: an
    /// edge is recorded once, the first time either of its ends is
    /// encountered while its source has not yet reached the depth bound.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidDepth`] when `depth` is outside
    /// `1..=4`, and [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn neighbours(
        &mut self,
        from: &str,
        depth: u32,
        direction: GraphDirection,
    ) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        if !(1..=4).contains(&depth) {
            return Err(SearchError::InvalidDepth { depth });
        }
        let start = self.require_node(from)?;

        let mut visited: HashMap<NodeIndex, u32> = HashMap::new();
        visited.insert(start, 0);
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(start);
        let mut recorded: HashSet<EdgeIndex> = HashSet::new();
        let mut edges: Vec<GraphEdge> = Vec::new();

        while let Some(node) = queue.pop_front() {
            let node_depth = visited.get(&node).copied().unwrap_or(0);
            if node_depth >= depth {
                continue;
            }
            for &dir in direction.directions() {
                for edge_ref in self.graph.edges_directed(node, dir) {
                    let edge_id = edge_ref.id();
                    let other = if dir == Direction::Outgoing {
                        edge_ref.target()
                    } else {
                        edge_ref.source()
                    };
                    if recorded.insert(edge_id)
                        && let Some((_, _, view)) = self.edge_view(edge_id)
                    {
                        edges.push(view);
                    }
                    if let Entry::Vacant(slot) = visited.entry(other) {
                        slot.insert(node_depth + 1);
                        queue.push_back(other);
                    }
                }
            }
        }

        let nodes: Vec<GraphNode> = visited
            .keys()
            .filter_map(|&idx| self.graph.node_weight(idx).cloned())
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// Returns the notes and edges that link or embed directly to `from`.
    /// Equivalent to `neighbours(from, 1, GraphDirection::In)`,
    /// kept as its own method for the tool surface.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn backlinks(&mut self, from: &str) -> Result<GraphView, SearchError> {
        self.neighbours(from, 1, GraphDirection::In)
    }

    /// Returns the shortest path between `from` and `to` by hop count,
    /// following `direction`.
    ///
    /// Ties are broken deterministically by exploring each node's
    /// neighbours in lexicographic order of their path, so the same graph
    /// always yields the same route. Returns an empty view when no route
    /// exists.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` or `to` is not in
    /// the graph.
    pub fn path_between(
        &mut self,
        from: &str,
        to: &str,
        direction: GraphDirection,
    ) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let start = self.require_node(from)?;
        let goal = self.require_node(to)?;

        if start == goal {
            let nodes = match self.graph.node_weight(start) {
                Some(node) => vec![node.clone()],
                None => Vec::new(),
            };
            return Ok(Self::into_view(nodes, Vec::new()));
        }

        let Some(route) = self.shortest_route(start, goal, direction) else {
            return Ok(GraphView {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        };

        let mut node_set: HashSet<NodeIndex> = HashSet::new();
        node_set.insert(start);
        let mut edges: Vec<GraphEdge> = Vec::new();
        for edge_id in route {
            if let Some((source, target, view)) = self.edge_view(edge_id) {
                node_set.insert(source);
                node_set.insert(target);
                edges.push(view);
            }
        }
        let nodes: Vec<GraphNode> = node_set
            .into_iter()
            .filter_map(|idx| self.graph.node_weight(idx).cloned())
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// Returns every real (non-phantom) note with neither incoming nor
    /// outgoing edges. Phantom nodes never appear: an unresolved
    /// target with nothing left referencing it is pruned on removal, and
    /// one still referenced is, by definition, not without edges.
    ///
    /// # Errors
    ///
    /// Returns a storage error when a cross-instance catch-up cannot read
    /// from the store; the traversal itself never fails.
    pub fn orphans(&mut self) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let nodes: Vec<GraphNode> = self
            .graph
            .node_indices()
            .filter_map(|idx| {
                let node = self.graph.node_weight(idx)?;
                let no_outgoing = self
                    .graph
                    .edges_directed(idx, Direction::Outgoing)
                    .next()
                    .is_none();
                let no_incoming = self
                    .graph
                    .edges_directed(idx, Direction::Incoming)
                    .next()
                    .is_none();
                (!node.phantom && no_outgoing && no_incoming).then(|| node.clone())
            })
            .collect();
        Ok(Self::into_view(nodes, Vec::new()))
    }

    /// Returns the sorted phantom-node paths `from` links or embeds to: the
    /// unresolved wikilink targets that back `links_read`'s
    /// `unresolved` reporting.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn unresolved_targets(&mut self, from: &str) -> Result<Vec<String>, SearchError> {
        self.catch_up()?;
        let idx = self.require_node(from)?;
        let mut targets: Vec<String> = self
            .graph
            .edges_directed(idx, Direction::Outgoing)
            .filter_map(|edge_ref| {
                self.graph
                    .node_weight(edge_ref.target())
                    .filter(|node| node.phantom)
                    .map(|node| node.path.clone())
            })
            .collect();
        targets.sort_unstable();
        targets.dedup();
        Ok(targets)
    }
}

impl LinkGraph {
    /// Resolves `changes` against the current in-memory graph into
    /// backend-agnostic [`GraphChange`] records, then hands them to the
    /// configured [`StoresGraph`] backend in one call: removed node and
    /// edge keys become removals, upserted nodes and edges are
    /// (re-)encoded from their current in-memory state. `StoresGraph::
    /// persist` unconditionally refreshes the store's format identifier
    /// and edge-id allocator alongside them.
    ///
    /// `reset` is `true` only from [`LinkGraph::rebuild`], which clears the
    /// store directly before repopulating it: a leading
    /// [`GraphChange::Reset`] record tells a propagation-tracking backend
    /// (`sqlite`) to force any sibling's next catch-up to a full reload
    /// rather than a partial merge, since the ordinary upsert/remove
    /// vocabulary cannot represent "the old state was cleared outright".
    ///
    /// After a successful write, also advances this instance's own
    /// `last_seen_generation` to match when the backend
    /// tracks one: without this, this instance's own next call would see
    /// the store's generation has advanced past what it last recorded (by
    /// its own write) and redundantly "catch up" to a change it already
    /// has in memory.
    fn persist_changes(
        &mut self,
        changes: &PendingChanges,
        reset: bool,
    ) -> Result<(), SearchError> {
        let mut records: Vec<GraphChange> = Vec::with_capacity(
            usize::from(reset)
                + changes.removed_node_keys.len()
                + changes.upserted_nodes.len()
                + changes.removed_edge_ids.len()
                + changes.upserted_edges.len(),
        );

        if reset {
            records.push(GraphChange::Reset);
        }
        for path in &changes.removed_node_keys {
            records.push(GraphChange::RemoveNode(path.clone()));
        }
        for &idx in &changes.upserted_nodes {
            if let Some(node) = self.graph.node_weight(idx) {
                records.push(GraphChange::UpsertNode(node.clone()));
            }
        }
        for &store_id in &changes.removed_edge_ids {
            records.push(GraphChange::RemoveEdge { store_id });
        }
        for &idx in &changes.upserted_edges {
            if let Some((from_idx, to_idx)) = self.graph.edge_endpoints(idx)
                && let Some(record) = self.graph.edge_weight(idx).copied()
                && let Some(from_node) = self.graph.node_weight(from_idx)
                && let Some(to_node) = self.graph.node_weight(to_idx)
            {
                records.push(GraphChange::UpsertEdge {
                    store_id: record.store_id,
                    edge: GraphEdge {
                        from: from_node.path.clone(),
                        to: to_node.path.clone(),
                        kind: record.kind,
                    },
                });
            }
        }

        self.store.persist(&records, self.next_edge_id)?;
        if self.backend == GraphBackend::Sqlite
            && let Some(generation) = self.store.current_generation()
        {
            self.last_seen_generation = generation;
        }
        Ok(())
    }

    /// Returns every node and edge currently in the graph, in the same
    /// deterministic order as a query result.
    ///
    /// Exposed beyond the crate (a deliberate, minimal visibility change)
    /// so a combined vault search service can report total node and edge
    /// counts for `query_index_status` without duplicating the
    /// graph's internal traversal.
    ///
    /// # Errors
    ///
    /// Returns a storage error when a cross-instance catch-up cannot read
    /// from the store.
    pub fn full_view(&mut self) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let nodes: Vec<GraphNode> = self.graph.node_weights().cloned().collect();
        let edges: Vec<GraphEdge> = self
            .graph
            .edge_indices()
            .filter_map(|id| self.edge_view(id).map(|(_, _, edge)| edge))
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// This instance's cross-instance propagation status, or `None` when
    /// the configured backend does not track one
    /// (`fjall`, `serde`).
    #[must_use]
    pub fn sync_status(&self) -> Option<SyncStatus> {
        (self.backend == GraphBackend::Sqlite).then_some(SyncStatus {
            generation: self.last_seen_generation,
            last_catch_up: self.last_catch_up,
        })
    }

    /// Brings this instance's in-memory graph up to date with any changes a
    /// sibling instance has persisted since this instance last checked. A
    /// no-op for every backend except `sqlite`, and a
    /// no-op even for `sqlite` when nothing has actually changed (the cheap
    /// common case: one indexed point lookup). Called at the start of
    /// every other public method.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the store cannot be read.
    fn catch_up(&mut self) -> Result<(), SearchError> {
        if self.backend != GraphBackend::Sqlite {
            return Ok(());
        }
        let Some(current) = self.store.current_generation() else {
            // Never persisted to yet: nothing a sibling could have written
            // either, so there is nothing to catch up to.
            return Ok(());
        };
        if current <= self.last_seen_generation {
            return Ok(());
        }

        if let Some(delta) = self.store.changes_since(self.last_seen_generation) {
            self.apply_external_changes(delta.changes);
            self.last_seen_generation = delta.generation;
            self.last_catch_up = Some(CatchUpKind::Partial);
        } else {
            let snapshot =
                load_snapshot(self.store.as_ref()).ok_or_else(|| SearchError::GraphStorage {
                    path: "sqlite graph store".to_owned(),
                    source: std::io::Error::other(
                        "became unreadable during a cross-instance catch-up's full-reload \
                         fallback, having been readable moments earlier when its generation \
                         was checked",
                    ),
                })?;
            self.graph = snapshot.graph;
            self.index = snapshot.index;
            self.edge_by_store_id = snapshot.edge_by_store_id;
            self.next_edge_id = snapshot.next_edge_id;
            self.last_seen_generation = snapshot.generation;
            self.last_catch_up = Some(CatchUpKind::FullReload);
        }
        self.needs_rebuild = false;
        Ok(())
    }

    /// Applies a sequence of externally-persisted [`GraphChange`] records
    /// (from a sibling instance's write, via
    /// [`StoresGraph::changes_since`]) to this instance's in-memory graph,
    /// without re-persisting them: they are already durable in the shared
    /// store. Every node/edge referenced by an upsert is resolved by its
    /// exact path (never the fuzzy bare-name matching
    /// [`LinkGraph::resolve_target`] does for a raw wikilink target,
    /// because these paths are already fully resolved by whichever
    /// instance originated the change): find-or-create, never a bare-name
    /// search. Idempotent: reapplying an already-applied change (possible
    /// if `self.store.current_generation()` was read slightly before
    /// `load_all()` completed during this instance's own initial open) is
    /// always safe, never doubles a node or drops an edge.
    ///
    /// [`GraphChange::Reset`] never appears here: [`LinkGraph::catch_up`]
    /// only reaches this method via the `Some(delta)` branch, and
    /// [`StoresGraph::changes_since`] itself guarantees a `Reset` anywhere
    /// in the requested range yields `None` instead, forcing the full
    /// reload branch. `next_edge_id` is advanced past every `store_id`
    /// this delta references: without this, a later *local* mutation on
    /// this instance could allocate a `store_id` a sibling has already
    /// used, silently colliding two unrelated edges into one stored row.
    fn apply_external_changes(&mut self, changes: Vec<GraphChange>) {
        for change in changes {
            match change {
                GraphChange::Reset => {}
                GraphChange::RemoveNode(path) => {
                    if let Some(idx) = self.index.remove(&path) {
                        self.graph.remove_node(idx);
                    }
                }
                GraphChange::UpsertNode(node) => {
                    let idx = self.ensure_node_by_exact_path(&node.path);
                    if let Some(weight) = self.graph.node_weight_mut(idx) {
                        *weight = node;
                    }
                }
                GraphChange::RemoveEdge { store_id } => {
                    if let Some(idx) = self.edge_by_store_id.remove(&store_id) {
                        self.graph.remove_edge(idx);
                    }
                }
                GraphChange::UpsertEdge { store_id, edge } => {
                    self.next_edge_id = self.next_edge_id.max(store_id.saturating_add(1));
                    if let Some(&existing) = self.edge_by_store_id.get(&store_id) {
                        // Never actually reached today (a `store_id` is
                        // never reused by `wire_links`'s originating
                        // instance either), kept for genuine idempotency
                        // rather than assuming that invariant holds
                        // forever.
                        self.graph.remove_edge(existing);
                    }
                    let from = self.ensure_node_by_exact_path(&edge.from);
                    let to = self.ensure_node_by_exact_path(&edge.to);
                    let edge_idx = self.graph.add_edge(
                        from,
                        to,
                        EdgeRecord {
                            kind: edge.kind,
                            store_id,
                        },
                    );
                    self.edge_by_store_id.insert(store_id, edge_idx);
                }
            }
        }
    }

    /// Finds the node at exactly `path`, or creates a new phantom node
    /// there: the resolution [`LinkGraph::apply_external_changes`] needs
    /// for an edge's `from`/`to` endpoint, already a fully resolved path
    /// (never a raw wikilink target), so no bare-name fallback applies.
    /// Every [`GraphChange::UpsertEdge`] delta record is preceded by its
    /// endpoints' own [`GraphChange::UpsertNode`] records within the same
    /// `persist` call (`persist_changes` always orders node changes before
    /// edge changes), so this is expected to find an existing node in
    /// practice; the phantom fallback exists for genuine defensiveness, not
    /// because it is a normal path.
    fn ensure_node_by_exact_path(&mut self, path: &str) -> NodeIndex {
        if let Some(&idx) = self.index.get(path) {
            return idx;
        }
        let idx = self.graph.add_node(GraphNode {
            path: path.to_owned(),
            title: path.to_owned(),
            phantom: true,
        });
        self.index.insert(path.to_owned(), idx);
        idx
    }

    /// Resolves `path` to its node index, upgrading an existing phantom to
    /// a real note in place, or creating a new real node. Records the node
    /// as touched in `changes` either way, so it is (re-)persisted.
    fn ensure_real_node(
        &mut self,
        path: &str,
        title: &str,
        changes: &mut PendingChanges,
    ) -> NodeIndex {
        let idx = if let Some(&idx) = self.index.get(path) {
            if let Some(node) = self.graph.node_weight_mut(idx) {
                node.phantom = false;
                title.clone_into(&mut node.title);
            }
            idx
        } else {
            let idx = self.graph.add_node(GraphNode {
                path: path.to_owned(),
                title: title.to_owned(),
                phantom: false,
            });
            self.index.insert(path.to_owned(), idx);
            idx
        };
        changes.upserted_nodes.push(idx);
        idx
    }

    /// Removes every outgoing edge of `node_idx`, recording each removed
    /// edge's persisted identity in `changes`.
    fn clear_outgoing(&mut self, node_idx: NodeIndex, changes: &mut PendingChanges) {
        let outgoing: Vec<EdgeIndex> = self
            .graph
            .edges_directed(node_idx, Direction::Outgoing)
            .map(|edge_ref| edge_ref.id())
            .collect();
        for edge_id in outgoing {
            if let Some(record) = self.graph.remove_edge(edge_id) {
                self.edge_by_store_id.remove(&record.store_id);
                changes.removed_edge_ids.push(record.store_id);
            }
        }
    }

    /// Adds one outgoing edge per link, resolving each target per
    /// [`LinkGraph::upsert_note`]'s documented resolution order. Every new
    /// edge is assigned the next value from `self.next_edge_id`, a
    /// monotonic, store-persisted allocator: this is what lets two
    /// parallel edges (a note linking to the same target twice) each keep
    /// a distinct, stable identity across a persist/reopen round trip.
    fn wire_links(
        &mut self,
        node_idx: NodeIndex,
        links: &[ObsidianLink],
        changes: &mut PendingChanges,
    ) {
        for link in links {
            if link.target.is_empty() {
                continue;
            }
            let target_idx = self.resolve_target(&link.target, changes);
            let kind = if link.embed {
                GraphEdgeKind::Embed
            } else {
                GraphEdgeKind::Link
            };
            let store_id = self.next_edge_id;
            self.next_edge_id += 1;
            let edge_idx = self
                .graph
                .add_edge(node_idx, target_idx, EdgeRecord { kind, store_id });
            self.edge_by_store_id.insert(store_id, edge_idx);
            changes.upserted_edges.push(edge_idx);
        }
    }

    /// Removes every phantom node left with no incoming or outgoing edge.
    fn prune_phantom_orphans(&mut self, changes: &mut PendingChanges) {
        let stale: Vec<NodeIndex> = self
            .graph
            .node_indices()
            .filter(|&idx| {
                self.graph.node_weight(idx).is_some_and(|node| node.phantom)
                    && self
                        .graph
                        .edges_directed(idx, Direction::Outgoing)
                        .next()
                        .is_none()
                    && self
                        .graph
                        .edges_directed(idx, Direction::Incoming)
                        .next()
                        .is_none()
            })
            .collect();
        for idx in stale {
            if let Some(node) = self.graph.remove_node(idx) {
                self.index.remove(&node.path);
                changes.removed_node_keys.push(node.path);
            }
        }
    }

    /// Resolves a wikilink target to a destination node index: an exact
    /// relative-path match, the target with `.md` appended, then (for a
    /// slash-free target) a bare-name match against known real notes.
    /// Falls back to creating or reusing a phantom node.
    fn resolve_target(&mut self, target: &str, changes: &mut PendingChanges) -> NodeIndex {
        if let Some(idx) = self.exact_or_extended_match(target) {
            return idx;
        }
        if !target.contains('/')
            && let Some(idx) = self.bare_name_match(target)
        {
            return idx;
        }
        self.ensure_phantom(target, changes)
    }

    /// Matches `target` against a node keyed by that exact string, or by
    /// that string with `.md` appended.
    fn exact_or_extended_match(&self, target: &str) -> Option<NodeIndex> {
        self.index
            .get(target)
            .or_else(|| {
                let with_extension = format!("{target}.md");
                self.index.get(&with_extension)
            })
            .copied()
    }

    /// Matches a slash-free `target` against the known real note whose
    /// file stem equals it, preferring the lexicographically smallest
    /// path when several match.
    fn bare_name_match(&self, target: &str) -> Option<NodeIndex> {
        let mut best: Option<(&str, NodeIndex)> = None;
        for (path, &idx) in &self.index {
            if file_stem(path) != target {
                continue;
            }
            if self.graph.node_weight(idx).is_none_or(|node| node.phantom) {
                continue;
            }
            let better = match best {
                Some((current, _)) => path.as_str() < current,
                None => true,
            };
            if better {
                best = Some((path.as_str(), idx));
            }
        }
        best.map(|(_, idx)| idx)
    }

    /// Reuses an existing node keyed by `target`, or creates a new
    /// phantom node for it, recording it in `changes` when newly created.
    fn ensure_phantom(&mut self, target: &str, changes: &mut PendingChanges) -> NodeIndex {
        if let Some(&idx) = self.index.get(target) {
            return idx;
        }
        let idx = self.graph.add_node(GraphNode {
            path: target.to_owned(),
            title: target.to_owned(),
            phantom: true,
        });
        self.index.insert(target.to_owned(), idx);
        changes.upserted_nodes.push(idx);
        idx
    }

    /// Resolves `path` to its node index.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `path` is not in the
    /// graph.
    fn require_node(&self, path: &str) -> Result<NodeIndex, SearchError> {
        self.index
            .get(path)
            .copied()
            .ok_or_else(|| SearchError::UnknownNote {
                path: path.to_owned(),
            })
    }

    /// Builds the externally rendered `GraphEdge` for one internal edge
    /// id, alongside its source and target node indices.
    fn edge_view(&self, edge_id: EdgeIndex) -> Option<(NodeIndex, NodeIndex, GraphEdge)> {
        let (source, target) = self.graph.edge_endpoints(edge_id)?;
        let kind = self.graph.edge_weight(edge_id)?.kind;
        let from = self.graph.node_weight(source)?.path.clone();
        let to = self.graph.node_weight(target)?.path.clone();
        Some((source, target, GraphEdge { from, to, kind }))
    }

    /// Breadth-first search for the shortest `from`-to-`goal` route,
    /// breaking ties by exploring each node's neighbours in lexicographic
    /// path order. Returns the ordered edge ids of the route, or `None`
    /// when `goal` is unreachable.
    fn shortest_route(
        &self,
        start: NodeIndex,
        goal: NodeIndex,
        direction: GraphDirection,
    ) -> Option<Vec<EdgeIndex>> {
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(start);
        let mut predecessor: HashMap<NodeIndex, EdgeIndex> = HashMap::new();
        let mut found = false;

        'search: while let Some(node) = queue.pop_front() {
            let mut candidates: Vec<(NodeIndex, EdgeIndex)> = Vec::new();
            for &dir in direction.directions() {
                for edge_ref in self.graph.edges_directed(node, dir) {
                    let other = if dir == Direction::Outgoing {
                        edge_ref.target()
                    } else {
                        edge_ref.source()
                    };
                    candidates.push((other, edge_ref.id()));
                }
            }
            candidates.sort_by(|(a, _), (b, _)| {
                let a_path = self
                    .graph
                    .node_weight(*a)
                    .map_or("", |node| node.path.as_str());
                let b_path = self
                    .graph
                    .node_weight(*b)
                    .map_or("", |node| node.path.as_str());
                a_path.cmp(b_path)
            });

            for (other, edge_id) in candidates {
                if visited.contains(&other) {
                    continue;
                }
                visited.insert(other);
                predecessor.insert(other, edge_id);
                if other == goal {
                    found = true;
                    break 'search;
                }
                queue.push_back(other);
            }
        }

        if !found {
            return None;
        }

        let mut route: Vec<EdgeIndex> = Vec::new();
        let mut cursor = goal;
        while cursor != start {
            let Some(&edge_id) = predecessor.get(&cursor) else {
                break;
            };
            route.push(edge_id);
            let Some((source, target)) = self.graph.edge_endpoints(edge_id) else {
                break;
            };
            cursor = if target == cursor { source } else { target };
        }
        route.reverse();
        Some(route)
    }

    /// Sorts `nodes` by path and `edges` by `(from, to, kind)`, producing
    /// the deterministic order every query result shares.
    fn into_view(mut nodes: Vec<GraphNode>, mut edges: Vec<GraphEdge>) -> GraphView {
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        edges.sort_by(|a, b| {
            (a.from.as_str(), a.to.as_str(), a.kind.label()).cmp(&(
                b.from.as_str(),
                b.to.as_str(),
                b.kind.label(),
            ))
        });
        GraphView { nodes, edges }
    }
}

/// Returns the final `/`-delimited segment of `path` with its extension
/// removed, matching Obsidian's bare-name link resolution target.
fn file_stem(path: &str) -> &str {
    let segment = path.rsplit('/').next().unwrap_or(path);
    match segment.rsplit_once('.') {
        Some((stem, _extension)) if !stem.is_empty() => stem,
        _ => segment,
    }
}

/// Encodes `value` as JSON for storage, mapping a failure to
/// [`SearchError::GraphStorage`] under `directory`'s label.
fn encode<T: Serialize>(value: &T, directory: &Path) -> Result<Vec<u8>, SearchError> {
    serde_json::to_vec(value).map_err(|source| graph_storage_error(directory, source))
}

/// Builds [`SearchError::GraphStorage`] for a failure against the store at
/// `directory`, preserving this crate's convention (already used for the
/// link graph's own former JSON encoding failures, and for other stores'
/// non-`std::io::Error` failure types) of carrying a non-I/O source through
/// `std::io::Error::other` rather than adding a second source type to the
/// error enum for what is, from an operator's perspective, the same
/// `index/storage` failure.
fn graph_storage_error<E>(directory: &Path, source: E) -> SearchError
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    SearchError::GraphStorage {
        path: directory.display().to_string(),
        source: std::io::Error::other(source),
    }
}
