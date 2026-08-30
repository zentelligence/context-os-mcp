//! `SQLite`-backed [`super::StoresGraph`] implementation
//! (`GraphBackend::Sqlite`): WAL journal mode with an explicit busy
//! timeout, so more than one `contextos` process can hold the same
//! vault's graph store open concurrently — the whole reason this
//! backend exists, and the one property `SqliteVecStore`
//! (`crate::vector_store`, single-process by design) deliberately does not
//! need.
//!
//! # Schema
//!
//! Three tables, one row per record, keyed to match `fjall`'s three
//! keyspaces: `nodes(path PRIMARY KEY, value)`, `edges(store_id PRIMARY
//! KEY, value)`, `metadata(key PRIMARY KEY, value)`. `value` holds the same
//! JSON encoding [`super::encode`] already produces for every other
//! backend, stored as `BLOB` rather than re-derived as native SQL columns:
//! this store's job is durable storage of the records `LinkGraph` hands
//! it, not a second schema for the same domain types.
//!
//! A fourth table, `changelog`, backs cross-instance propagation: one row
//! per [`super::GraphChange`] ever persisted, in application
//! order (`seq`, an autoincrementing rowid), tagged with the `generation`
//! it was persisted at. `nodes`/`edges` stay the materialised "current
//! state" `load_all` reads; `changelog` is the append-only history
//! `changes_since` replays for a sibling instance's partial catch-up.
//! Deliberately not a "tag `nodes`/`edges` rows with their own generation"
//! design: that would need a live schema migration for every pre-existing
//! `sqlite` store, and would still need a separate tombstone mechanism for
//! deletions (a plain `DELETE` leaves nothing to diff against). A single
//! changelog table needs neither: it is new (no migration, `CREATE TABLE
//! IF NOT EXISTS` on an already-populated `nodes`/`edges` pair is enough),
//! and a removal is naturally its own row (`kind = 'remove_node'`/
//! `'remove_edge'`, `value` left `NULL`), no separate tombstone table
//! required. Pruned by generation on every `persist` call
//! ([`RETENTION_GENERATIONS`]) so it cannot grow unbounded; a sibling whose
//! last-seen generation has aged out of what remains gets `None` from
//! `changes_since` and must fall back to a full [`super::StoresGraph::load_all`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, STORE_FORMAT, StoredGraphMetadata,
    StoresGraph, encode, graph_storage_error,
};
use crate::SearchError;

const FILE_NAME: &str = "graph.sqlite3";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const FORMAT_KEY: &str = "format_version";
const NEXT_EDGE_ID_KEY: &str = "next_edge_id";
const GENERATION_KEY: &str = "generation";
/// How many generations of `changelog` history a store retains before
/// pruning the oldest. A starting estimate, not a measurement: large
/// enough that a sibling checking in reasonably often
/// during an active Cowork session always finds a partial catch-up
/// available, small enough that the changelog itself never becomes the
/// dominant cost of this store. A sibling that falls further behind than
/// this simply pays a full reload instead of erroring.
const RETENTION_GENERATIONS: u64 = 10_000;

pub(super) struct SqliteGraphStore {
    connection: Mutex<Connection>,
    directory: PathBuf,
}

impl SqliteGraphStore {
    /// Opens (creating if absent) the `SQLite` database at
    /// `directory/graph.sqlite3`, switches it to WAL journal mode with an
    /// explicit busy timeout so a concurrent open from another process
    /// waits rather than failing outright, and ensures the `nodes`,
    /// `edges`, `metadata`, and `changelog` tables exist.
    ///
    /// # Errors
    ///
    /// Returns a storage error when `directory` cannot be created, or the
    /// database cannot be opened, configured, or migrated.
    pub(super) fn open(directory: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(directory).map_err(|source| SearchError::GraphStorage {
            path: directory.display().to_string(),
            source,
        })?;
        let file_path = directory.join(FILE_NAME);
        let connection = Self::open_connection(&file_path)
            .map_err(|source| graph_storage_error(directory, source))?;

        Ok(Self {
            connection: Mutex::new(connection),
            directory: directory.to_path_buf(),
        })
    }

