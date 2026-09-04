//! `SQLite`-backed vector storage for semantic search chunks.
//!
//! `StoresVectors` is the port every vector store implementation satisfies
//! (architecture.md's trait catalogue: `upsert`, `delete`, `similar(k)`);
//! `SqliteVecStore` is the default implementation, backed by `rusqlite`
//! (bundled `SQLite`) and the `sqlite-vec` extension's `vec0` virtual table
//! mechanism.
//!
//! Vectors are derived, rebuildable state: every row here can be
//! regenerated from the vault's markdown content and the configured
//! embedding provider by re-chunking (`chunk_document`) and re-embedding
//! (`EmbedsText::embed`) the affected paths. A corrupt or deleted
//! `vectors.db` is never a data-loss event, only a trigger to rebuild it via
//! `query_index_rebuild`. Intended production location: `.contextos/
//! vectors.db` at the vault root; this module accepts any caller-supplied
//! path so tests use a tempdir, and wiring the production path is a later
//! composition-root concern.
//!
//! # Schema
//!
//! Two tables share the same `rowid` space so a plain `JOIN` recombines
//! metadata and vector:
//!
//! - `chunks`: one row per `(path, ordinal)` chunk identity, holding the
//!   heading trail (JSON-encoded `Vec<String>`) and the SHA-256 content
//!   hash as lowercase hex text. `UNIQUE(path, ordinal)` is the upsert key.
//! - `vec_chunks`: a `sqlite-vec` `vec0` virtual table holding only the
//!   embedding, declared `distance_metric=cosine`. Row `rowid` always
//!   equals the corresponding `chunks.rowid`.
//!
//! `vec0` has no `UPDATE`/`INSERT OR REPLACE` support for an existing
//! `rowid` (confirmed empirically: both fail with a `UNIQUE constraint`
//! error), so [`SqliteVecStore::upsert`] replaces a chunk with a
//! transactional delete-then-insert rather than a single statement.
//!
//! # `path_prefix` filtering
//!
//! [`SimilarityQuery::path_prefix`] is resolved as a `rowid IN (...)`
//! pre-filter against `chunks` inside the same statement as the
//! nearest-neighbour `MATCH` clause (confirmed empirically to combine
//! correctly with `sqlite-vec`'s `k = ?` constraint), not as a post-hoc
//! truncation of an unconditional top-k. Every prefix-scoped
//! [`StoresVectors::similar`] call therefore returns the true top-`k` within
//! the prefix, never an under-count caused by filtering after a fixed-size
//! candidate window. Filtering is done here, at the store layer, rather than
//! by the caller, because the alternative would require either this same
//! query shape duplicated at the caller, or a lossy overfetch-then-truncate.
//! [`SimilarityQuery::exclude_paths`] is resolved the same way, as one
//! `rowid NOT IN (...)` clause per entry in the same statement, so an
//! exclusion never causes the same under-count either.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, Once, PoisonError};

use contextos_core::ContentHash;
use rusqlite::{Connection, OptionalExtension, params};

use crate::SearchError;

/// One vector to upsert: the chunk identity (`path`, `ordinal`) carried by
/// [`crate::Chunk`], its heading trail and content hash, and the embedding
/// vector itself. `path` is the key later `delete` calls remove by.
#[derive(Clone, Copy, Debug)]
pub struct VectorRecord<'a> {
    pub path: &'a str,
    pub ordinal: usize,
    pub heading_context: &'a [String],
    pub content_hash: &'a ContentHash,
    pub vector: &'a [f32],
}

/// One nearest-neighbour search request.
#[derive(Clone, Copy, Debug)]
pub struct SimilarityQuery<'a> {
    /// The already-embedded query vector; must have the store's configured
    /// dimension.
    pub vector: &'a [f32],
    /// Maximum number of hits to return. `0` returns no hits, without error.
    pub k: usize,
    /// Optional forward-slash relative path prefix, matching whole path
    /// segments: `path_prefix = Some("a")` matches `"a"` and everything
    /// under `"a/"`, never `"another.md"`. See the module documentation for
    /// why this filters inside the same query as the similarity search.
    pub path_prefix: Option<&'a str>,
    /// Forward-slash relative path prefixes to exclude, with the same
    /// whole-segment matching as `path_prefix`: `exclude_paths = ["old"]`
    /// excludes `"old"` and everything under `"old/"`, never
    /// `"oldstuff.md"`. A hit is excluded if it matches any entry.
    /// Composable with `path_prefix`: both are applied in the same query.
    pub exclude_paths: &'a [String],
}

