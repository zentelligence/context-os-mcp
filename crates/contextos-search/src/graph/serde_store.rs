//! `serde`-backed [`super::StoresGraph`] implementation
//! (`GraphBackend::Serde`): a single JSON file, rewritten whole on every
//! persisted change.
//!
//! Intended for a small, starter, or experimental vault, not concurrent
//! multi-process use: a plain file has no locking of its own, unlike
//! `fjall` (an exclusive lock) or `sqlite` (WAL mode plus a busy timeout).
//! Kept as simple as possible in exchange for that narrower scope: no
//! incremental on-disk writes, no keyspaces, nothing beyond "read the whole
//! document in, write the whole document out."

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, STORE_FORMAT, StoredGraphMetadata,
    StoresGraph, graph_storage_error,
};
use crate::SearchError;

const FILE_NAME: &str = "graph.json";

/// The whole on-disk shape of a `serde`-backed graph store: one JSON
/// document holding the format identifier, the edge-id allocator, and
/// every current node and edge record.
#[derive(Serialize, Deserialize)]
struct SerdeDocument {
    format: u32,
    next_edge_id: u64,
    nodes: Vec<GraphNode>,
    edges: Vec<SerdeEdgeRecord>,
}

#[derive(Serialize, Deserialize)]
struct SerdeEdgeRecord {
    store_id: u64,
    edge: GraphEdge,
}

/// The in-memory mirror kept between `persist` calls, so a call carrying
/// only a delta (per [`GraphChange`]) can still emit a complete document.
/// `valid` is `false` when no on-disk document was ever successfully
/// loaded (missing file, unreadable, malformed JSON, or a format mismatch):
/// every such case reports [`StoresGraph::read_metadata`] as `None`, the
/// same all-or-nothing "cannot be trusted" contract every other backend
/// shares.
struct SerdeState {
    nodes: HashMap<String, GraphNode>,
    edges: HashMap<u64, GraphEdge>,
    next_edge_id: u64,
    valid: bool,
}

pub(super) struct SerdeGraphStore {
    file_path: PathBuf,
    state: Mutex<SerdeState>,
}

impl SerdeGraphStore {
    /// Opens (without creating) the JSON document at
    /// `directory/graph.json`, creating `directory` itself if absent so a
    /// later `persist` call has somewhere to write. A missing, unreadable,
    /// malformed, or format-mismatched document is not an error here: it
    /// simply yields an empty, `valid: false` starting state, exactly as
    /// [`super::LinkGraph::try_from`] expects from every backend when the
    /// store cannot yet be trusted.
    ///
    /// # Errors
    ///
    /// Returns a storage error when `directory` does not exist and cannot
    /// be created.
    pub(super) fn open(directory: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(directory).map_err(|source| SearchError::GraphStorage {
            path: directory.display().to_string(),
            source,
        })?;
        let file_path = directory.join(FILE_NAME);

        let state = std::fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SerdeDocument>(&bytes).ok())
            .filter(|document| document.format == STORE_FORMAT)
            .map_or_else(
                || SerdeState {
                    nodes: HashMap::new(),
                    edges: HashMap::new(),
                    next_edge_id: 0,
                    valid: false,
                },
                |document| SerdeState {
                    nodes: document
                        .nodes
                        .into_iter()
                        .map(|node| (node.path.clone(), node))
                        .collect(),
                    edges: document
                        .edges
                        .into_iter()
                        .map(|record| (record.store_id, record.edge))
                        .collect(),
                    next_edge_id: document.next_edge_id,
                    valid: true,
                },
            );

        Ok(Self {
            file_path,
            state: Mutex::new(state),
        })
    }
}

impl StoresGraph for SerdeGraphStore {
    fn read_metadata(&self) -> Option<StoredGraphMetadata> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.valid.then_some(StoredGraphMetadata {
            next_edge_id: state.next_edge_id,
            generation: 0,
        })
    }

    fn load_all(&self) -> Option<GraphRecords> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let nodes = state.nodes.values().cloned().collect();
        let edges = state
            .edges
            .iter()
            .map(|(&store_id, edge)| (store_id, edge.clone()))
            .collect();
        Some((nodes, edges))
    }

    /// Clears the in-memory mirror only; the on-disk document is left
    /// untouched until the next `persist` call rewrites it whole. A crash
    /// between this call and the following `persist` (as
    /// `LinkGraph::rebuild` always issues) therefore leaves the *previous*
    /// document fully intact on reopen, rather than an emptied-but-not-yet-
    /// repopulated one: strictly safer for a whole-file-rewrite backend
    /// than clearing on disk ahead of the rewrite would be.
    fn clear(&self) -> Result<(), SearchError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.nodes.clear();
        state.edges.clear();
        Ok(())
    }

    fn persist(&self, changes: &[GraphChange], next_edge_id: u64) -> Result<(), SearchError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for change in changes {
            match change {
                // `serde` already loses data outright under concurrent
                // instances (see `current_generation` above); there is no
                // sibling for a `Reset` marker to matter to.
                GraphChange::Reset => {}
                GraphChange::RemoveNode(path) => {
                    state.nodes.remove(path);
                }
                GraphChange::UpsertNode(node) => {
                    state.nodes.insert(node.path.clone(), node.clone());
                }
                GraphChange::RemoveEdge { store_id } => {
                    state.edges.remove(store_id);
                }
                GraphChange::UpsertEdge { store_id, edge } => {
                    state.edges.insert(*store_id, edge.clone());
                }
            }
        }
        state.next_edge_id = next_edge_id;
        state.valid = true;

        let document = SerdeDocument {
            format: STORE_FORMAT,
            next_edge_id,
            nodes: state.nodes.values().cloned().collect(),
            edges: state
                .edges
                .iter()
                .map(|(&store_id, edge)| SerdeEdgeRecord {
                    store_id,
                    edge: edge.clone(),
                })
                .collect(),
        };
        drop(state);

        write_atomically(&self.file_path, &document)
    }

    /// Always `None`: `serde` already loses data outright under concurrent
    /// instances (its own module documentation above), so cross-instance
    /// propagation would be solving the wrong layer for this backend, not
    /// merely unimplemented.
    fn current_generation(&self) -> Option<u64> {
        None
    }

    /// Always `None`, for the same reason as [`SerdeGraphStore::current_generation`].
    fn changes_since(&self, _since: u64) -> Option<GraphDelta> {
        None
    }
}

/// Serialises `document` and writes it to `path` via a same-directory
/// temporary file plus an atomic rename, so a reader never observes a
/// partially written document, matching this project's write-atomicity
/// convention (`.claude/rules/architecture.md`) even though this store's
/// content is disposable, rebuildable derived state.
fn write_atomically(path: &Path, document: &SerdeDocument) -> Result<(), SearchError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let bytes =
        serde_json::to_vec(document).map_err(|source| graph_storage_error(directory, source))?;

    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, &bytes).map_err(|source| SearchError::GraphStorage {
        path: directory.display().to_string(),
        source,
    })?;
    std::fs::rename(&temp_path, path).map_err(|source| SearchError::GraphStorage {
        path: directory.display().to_string(),
        source,
    })
}