    /// Opens and initialises one connection, retrying the whole sequence on
    /// `SQLITE_BUSY`/`SQLITE_LOCKED` for up to [`BUSY_TIMEOUT`]'s own
    /// budget.
    ///
    /// `busy_timeout` (set immediately after `Connection::open`, before the
    /// `journal_mode` pragma that itself briefly needs a lock) covers
    /// ordinary lock waits on an already-open connection, but does not
    /// reliably cover the specific race of two connections switching a
    /// brand-new database file to WAL mode and creating its schema at the
    /// same instant: confirmed empirically by
    /// `benches/graph_persistence.rs`'s concurrent-write contention test,
    /// which still hit `DatabaseBusy` here even with `busy_timeout` ordered
    /// first. Retrying the whole bootstrap sequence, not just a single
    /// statement, is the general-purpose fix for that race: the failed
    /// attempt's connection is dropped and a fresh one opened each time,
    /// since a connection that failed partway through WAL setup should not
    /// be reused.
    fn open_connection(file_path: &Path) -> rusqlite::Result<Connection> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match Self::try_open_connection(file_path) {
                Ok(connection) => return Ok(connection),
                Err(error) if is_transient_busy(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn try_open_connection(file_path: &Path) -> rusqlite::Result<Connection> {
        let connection = Connection::open(file_path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (path TEXT PRIMARY KEY, value BLOB NOT NULL);\
             CREATE TABLE IF NOT EXISTS edges (store_id INTEGER PRIMARY KEY, value BLOB NOT NULL);\
             CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value INTEGER NOT NULL);\
             CREATE TABLE IF NOT EXISTS changelog (\
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,\
                 generation INTEGER NOT NULL,\
                 kind TEXT NOT NULL,\
                 key TEXT NOT NULL,\
                 value BLOB\
             );\
             CREATE INDEX IF NOT EXISTS changelog_generation_idx ON changelog(generation);",
        )?;
        Ok(connection)
    }

    fn storage_error<E>(&self, source: E) -> SearchError
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        graph_storage_error(&self.directory, source)
    }

    /// Applies one [`GraphChange`] to the live `nodes`/`edges` tables and
    /// records its own `changelog` row at `generation_value`, within `tx`.
    fn apply_one_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        generation_value: i64,
        change: &GraphChange,
    ) -> Result<(), SearchError> {
        match change {
            GraphChange::Reset => {
                self.record_change(tx, generation_value, "reset", "", None)?;
            }
            GraphChange::RemoveNode(path) => {
                tx.execute("DELETE FROM nodes WHERE path = ?1", params![path])
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "remove_node", path, None)?;
            }
            GraphChange::UpsertNode(node) => {
                let value = encode(node, &self.directory)?;
                tx.execute(
                    "INSERT INTO nodes (path, value) VALUES (?1, ?2) \
                     ON CONFLICT(path) DO UPDATE SET value = excluded.value",
                    params![node.path, value],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(
                    tx,
                    generation_value,
                    "upsert_node",
                    &node.path,
                    Some(&value),
                )?;
            }
            GraphChange::RemoveEdge { store_id } => {
                let store_id_value = i64::try_from(*store_id)
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                tx.execute(
                    "DELETE FROM edges WHERE store_id = ?1",
                    params![store_id_value],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(
                    tx,
                    generation_value,
                    "remove_edge",
                    &store_id.to_string(),
                    None,
                )?;
            }
            GraphChange::UpsertEdge { store_id, edge } => {
                let value = encode(edge, &self.directory)?;
                let store_id_value = i64::try_from(*store_id)
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                tx.execute(
                    "INSERT INTO edges (store_id, value) VALUES (?1, ?2) \
                     ON CONFLICT(store_id) DO UPDATE SET value = excluded.value",
                    params![store_id_value, value],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(
                    tx,
                    generation_value,
                    "upsert_edge",
                    &store_id.to_string(),
                    Some(&value),
                )?;
            }
        }
        Ok(())
    }

    /// Inserts one `changelog` row.
    fn record_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        generation_value: i64,
        kind: &str,
        key: &str,
        value: Option<&[u8]>,
    ) -> Result<(), SearchError> {
        tx.execute(
            "INSERT INTO changelog (generation, kind, key, value) VALUES (?1, ?2, ?3, ?4)",
            params![generation_value, kind, key, value],
        )
        .map_err(|source| graph_storage_error(&self.directory, source))?;
        Ok(())
    }

    /// Unconditionally (re)writes `format_version`, `next_edge_id`, and
    /// `generation` in one call, matching [`StoresGraph::persist`]'s own
    /// documented contract.
    fn write_metadata(
        &self,
        tx: &rusqlite::Transaction<'_>,
        next_edge_id: u64,
        generation_value: i64,
    ) -> Result<(), SearchError> {
        let next_edge_id_value = i64::try_from(next_edge_id)
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        for (key, value) in [
            (FORMAT_KEY, i64::from(STORE_FORMAT)),
            (NEXT_EDGE_ID_KEY, next_edge_id_value),
            (GENERATION_KEY, generation_value),
        ] {
            tx.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        }
        Ok(())
    }

    /// Deletes `changelog` rows older than [`RETENTION_GENERATIONS`].
    fn prune_changelog(
        &self,
        tx: &rusqlite::Transaction<'_>,
        generation: u64,
    ) -> Result<(), SearchError> {
        let retain_after = i64::try_from(generation.saturating_sub(RETENTION_GENERATIONS))
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        tx.execute(
            "DELETE FROM changelog WHERE generation <= ?1",
            params![retain_after],
        )
        .map_err(|source| graph_storage_error(&self.directory, source))?;
        Ok(())
    }

    fn persist_once(&self, changes: &[GraphChange], next_edge_id: u64) -> Result<(), SearchError> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let tx = connection
            .transaction()
            .map_err(|source| graph_storage_error(&self.directory, source))?;

        let previous_generation = read_metadata_u64(&tx, GENERATION_KEY)
            .map_err(|source| graph_storage_error(&self.directory, source))?
            .unwrap_or(0);
        let generation = previous_generation.saturating_add(1);
        let generation_value = i64::try_from(generation)
            .map_err(|source| graph_storage_error(&self.directory, source))?;

        for change in changes {
            self.apply_one_change(&tx, generation_value, change)?;
        }
        self.write_metadata(&tx, next_edge_id, generation_value)?;
        self.prune_changelog(&tx, generation)?;

        tx.commit()
            .map_err(|source| graph_storage_error(&self.directory, source))
    }
}