/// One ranked nearest-neighbour hit, with enough metadata for a caller to
/// reconstruct a `query_semantic` result row. The chunk's
/// prose itself is not reproduced here: this store only ever holds vectors
/// and their identifying metadata, never chunk text, so re-reading
/// `Chunk::text()` from the source document is the caller's job.
#[derive(Clone, Debug, PartialEq)]
pub struct SimilarityHit {
    pub path: String,
    pub ordinal: usize,
    pub heading_context: Vec<String>,
    pub content_hash: ContentHash,
    /// Cosine similarity in the closed range `[-1.0, 1.0]`; higher is more
    /// similar. Derived from `sqlite-vec`'s cosine distance as
    /// `score = 1.0 - distance` (confirmed empirically: identical or
    /// same-direction vectors score `1.0`, orthogonal vectors score `0.0`,
    /// opposite vectors score `-1.0`).
    pub score: f32,
}

/// Aggregate counts of currently stored chunks, for `query_index_status`
/// reporting: distinct documents holding at least one chunk, and the
/// total chunk count across all of them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VectorStoreStats {
    pub documents: usize,
    pub chunks: usize,
}

/// Port for the vector store behind semantic search.
pub trait StoresVectors: Send + Sync {
    /// Upserts each record: a new `(path, ordinal)` is inserted; an
    /// existing one has its heading context, content hash, and vector
    /// replaced. All records are applied in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::VectorDimensionMismatch`] when a vector's
    /// length does not match the store's configured dimension, and a
    /// storage error when the database cannot be written.
    fn upsert(&self, records: &[VectorRecord<'_>]) -> Result<(), SearchError>;

    /// Removes every chunk stored for `path`. Removing a path with no
    /// stored chunks is not an error.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database cannot be written.
    fn delete(&self, path: &str) -> Result<(), SearchError>;

    /// Returns the top `request.k` chunks by cosine similarity to
    /// `request.vector`, optionally scoped to `request.path_prefix`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::VectorDimensionMismatch`] when the query
    /// vector's length does not match the store's configured dimension, and
    /// a storage error when the database cannot be read.
    fn similar(&self, request: &SimilarityQuery<'_>) -> Result<Vec<SimilarityHit>, SearchError>;

    /// Returns the content hash currently stored for `(path, ordinal)`, or
    /// `None` when no such chunk is stored. Used to skip re-embedding a
    /// chunk whose text has not changed since the last build.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database cannot be read.
    fn existing_hash(&self, path: &str, ordinal: usize) -> Result<Option<ContentHash>, SearchError>;

    /// Removes every stored chunk for `path` whose ordinal is `keep_below`
    /// or greater. Called after re-chunking `path` produces fewer chunks
    /// than before (the document shrank), so trailing chunks from the
    /// previous, longer version never survive as stale, orphaned rows.
    /// Removing a path with no such ordinals is not an error.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database cannot be written.
    fn prune_ordinals_at_or_beyond(&self, path: &str, keep_below: usize) -> Result<(), SearchError>;

    /// Returns aggregate counts of currently stored chunks, for
    /// `query_index_status` reporting.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database cannot be read.
    fn stats(&self) -> Result<VectorStoreStats, SearchError>;
}

/// Trusted construction input for [`SqliteVecStore`].
#[derive(Clone, Debug)]
pub struct SqliteVecConfig {
    /// Filesystem path of the `SQLite` database file. Production wiring
    /// places this at `.contextos/vectors.db`; tests use a tempdir path.
    pub path: PathBuf,
    /// Fixed embedding dimension for every vector this store holds, matching
    /// the configured `EmbedsText` provider's `dimension()`. Must be
    /// greater than zero. Declared once, at construction, because
    /// `sqlite-vec`'s `vec0` virtual table fixes its column width in the
    /// `CREATE VIRTUAL TABLE` statement itself; changing providers to a
    /// different dimension requires a fresh database, rebuilt through
    /// `query_index_rebuild`, not an in-place migration.
    pub dimension: usize,
}

/// Default [`StoresVectors`] implementation over a `sqlite-vec` database.
pub struct SqliteVecStore {
    connection: Mutex<Connection>,
    dimension: usize,
    db_path: String,
}

impl std::fmt::Debug for SqliteVecStore {
    /// Reports only the database path and configured dimension: the
    /// wrapped `rusqlite::Connection` does not implement `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteVecStore")
            .field("db_path", &self.db_path)
            .field("dimension", &self.dimension)
            .finish_non_exhaustive()
    }
}

impl TryFrom<SqliteVecConfig> for SqliteVecStore {
    type Error = SearchError;

    /// Opens (creating if absent) the database at `value.path` and ensures
    /// the `chunks` and `vec_chunks` schema exists.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::VectorDimensionInvalid`] when `dimension` is
    /// zero, [`SearchError::IndexDirectory`] when `value.path`'s parent
    /// directory does not exist and cannot be created (mirroring
    /// `TantivyIndex::try_from`'s own directory creation), and a storage
    /// error when the database cannot be opened or the schema cannot be
    /// created.
    fn try_from(value: SqliteVecConfig) -> Result<Self, Self::Error> {
        if value.dimension == 0 {
            return Err(SearchError::VectorDimensionInvalid {
                dimension: value.dimension,
            });
        }

        register_sqlite_vec();

        let db_path = value.path.display().to_string();
        if let Some(parent) = value.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SearchError::IndexDirectory {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let connection = Connection::open(&value.path).map_err(|source| storage_error(&db_path, source))?;

        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS chunks (\
                    rowid INTEGER PRIMARY KEY, \
                    path TEXT NOT NULL, \
                    ordinal INTEGER NOT NULL, \
                    heading_context TEXT NOT NULL, \
                    content_hash TEXT NOT NULL, \
                    UNIQUE(path, ordinal)\
                )",
                [],
            )
            .map_err(|source| storage_error(&db_path, source))?;
        connection
            .execute("CREATE INDEX IF NOT EXISTS chunks_path_idx ON chunks(path)", [])
            .map_err(|source| storage_error(&db_path, source))?;

        // `vec0`'s column width is a virtual table module argument, parsed
        // as SQL text at `CREATE VIRTUAL TABLE` time; `SQLite` provides no
        // bind-parameter mechanism for module arguments (unlike every other
        // query in this store, which always uses `rusqlite`'s parameterised
        // API). `value.dimension` is validated to be non-zero immediately
        // above and never originates from vault content, a tool argument
        // string, or any other untrusted text, so formatting it into this
        // one DDL statement carries none of the injection risk the
        // parameterised-query rule guards against.
        let create_vec_table = format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(embedding float[{}] distance_metric=cosine)",
            value.dimension
        );
        connection
            .execute(&create_vec_table, [])
            .map_err(|source| storage_error(&db_path, source))?;

        Ok(Self {
            connection: Mutex::new(connection),
            dimension: value.dimension,
            db_path,
        })
    }
}

impl StoresVectors for SqliteVecStore {
    fn upsert(&self, records: &[VectorRecord<'_>]) -> Result<(), SearchError> {
        if records.is_empty() {
            return Ok(());
        }
        for record in records {
            if record.vector.len() != self.dimension {
                return Err(SearchError::VectorDimensionMismatch {
                    path: record.path.to_owned(),
                    ordinal: record.ordinal,
                    expected: self.dimension,
                    actual: record.vector.len(),
                });
            }
        }

        let mut connection = self.locked();
        let transaction = connection
            .transaction()
            .map_err(|source| storage_error(&self.db_path, source))?;

        for record in records {
            let ordinal = i64::try_from(record.ordinal).map_err(|_| SearchError::VectorOrdinalOutOfRange {
                path: record.path.to_owned(),
                ordinal: record.ordinal,
            })?;
            let heading_json =
                serde_json::to_string(record.heading_context).map_err(|source| SearchError::VectorRecordCorrupt {
                    path: self.db_path.clone(),
                    reason: format!("heading context could not be encoded: {source}"),
                })?;
            let hash_text: &str = record.content_hash.into();

            let existing_rowid: Option<i64> = transaction
                .query_row(
                    "SELECT rowid FROM chunks WHERE path = ?1 AND ordinal = ?2",
                    params![record.path, ordinal],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|source| storage_error(&self.db_path, source))?;

            let rowid = if let Some(rowid) = existing_rowid {
                transaction
                    .execute("DELETE FROM vec_chunks WHERE rowid = ?1", params![rowid])
                    .map_err(|source| storage_error(&self.db_path, source))?;
                transaction
                    .execute(
                        "UPDATE chunks SET heading_context = ?1, content_hash = ?2 WHERE rowid = ?3",
                        params![heading_json, hash_text, rowid],
                    )
                    .map_err(|source| storage_error(&self.db_path, source))?;
                rowid
            } else {
                transaction
                    .execute(
                        "INSERT INTO chunks(path, ordinal, heading_context, content_hash) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![record.path, ordinal, heading_json, hash_text],
                    )
                    .map_err(|source| storage_error(&self.db_path, source))?;
                transaction.last_insert_rowid()
            };

            let blob = serialise_vector(record.vector);
            transaction
                .execute(
                    "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                    params![rowid, blob],
                )
                .map_err(|source| storage_error(&self.db_path, source))?;
        }

        transaction
            .commit()
            .map_err(|source| storage_error(&self.db_path, source))
    }

    fn delete(&self, path: &str) -> Result<(), SearchError> {
        let mut connection = self.locked();
        let transaction = connection
            .transaction()
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .execute(
                "DELETE FROM vec_chunks WHERE rowid IN (SELECT rowid FROM chunks WHERE path = ?1)",
                params![path],
            )
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .execute("DELETE FROM chunks WHERE path = ?1", params![path])
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .commit()
            .map_err(|source| storage_error(&self.db_path, source))
    }

    fn similar(&self, request: &SimilarityQuery<'_>) -> Result<Vec<SimilarityHit>, SearchError> {
        if request.vector.len() != self.dimension {
            return Err(SearchError::VectorDimensionMismatch {
                path: String::new(),
                ordinal: 0,
                expected: self.dimension,
                actual: request.vector.len(),
            });
        }
        if request.k == 0 {
            return Ok(Vec::new());
        }

        let connection = self.locked();
        let query_vector = serialise_vector(request.vector);
        let k = i64::try_from(request.k).unwrap_or(i64::MAX);

        let mut sql = String::from(
            "SELECT c.path, c.ordinal, c.heading_context, c.content_hash, v.distance \
             FROM vec_chunks v JOIN chunks c ON c.rowid = v.rowid \
             WHERE v.embedding MATCH ?1 AND k = ?2",
        );
        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(query_vector), Box::new(k)];
        if let Some(prefix) = request.path_prefix {
            push_segment_scope_clause(&mut sql, &mut bound, prefix, "IN");
        }
        for excluded in request.exclude_paths {
            push_segment_scope_clause(&mut sql, &mut bound, excluded, "NOT IN");
        }
        sql.push_str(" ORDER BY v.distance");

        let mut statement = connection
            .prepare(&sql)
            .map_err(|source| storage_error(&self.db_path, source))?;
        let bound_refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(std::convert::AsRef::as_ref).collect();
        let mut rows = collect_hits(&mut statement, bound_refs.as_slice(), &self.db_path)?;
        rows.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(rows)
    }

