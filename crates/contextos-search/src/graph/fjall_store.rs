//! `fjall`-backed [`super::StoresGraph`] implementation: the default link
//! graph backend (`GraphBackend::Fjall`), an embedded key-value store,
//! single-process-exclusive.

use std::path::{Path, PathBuf};

use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode, Readable};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, STORE_FORMAT, StoredGraphMetadata,
    StoresGraph, encode, graph_storage_error,
};
use crate::SearchError;

const META_FORMAT_KEY: &[u8] = b"format_version";
const META_NEXT_EDGE_ID_KEY: &[u8] = b"next_edge_id";
const NODES_KEYSPACE: &str = "nodes";
const EDGES_KEYSPACE: &str = "edges";
const METADATA_KEYSPACE: &str = "metadata";

/// The three `fjall` keyspaces backing one link graph's persisted state,
/// plus the database handle they share.
pub(super) struct FjallGraphStore {
    database: Database,
    nodes: Keyspace,
    edges: Keyspace,
    metadata: Keyspace,
    directory: PathBuf,
}

impl FjallGraphStore {
    /// Opens (creating if absent) the `fjall` database at `directory` and
    /// its three keyspaces.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database or any keyspace cannot be
    /// opened, for example due to a permissions problem or genuinely
    /// corrupted on-disk state. This is a narrower failure than the former
    /// JSON cache ever raised (a missing or malformed *file* was always
    /// trivially recoverable by treating it as absent), and deliberately
    /// aligns `LinkGraph`'s fallibility with this crate's other derived
    /// stores (`TantivyIndex`, `SqliteVecStore`), both of which already
    /// surface a genuine storage-open failure rather than masking it.
    ///
    /// A store already held open by another process (`fjall::Error::Locked`,
    /// its exclusive lock file could not be acquired) is reported as
    /// [`SearchError::GraphLocked`] rather than the generic
    /// [`SearchError::GraphStorage`], so `VaultSearchService::try_from` can
    /// tell contention on a live store apart from genuine corruption and
    /// degrade only the former.
    pub(super) fn open(directory: &Path) -> Result<Self, SearchError> {
        let database = Database::builder(directory).open().map_err(|source| {
            if matches!(source, fjall::Error::Locked) {
                SearchError::GraphLocked {
                    path: directory.display().to_string(),
                }
            } else {
                graph_storage_error(directory, source)
            }
        })?;
        let nodes = database
            .keyspace(NODES_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        let edges = database
            .keyspace(EDGES_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        let metadata = database
            .keyspace(METADATA_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        Ok(Self {
            database,
            nodes,
            edges,
            metadata,
            directory: directory.to_path_buf(),
        })
    }
}

impl StoresGraph for FjallGraphStore {
    fn read_metadata(&self) -> Option<StoredGraphMetadata> {
        let snapshot = self.database.snapshot();
        let format = read_u32(&snapshot, &self.metadata, META_FORMAT_KEY)?;
        if format != STORE_FORMAT {
            return None;
        }
        let next_edge_id = read_u64(&snapshot, &self.metadata, META_NEXT_EDGE_ID_KEY)?;
        Some(StoredGraphMetadata {
            next_edge_id,
            generation: 0,
        })
    }

    fn load_all(&self) -> Option<GraphRecords> {
        let snapshot = self.database.snapshot();

        let mut nodes = Vec::new();
        for item in snapshot.iter(&self.nodes) {
            let (_, value) = item.into_inner().ok()?;
            let node: GraphNode = serde_json::from_slice(&value).ok()?;
            nodes.push(node);
        }

        let mut edges = Vec::new();
        for item in snapshot.iter(&self.edges) {
            let (key, value) = item.into_inner().ok()?;
            let store_id = decode_edge_key(&key)?;
            let edge: GraphEdge = serde_json::from_slice(&value).ok()?;
            edges.push((store_id, edge));
        }

        Some((nodes, edges))
    }

    fn clear(&self) -> Result<(), SearchError> {
        self.nodes
            .clear()
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        self.edges
            .clear()
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        Ok(())
    }

    fn persist(&self, changes: &[GraphChange], next_edge_id: u64) -> Result<(), SearchError> {
        let mut batch = self.database.batch().durability(Some(PersistMode::SyncAll));

        for change in changes {
            match change {
                // `fjall`'s exclusive lock means a sibling can never be
                // concurrently open to begin with (see `current_generation`
                // above), so there is nothing for a `Reset` marker to do
                // for this backend.
                GraphChange::Reset => {}
                GraphChange::RemoveNode(path) => {
                    batch.remove(&self.nodes, path.as_bytes());
                }
                GraphChange::UpsertNode(node) => {
                    let value = encode(node, &self.directory)?;
                    batch.insert(&self.nodes, node.path.as_bytes(), value);
                }
                GraphChange::RemoveEdge { store_id } => {
                    batch.remove(&self.edges, store_id.to_be_bytes());
                }
                GraphChange::UpsertEdge { store_id, edge } => {
                    let value = encode(edge, &self.directory)?;
                    batch.insert(&self.edges, store_id.to_be_bytes(), value);
                }
            }
        }

        batch.insert(&self.metadata, META_FORMAT_KEY, STORE_FORMAT.to_be_bytes());
        batch.insert(
            &self.metadata,
            META_NEXT_EDGE_ID_KEY,
            next_edge_id.to_be_bytes(),
        );

        batch
            .commit()
            .map_err(|source| graph_storage_error(&self.directory, source))
    }

    /// Always `None`: `fjall`'s exclusive lock means a sibling instance can
    /// never be concurrently open against the same store to begin with, so
    /// cross-instance propagation is structurally unnecessary for this
    /// backend, not merely unimplemented.
    fn current_generation(&self) -> Option<u64> {
        None
    }

    /// Always `None`, for the same reason as [`FjallGraphStore::current_generation`].
    fn changes_since(&self, _since: u64) -> Option<GraphDelta> {
        None
    }
}

fn read_u32(snapshot: &fjall::Snapshot, keyspace: &Keyspace, key: &[u8]) -> Option<u32> {
    let bytes = snapshot.get(keyspace, key).ok().flatten()?;
    let bytes: [u8; 4] = bytes.as_ref().try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

fn read_u64(snapshot: &fjall::Snapshot, keyspace: &Keyspace, key: &[u8]) -> Option<u64> {
    let bytes = snapshot.get(keyspace, key).ok().flatten()?;
    let bytes: [u8; 8] = bytes.as_ref().try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

/// Decodes an `edges` keyspace key back into the `u64` store id it encodes.
fn decode_edge_key(key: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = key.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}