/// Whether `error` is the transient `SQLITE_BUSY`/`SQLITE_LOCKED` this
/// module retries opening through, rather than a genuine failure (a
/// permissions problem, corrupted file, or malformed schema) that retrying
/// cannot fix.
fn is_transient_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// The [`is_transient_busy`] check, applied through the `rusqlite::Error`
/// [`graph_storage_error`] boxes inside [`SearchError::GraphStorage`]'s
/// `std::io::Error::other` source.
fn is_transient_busy_search_error(error: &SearchError) -> bool {
    match error {
        SearchError::GraphStorage { source, .. } => source
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rusqlite::Error>())
            .is_some_and(is_transient_busy),
        _ => false,
    }
}

/// Reads one `INTEGER` metadata row, or `None` when the key is absent
/// (never written yet) or the value does not fit `u64`.
fn read_metadata_u64(connection: &Connection, key: &str) -> Result<Option<u64>, rusqlite::Error> {
    let value: Option<i64> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|value| u64::try_from(value).ok()))
}

impl StoresGraph for SqliteGraphStore {
    fn read_metadata(&self) -> Option<StoredGraphMetadata> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let format = read_metadata_u64(&connection, FORMAT_KEY).ok()??;
        if u32::try_from(format).ok()? != STORE_FORMAT {
            return None;
        }
        let next_edge_id = read_metadata_u64(&connection, NEXT_EDGE_ID_KEY).ok()??;
        // A `generation` key absent entirely means this store predates the
        // changelog table's introduction: treated leniently as `0`, the
        // oldest possible generation, an in-place additive capability
        // rather than a format mismatch forcing a full rebuild (see this
        // module's own doc comment on why no schema migration is needed for
        // `nodes`/`edges` at all).
        let generation = read_metadata_u64(&connection, GENERATION_KEY)
            .ok()
            .flatten()
            .unwrap_or(0);