    fn existing_hash(&self, path: &str, ordinal: usize) -> Result<Option<ContentHash>, SearchError> {
        let ordinal = i64::try_from(ordinal).map_err(|_| SearchError::VectorOrdinalOutOfRange {
            path: path.to_owned(),
            ordinal,
        })?;
        let connection = self.locked();
        let stored: Option<String> = connection
            .query_row(
                "SELECT content_hash FROM chunks WHERE path = ?1 AND ordinal = ?2",
                params![path, ordinal],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| storage_error(&self.db_path, source))?;
        stored
            .map(|text| {
                ContentHash::try_from(text.as_str()).map_err(|source| SearchError::VectorRecordCorrupt {
                    path: path.to_owned(),
                    reason: format!("stored content hash is invalid: {source}"),
                })
            })
            .transpose()
    }

    fn prune_ordinals_at_or_beyond(&self, path: &str, keep_below: usize) -> Result<(), SearchError> {
        let keep_below = i64::try_from(keep_below).unwrap_or(i64::MAX);
        let mut connection = self.locked();
        let transaction = connection
            .transaction()
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .execute(
                "DELETE FROM vec_chunks WHERE rowid IN (\
                     SELECT rowid FROM chunks WHERE path = ?1 AND ordinal >= ?2\
                 )",
                params![path, keep_below],
            )
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .execute(
                "DELETE FROM chunks WHERE path = ?1 AND ordinal >= ?2",
                params![path, keep_below],
            )
            .map_err(|source| storage_error(&self.db_path, source))?;
        transaction
            .commit()
            .map_err(|source| storage_error(&self.db_path, source))
    }