        Some(StoredGraphMetadata {
            next_edge_id,
            generation,
        })
    }

    fn load_all(&self) -> Option<GraphRecords> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let mut nodes_stmt = connection.prepare("SELECT value FROM nodes").ok()?;
        let nodes = nodes_stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .ok()?
            .map(|value| {
                let value = value.ok()?;
                serde_json::from_slice::<GraphNode>(&value).ok()
            })
            .collect::<Option<Vec<_>>>()?;

        let mut edges_stmt = connection
            .prepare("SELECT store_id, value FROM edges")
            .ok()?;
        let edges = edges_stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .ok()?
            .map(|row| {
                let (store_id, value) = row.ok()?;
                let store_id = u64::try_from(store_id).ok()?;
                let edge = serde_json::from_slice::<GraphEdge>(&value).ok()?;
                Some((store_id, edge))
            })
            .collect::<Option<Vec<_>>>()?;

        Some((nodes, edges))
    }

    fn clear(&self) -> Result<(), SearchError> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        connection
            .execute_batch("DELETE FROM nodes; DELETE FROM edges;")
            .map_err(|source| self.storage_error(source))
    }

    /// Persists `changes`, retrying the whole transaction on
    /// `SQLITE_BUSY`/`SQLITE_LOCKED` for up to [`BUSY_TIMEOUT`]'s own
    /// budget, the same defence [`Self::open_connection`] already applies
    /// to opening: this connection's own `busy_timeout` pragma covers an
    /// ordinary lock wait, but a sibling instance's concurrent writer
    /// transaction can still surface as an immediate `DatabaseBusy` here
    /// rather than a retried wait, particularly under CI-runner disk
    /// contention. A failed attempt's transaction is dropped (rolling back)
    /// before the next attempt begins a fresh one.
    fn persist(&self, changes: &[GraphChange], next_edge_id: u64) -> Result<(), SearchError> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match self.persist_once(changes, next_edge_id) {
                Ok(()) => return Ok(()),
                Err(error)
                    if is_transient_busy_search_error(&error) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn current_generation(&self) -> Option<u64> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        read_metadata_u64(&connection, GENERATION_KEY).ok()?
    }

    fn changes_since(&self, since: u64) -> Option<GraphDelta> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        let current = read_metadata_u64(&connection, GENERATION_KEY).ok()??;
        if since >= current {
            return Some(GraphDelta {
                changes: Vec::new(),
                generation: current,
            });
        }
        // Anything at or before this generation may already be pruned:
        // completeness cannot be guaranteed, so the caller must fall back
        // to a full reload rather than risk an incomplete partial delta.
        let oldest_safe = current.saturating_sub(RETENTION_GENERATIONS);
        if since < oldest_safe {
            return None;
        }

        let since_value = i64::try_from(since).ok()?;

        // A `rebuild` cleared the store directly (`StoresGraph::clear`)
        // somewhere in this range: the ordinary upsert/remove vocabulary
        // cannot represent that, so a partial merge here would silently
        // keep records the rebuild actually dropped. Force the full-reload
        // fallback instead of returning an incomplete delta.
        let reset_in_range: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM changelog WHERE generation > ?1 AND kind = 'reset')",
                params![since_value],
                |row| row.get(0),
            )
            .ok()?;
        if reset_in_range {
            return None;
        }

        let mut stmt = connection
            .prepare(
                "SELECT kind, key, value FROM changelog \
                 WHERE generation > ?1 ORDER BY seq",
            )
            .ok()?;
        let changes = stmt
            .query_map(params![since_value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .ok()?
            .map(|row| {
                let (kind, key, value) = row.ok()?;
                decode_changelog_row(&kind, &key, value)
            })
            .collect::<Option<Vec<GraphChange>>>()?;

        Some(GraphDelta {
            changes,
            generation: current,
        })
    }
}

/// Decodes one `changelog` row back into the [`GraphChange`] it recorded,
/// or `None` on any malformed row (unrecognised `kind`, non-numeric edge
/// `key`, or undecodable JSON `value`): the same all-or-nothing contract
/// every other read in this module shares, aborting the whole
/// `changes_since` call rather than admitting a partially reconstructed
/// delta.
fn decode_changelog_row(kind: &str, key: &str, value: Option<Vec<u8>>) -> Option<GraphChange> {
    match kind {
        "remove_node" => Some(GraphChange::RemoveNode(key.to_owned())),
        "upsert_node" => {
            let node: GraphNode = serde_json::from_slice(&value?).ok()?;
            Some(GraphChange::UpsertNode(node))
        }
        "remove_edge" => {
            let store_id: u64 = key.parse().ok()?;
            Some(GraphChange::RemoveEdge { store_id })
        }
        "upsert_edge" => {
            let store_id: u64 = key.parse().ok()?;
            let edge: GraphEdge = serde_json::from_slice(&value?).ok()?;
            Some(GraphChange::UpsertEdge { store_id, edge })
        }
        _ => None,
    }
}