    fn stats(&self) -> Result<VectorStoreStats, SearchError> {
        let connection = self.locked();
        let (documents, chunks): (i64, i64) = connection
            .query_row("SELECT COUNT(DISTINCT path), COUNT(*) FROM chunks", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|source| storage_error(&self.db_path, source))?;
        Ok(VectorStoreStats {
            documents: usize::try_from(documents).unwrap_or(0),
            chunks: usize::try_from(chunks).unwrap_or(0),
        })
    }
}

impl SqliteVecStore {
    fn locked(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Reads every row from an already-prepared similarity statement into
/// [`SimilarityHit`]s. `db_path` is used only to label a storage error if
/// one occurs while binding or reading.
fn collect_hits(
    statement: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
    db_path: &str,
) -> Result<Vec<SimilarityHit>, SearchError> {
    let rows = statement
        .query_map(params, |row| {
            let path: String = row.get(0)?;
            let ordinal: i64 = row.get(1)?;
            let heading_json: String = row.get(2)?;
            let hash_text: String = row.get(3)?;
            let distance: f32 = row.get(4)?;
            Ok((path, ordinal, heading_json, hash_text, distance))
        })
        .map_err(|source| storage_error(db_path, source))?;

    let mut hits = Vec::new();
    for row in rows {
        let (path, ordinal, heading_json, hash_text, distance) =
            row.map_err(|source| storage_error(db_path, source))?;
        let ordinal = usize::try_from(ordinal).map_err(|_| SearchError::VectorRecordCorrupt {
            path: path.clone(),
            reason: format!("stored ordinal {ordinal} is negative"),
        })?;
        let heading_context: Vec<String> =
            serde_json::from_str(&heading_json).map_err(|source| SearchError::VectorRecordCorrupt {
                path: path.clone(),
                reason: format!("stored heading context is invalid: {source}"),
            })?;
        let content_hash =
            ContentHash::try_from(hash_text.as_str()).map_err(|source| SearchError::VectorRecordCorrupt {
                path: path.clone(),
                reason: format!("stored content hash is invalid: {source}"),
            })?;
        hits.push(SimilarityHit {
            path,
            ordinal,
            heading_context,
            content_hash,
            score: 1.0 - distance,
        });
    }
    Ok(hits)
}

/// Serialises a vector into `sqlite-vec`'s little-endian `float[N]` blob
/// format: four bytes per component, in order.
fn serialise_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len().saturating_mul(4));
    for component in vector {
        bytes.extend_from_slice(&component.to_le_bytes());
    }
    bytes
}

/// Escapes `%`, `_`, and the escape character itself in `text` so it can be
/// bound as a `LIKE ... ESCAPE '\'` parameter without its content being
/// misinterpreted as a wildcard. The escaped result is still bound as a
/// query parameter value, never concatenated into SQL text, so this guards
/// wildcard semantics only; parameter binding already rules out injection.
fn escape_like_special_characters(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Appends a whole-path-segment scope clause (`AND v.rowid {verb} (...)`,
/// `verb` being `"IN"` for an include scope or `"NOT IN"` for an exclude
/// scope) to `sql`, pushing the two bound parameters it references
/// (exact-match path, then the escaped `LIKE` suffix pattern) onto `bound`.
/// Used for both `path_prefix` and each `exclude_paths` entry so the two
/// filters share one matching rule and one query.
fn push_segment_scope_clause(sql: &mut String, bound: &mut Vec<Box<dyn rusqlite::ToSql>>, path: &str, verb: &str) {
    let escaped = escape_like_special_characters(path);
    let suffix_pattern = format!("{escaped}/%");
    let exact_placeholder = bound.len() + 1;
    let suffix_placeholder = bound.len() + 2;
    let _ = write!(
        sql,
        " AND v.rowid {verb} (SELECT rowid FROM chunks WHERE path = ?{exact_placeholder} \
         OR path LIKE ?{suffix_placeholder} ESCAPE '\\')"
    );
    bound.push(Box::new(path.to_owned()));
    bound.push(Box::new(suffix_pattern));
}

fn storage_error(path: &str, source: rusqlite::Error) -> SearchError {
    SearchError::VectorStorage {
        path: path.to_owned(),
        source,
    }
}

/// Registers the statically linked `sqlite-vec` extension with `SQLite`'s
/// process-global auto-extension list, so every `rusqlite::Connection`
/// opened afterwards in this process exposes the `vec0` virtual table
/// module and the `vec_version()` scalar function, without `SQLite`'s
/// network- and plugin-loading-capable `load_extension` mechanism.
///
/// # Safety
///
/// This function contains the one `unsafe` block approved by
/// `phase-5-decision-addendum.md` A3. `sqlite3_auto_extension` is a C FFI
/// entry point (`rusqlite::ffi::sqlite3_auto_extension`) that stores a raw
/// function pointer in `SQLite`'s process-global extension list; `SQLite`
/// invokes it once per newly opened connection. Two invariants make the
/// call sound:
///
/// 1. `sqlite_vec::sqlite3_vec_init` is the real, exported C symbol for
///    `sqlite-vec`'s extension entry point, linked in by the `sqlite-vec`
///    crate's build script. Upstream declares it in Rust as a no-argument,
///    no-return `extern "C" fn()` purely so the crate links without binding
///    every `SQLite` C type; its true, ABI-correct signature is `SQLite`'s own
///    `xEntryPoint` shape,
///    `unsafe extern "C" fn(*mut sqlite3, *mut *mut c_char, *const
///    sqlite3_api_routines) -> c_int`, which is exactly what
///    `sqlite3_auto_extension` requires and what the `transmute` below
///    targets. This double declaration (a convenience no-op signature for
///    linking, the real signature for the actual call) is upstream
///    `sqlite-vec`'s own documented registration pattern, reproduced here
///    with the same `transmute`, in that crate's own test suite.
/// 2. The `std::mem::transmute` only reinterprets a function pointer as
///    another function-pointer type; it performs no layout, lifetime, or
///    ownership transmutation of any Rust value. Both the source and
///    target types are themselves plain, thin function pointers, so the
///    reinterpretation is a bit-for-bit no-op; the risk this `unsafe`
///    block carries is entirely in asserting the *signature* is correct,
///    not in the mechanics of the transmute itself.
///
/// The `Once` guard means the FFI call itself happens at most once per
/// process, regardless of how many [`SqliteVecStore`]s are constructed;
/// this is a stronger guarantee than relying solely on `SQLite`'s own
/// documented (and, in `tests/vector_store.rs`, empirically confirmed)
/// tolerance of repeated registration.
///
/// This is the only `unsafe` block in this crate. Every other line of
/// `SqliteVecStore` (every statement it prepares, binds, and reads) is
/// safe Rust using `rusqlite`'s parameterised query API.
#[allow(unsafe_code)]
fn register_sqlite_vec() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // SAFETY: see the function-level safety comment above.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(sqlite_vec::sqlite3_vec_init as *const ())));
        }
    });
}
