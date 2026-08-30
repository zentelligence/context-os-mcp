//! Prototype-only harness for the embedvec evaluation notes (location
//! recorded in `CLAUDE.local.md`).
//!
//! Not part of the crate's production surface: `EmbedVecStore` here is a
//! throwaway `StoresVectors` implementation used only to gather real,
//! measured numbers for the evaluation document, per that document's own
//! "if this is revisited later" guidance (build a throwaway store behind
//! the existing trait, in isolation, before touching `EmbeddingWorker` or
//! anything upstream of it). Nothing in `src/` references this file or the
//! `embedvec` dev-dependency it exercises.
//!
//! `unwrap`/`expect`/`panic` are allowed only in this file: the workspace's
//! `[lints]` table (`unwrap_used`, `expect_used`, `panic` all `deny`) exists
//! to keep production paths honest about failure, which does not apply to a
//! disposable measurement harness that is meant to abort loudly on the
//! first unexpected condition rather than hide it. `cast_precision_loss` is
//! allowed for the same reason it is harmless here: byte counts are
//! formatted as human-readable MiB for println! output only, never used
//! for a comparison or a stored value.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss
)]

const EF_SEARCH_MITIGATED: usize = 200;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use contextos_core::ContentHash;
use contextos_search::{
    SimilarityQuery, SqliteVecConfig, SqliteVecStore, StoresVectors, VectorRecord,
};
use embedvec::{Distance, EmbedVec, EmbedVecBuilder, FilterExpr};

/// Prototype `StoresVectors` implementation over `embedvec`, built entirely
/// on the crate's synchronous `_internal` methods (`add_internal`,
/// `delete_internal`, `search_internal`, `clear_sync`) rather than the
/// `async fn` wrapper API. Those methods are public but documented as
/// "public for Python bindings" — this is the first real test of whether
/// they are usable as a first-class synchronous Rust surface, which the
/// evaluation document flagged as unverified.
///
/// `embedvec` identifies vectors by a bare `usize` id it assigns; this
/// crate's `StoresVectors` port identifies chunks by `(path, ordinal)`. The
/// `index` field bridges the two. It also stands in for the lookup
/// `existing_hash`, `delete`, and `prune_ordinals_at_or_beyond` need:
/// `embedvec`'s `FilterExpr` is evaluated only against the bounded ANN
/// candidate window inside `search()` (confirmed by reading
/// `search_internal` in `embedvec`'s own source — see the evaluation
/// document), never as a standalone exact-match index, so there is no way
/// to ask "which ids have `path = X`" other than a linear `entries()` scan
/// or maintaining this side index ourselves.
struct EmbedVecStore {
    db: Mutex<EmbedVec>,
    index: Mutex<HashMap<(String, usize), usize>>,
}

impl EmbedVecStore {
    fn open(path: &Path, dimension: usize) -> Self {
        let db = EmbedVecBuilder::new(dimension)
            .metric(Distance::Cosine)
            .persistence(path.to_string_lossy().to_string())
            .build()
            .expect("embedvec store opens");
        Self {
            db: Mutex::new(db),
            index: Mutex::new(HashMap::new()),
        }
    }

    fn payload(record: &VectorRecord<'_>) -> serde_json::Value {
        let hash_text: &str = record.content_hash.into();
        serde_json::json!({
            "path": record.path,
            "ordinal": record.ordinal,
            "heading_context": record.heading_context,
            "content_hash": hash_text,
        })
    }
}

impl StoresVectors for EmbedVecStore {
    fn upsert(&self, records: &[VectorRecord<'_>]) -> Result<(), contextos_search::SearchError> {
        let mut db = self.db.lock().expect("db lock");
        let mut index = self.index.lock().expect("index lock");
        for record in records {
            let key = (record.path.to_owned(), record.ordinal);
            // No update-in-place (mirrors SqliteVecStore's own delete-then-
            // insert, forced there by `vec0`'s lack of `UPDATE` support);
            // here it is forced by `embedvec` exposing no update call at
            // all, only `add`/`delete`.
            if let Some(&old_id) = index.get(&key) {
                db.delete_internal(old_id).expect("delete stale record");
            }
            let id = db
                .add_internal(record.vector, Self::payload(record))
                .expect("add record");
            index.insert(key, id);
        }
        // Matches SqliteVecStore's durability: every upsert there is a
        // committed SQLite transaction, durable before the call returns.
        // Without this, embedvec would never actually persist
        // `high_water_mark`, silently not paying the cost the flush_sync
        // fix exists to make payable at all in a sync-only build.
        //
        // `flush_sync` exists only on Peter's local, unreleased embedvec
        // checkout (commit a5af175/d9eb4c0), not the published 0.10.0 this
        // dev-dependency normally pins — see the embedvec evaluation notes
        // (location recorded in CLAUDE.local.md). Gated behind a Cargo
        // feature so this file compiles against the published crate by
        // default.
        #[cfg(feature = "embedvec-unreleased-flush-sync")]
        db.flush_sync().expect("flush_sync");
        Ok(())
    }

    fn delete(&self, path: &str) -> Result<(), contextos_search::SearchError> {
        let mut db = self.db.lock().expect("db lock");
        let mut index = self.index.lock().expect("index lock");
        let matching: Vec<(String, usize)> =
            index.keys().filter(|(p, _)| p == path).cloned().collect();
        for key in matching {
            if let Some(id) = index.remove(&key) {
                db.delete_internal(id).expect("delete record");
            }
        }
        #[cfg(feature = "embedvec-unreleased-flush-sync")]
        db.flush_sync().expect("flush_sync");
        Ok(())
    }

    fn similar(
        &self,
        request: &SimilarityQuery<'_>,
    ) -> Result<Vec<contextos_search::SimilarityHit>, contextos_search::SearchError> {
        if request.k == 0 {
            return Ok(Vec::new());
        }
        let db = self.db.lock().expect("db lock");
        // ef_search widened well past k: embedvec truncates its ANN
        // candidate window to `ef_search.max(k)` *before* applying the
        // metadata filter (confirmed in `search_internal`'s source), so a
        // narrow window under-counts a path_prefix-scoped query. Widening
        // is a mitigation, not a fix: it still cannot guarantee the true
        // top-k the way `sqlite-vec`'s pre-filtered exact search does.
        let ef_search = (request.k * 20).max(EF_SEARCH_MITIGATED);
        let filter = request.path_prefix.map(|prefix| {
            FilterExpr::eq("path", prefix).or(FilterExpr::starts_with("path", format!("{prefix}/")))
        });
        let hits = db
            .search_internal(request.vector, request.k, ef_search, filter)
            .expect("search");
        Ok(hits
            .into_iter()
            .map(|hit| {
                let path = hit.payload["path"].as_str().unwrap_or_default().to_owned();
                let ordinal =
                    usize::try_from(hit.payload["ordinal"].as_u64().unwrap_or(0)).unwrap_or(0);
                let heading_context: Vec<String> = hit
                    .payload
                    .get("heading_context")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let hash_text = hit.payload["content_hash"].as_str().unwrap_or_default();
                let content_hash = ContentHash::try_from(hash_text).expect("valid stored hash");
                contextos_search::SimilarityHit {
                    path,
                    ordinal,
                    heading_context,
                    content_hash,
                    score: 1.0 - hit.score,
                }
            })
            .collect())
    }

    fn existing_hash(
        &self,
        path: &str,
        ordinal: usize,
    ) -> Result<Option<ContentHash>, contextos_search::SearchError> {
        let db = self.db.lock().expect("db lock");
        let index = self.index.lock().expect("index lock");
        let Some(&id) = index.get(&(path.to_owned(), ordinal)) else {
            return Ok(None);
        };
        let Some(payload) = db.payload(id) else {
            return Ok(None);
        };
        let hash_text = payload["content_hash"].as_str().unwrap_or_default();
        Ok(Some(
            ContentHash::try_from(hash_text).expect("valid stored hash"),
        ))
    }

    fn prune_ordinals_at_or_beyond(
        &self,
        path: &str,
        keep_below: usize,
    ) -> Result<(), contextos_search::SearchError> {
        let mut db = self.db.lock().expect("db lock");
        let mut index = self.index.lock().expect("index lock");
        let matching: Vec<(String, usize)> = index
            .keys()
            .filter(|(p, ordinal)| p == path && *ordinal >= keep_below)
            .cloned()
            .collect();
        for key in matching {
            if let Some(id) = index.remove(&key) {
                db.delete_internal(id).expect("delete record");
            }
        }
        #[cfg(feature = "embedvec-unreleased-flush-sync")]
        db.flush_sync().expect("flush_sync");
        Ok(())
    }

    fn stats(&self) -> Result<contextos_search::VectorStoreStats, contextos_search::SearchError> {
        let index = self.index.lock().expect("index lock");
        let documents = index
            .keys()
            .map(|(p, _)| p.clone())
            .collect::<std::collections::HashSet<_>>()
            .len();
        Ok(contextos_search::VectorStoreStats {
            documents,
            chunks: index.len(),
        })
    }
}

/// Deterministic xorshift64 PRNG so the benchmark needs no extra dependency
/// and is reproducible run to run (same shape as `embedvec`'s own
/// `test_h4_search_recall_multi` test in `src/lib.rs`).
struct Xorshift64(u64);

impl Xorshift64 {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn synthetic_vector(rng: &mut Xorshift64, dimension: usize) -> Vec<f32> {
    (0..dimension).map(|_| rng.next_f32()).collect()
}

fn synthetic_hash(seed: u64) -> ContentHash {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    ContentHash::from(bytes)
}

fn rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")
            .map(str::trim)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse().ok())
    })
}

fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                total += dir_size_bytes(&entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

/// One synthetic "vault": `file_count` files, `chunks_per_file` chunks each,
/// paths grouped under 20 top-level prefixes so `path_prefix` queries have
/// something realistic to scope against.
struct SyntheticVault {
    files: Vec<(String, Vec<VectorRecordOwned>)>,
}

struct VectorRecordOwned {
    ordinal: usize,
    heading_context: Vec<String>,
    content_hash: ContentHash,
    vector: Vec<f32>,
}

fn build_vault(file_count: usize, chunks_per_file: usize, dimension: usize) -> SyntheticVault {
    let mut rng = Xorshift64(0x9E37_79B9_7F4A_7C15);
    let mut files = Vec::with_capacity(file_count);
    for file_index in 0..file_count {
        let area = file_index % 20;
        let path = format!("area{area}/note{file_index}.md");
        let mut chunks = Vec::with_capacity(chunks_per_file);
        for ordinal in 0..chunks_per_file {
            chunks.push(VectorRecordOwned {
                ordinal,
                heading_context: vec![format!("Section {ordinal}")],
                content_hash: synthetic_hash((file_index * 1000 + ordinal) as u64),
                vector: synthetic_vector(&mut rng, dimension),
            });
        }
        files.push((path, chunks));
    }
    SyntheticVault { files }
}

fn load_into(store: &dyn StoresVectors, vault: &SyntheticVault) -> std::time::Duration {
    let started = Instant::now();
    for (path, chunks) in &vault.files {
        let records: Vec<VectorRecord<'_>> = chunks
            .iter()
            .map(|chunk| VectorRecord {
                path,
                ordinal: chunk.ordinal,
                heading_context: &chunk.heading_context,
                content_hash: &chunk.content_hash,
                vector: &chunk.vector,
            })
            .collect();
        store.upsert(&records).expect("upsert succeeds");
    }
    started.elapsed()
}

fn bench_existing_hash(store: &dyn StoresVectors, vault: &SyntheticVault) -> std::time::Duration {
    let started = Instant::now();
    for (path, chunks) in &vault.files {
        for chunk in chunks {
            store
                .existing_hash(path, chunk.ordinal)
                .expect("existing_hash succeeds");
        }
    }
    started.elapsed()
}

fn bench_queries(
    store: &dyn StoresVectors,
    vault: &SyntheticVault,
    dimension: usize,
    k: usize,
    query_count: usize,
    path_prefix: Option<&str>,
) -> std::time::Duration {
    let mut rng = Xorshift64(0xD1B5_4A32_D192_ED03);
    let started = Instant::now();
    for _ in 0..query_count {
        let query = synthetic_vector(&mut rng, dimension);
        let request = SimilarityQuery {
            vector: &query,
            k,
            path_prefix,
            exclude_paths: &[],
        };
        store.similar(&request).expect("similar succeeds");
    }
    let _ = vault;
    started.elapsed()
}

fn verify_path_prefix_undercount(dimension: usize) {
    // Correctness check, not a throughput measurement: build a small store
    // where the true top-k within a path_prefix scope exists only outside
    // the ANN candidate window `search_internal` examines before applying
    // the metadata filter, and confirm `similar()` returns fewer than k
    // hits even though k matching records exist in the store. This is the
    // concrete failure mode the evaluation document's `sqlite-vec` module
    // docs warned an unverified store might have.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = EmbedVecStore::open(dir.path(), dimension);
    let mut rng = Xorshift64(0x1234_5678_9ABC_DEF0);

    // 500 "noise" records under a different path, inserted first so they
    // dominate the near neighbourhood of the query vector below.
    let query = synthetic_vector(&mut rng, dimension);
    for i in 0..500u64 {
        let hash = synthetic_hash(i);
        let heading = vec!["noise".to_owned()];
        let vector = synthetic_vector(&mut rng, dimension);
        let record = VectorRecord {
            path: "noise",
            ordinal: usize::try_from(i).expect("small index fits usize"),
            heading_context: &heading,
            content_hash: &hash,
            vector: &vector,
        };
        store.upsert(&[record]).expect("noise upsert");
    }

    // 10 "scoped" records under "scoped/", each an exact copy of the query
    // vector so they are the true nearest neighbours within that prefix,
    // but inserted after 500 unrelated vectors already occupy the ANN
    // graph's shallow layers.
    for i in 0..10u64 {
        let hash = synthetic_hash(1000 + i);
        let heading = vec!["scoped".to_owned()];
        let record = VectorRecord {
            path: "scoped/file.md",
            ordinal: usize::try_from(i).expect("small index fits usize"),
            heading_context: &heading,
            content_hash: &hash,
            vector: &query,
        };
        store.upsert(&[record]).expect("scoped upsert");
    }

    let request = SimilarityQuery {
        vector: &query,
        k: 10,
        path_prefix: Some("scoped"),
        exclude_paths: &[],
    };
    let hits = store.similar(&request).expect("scoped similar succeeds");
    println!(
        "mitigated (ef_search={EF_SEARCH_MITIGATED} via EmbedVecStore's own widening): \
         requested k=10 within \"scoped\", got {} hits",
        hits.len()
    );

    // Raw, unmitigated case: call search_internal directly with
    // ef_search == k (no widening), the parameter a naive port of the
    // `SimilarityQuery` shape (which has no ef_search concept at all) would
    // reach for. This isolates the structural behaviour from this
    // prototype's own compensating multiplier.
    let filter = Some(FilterExpr::eq("path", "scoped/file.md"));
    let db = store.db.lock().expect("db lock");
    let raw_hits = db
        .search_internal(&query, 10, 10, filter)
        .expect("raw search succeeds");
    drop(db);
    println!(
        "unmitigated (ef_search=k=10, no widening): requested k=10 within \"scoped\", got {} hits",
        raw_hits.len()
    );
    if raw_hits.len() < 10 {
        println!(
            "CONFIRMED: with ef_search == k, embedvec's post-hoc metadata filtering \
             under-counts a scoped query (found {}/10) even though 10 exact matches exist in \
             the store; sqlite-vec's pre-filtered exact search never under-counts this way. \
             Widening ef_search (this prototype's mitigation) recovered {}/10 in the same \
             scenario, at the cost of {}x more candidates scanned per query.",
            raw_hits.len(),
            hits.len(),
            EF_SEARCH_MITIGATED / 10
        );
    } else {
        println!(
            "Not reproduced even unmitigated at this corpus size/shape — the structural risk \
             (filter applied after a bounded candidate window, confirmed by reading \
             search_internal's source) remains real, but this synthetic corpus was not \
             adversarial enough to force it. A production-scale test against a real vault's \
             path distribution would need to check this directly before relying on \
             path_prefix-scoped queries."
        );
    }
}

fn main() {
    let dimension = 384; // common fastembed default (e.g. all-MiniLM-L6-v2 / bge-small)
    let file_count: usize = std::env::var("BENCH_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);
    let chunks_per_file = 8;
    let total_chunks = file_count * chunks_per_file;

    println!("=== embedvec vs sqlite-vec prototype benchmark ===");
    println!(
        "dimension={dimension} files={file_count} chunks_per_file={chunks_per_file} \
         total_chunks={total_chunks}"
    );

    verify_path_prefix_undercount(dimension);

    let vault = build_vault(file_count, chunks_per_file, dimension);

    // --- sqlite-vec ---
    let sqlite_dir = tempfile::tempdir().expect("tempdir");
    let sqlite_path = sqlite_dir.path().join("vectors.db");
    let rss_before = rss_kb();
    let sqlite_store = SqliteVecStore::try_from(SqliteVecConfig {
        path: sqlite_path.clone(),
        dimension,
    })
    .expect("sqlite-vec store opens");
    let sqlite_load_time = load_into(&sqlite_store, &vault);
    let rss_after_sqlite = rss_kb();
    let sqlite_disk_bytes = std::fs::metadata(&sqlite_path).map_or(0, |m| m.len());
    let sqlite_hash_time = bench_existing_hash(&sqlite_store, &vault);
    let sqlite_query_time_unscoped = bench_queries(&sqlite_store, &vault, dimension, 10, 200, None);
    let sqlite_query_time_scoped =
        bench_queries(&sqlite_store, &vault, dimension, 10, 200, Some("area0"));

    println!("\n--- sqlite-vec ---");
    println!("bulk load ({total_chunks} chunks, {file_count} upsert calls): {sqlite_load_time:?}");
    println!("existing_hash x{total_chunks}: {sqlite_hash_time:?}");
    println!("similar(k=10) x200 unscoped: {sqlite_query_time_unscoped:?}");
    println!("similar(k=10) x200 path_prefix-scoped: {sqlite_query_time_scoped:?}");
    println!(
        "on-disk size: {} bytes ({:.1} MiB)",
        sqlite_disk_bytes,
        sqlite_disk_bytes as f64 / (1024.0 * 1024.0)
    );
    if let (Some(before), Some(after)) = (rss_before, rss_after_sqlite) {
        println!("RSS delta: {} KiB", after.saturating_sub(before));
    }

    // --- embedvec ---
    let embedvec_dir = tempfile::tempdir().expect("tempdir");
    let rss_before = rss_kb();
    let embedvec_store = EmbedVecStore::open(embedvec_dir.path(), dimension);
    let embedvec_load_time = load_into(&embedvec_store, &vault);
    let rss_after_embedvec = rss_kb();
    let embedvec_disk_bytes = dir_size_bytes(embedvec_dir.path());
    let embedvec_hash_time = bench_existing_hash(&embedvec_store, &vault);
    let embedvec_query_time_unscoped =
        bench_queries(&embedvec_store, &vault, dimension, 10, 200, None);
    let embedvec_query_time_scoped =
        bench_queries(&embedvec_store, &vault, dimension, 10, 200, Some("area0"));

    println!("\n--- embedvec (sync _internal API, default-features=false, persistence-fjall) ---");
    println!(
        "bulk load ({total_chunks} chunks, {file_count} upsert calls): {embedvec_load_time:?}"
    );
    println!("existing_hash x{total_chunks} (side-index lookup): {embedvec_hash_time:?}");
    println!("similar(k=10) x200 unscoped: {embedvec_query_time_unscoped:?}");
    println!(
        "similar(k=10) x200 path_prefix-scoped (ef_search widened): {embedvec_query_time_scoped:?}"
    );
    println!(
        "on-disk size: {} bytes ({:.1} MiB)",
        embedvec_disk_bytes,
        embedvec_disk_bytes as f64 / (1024.0 * 1024.0)
    );
    if let (Some(before), Some(after)) = (rss_before, rss_after_embedvec) {
        println!("RSS delta: {} KiB", after.saturating_sub(before));
    }

    println!("\n=== summary ===");
    println!(
        "load: sqlite-vec {sqlite_load_time:?} vs embedvec {embedvec_load_time:?} \
         ({:.2}x)",
        embedvec_load_time.as_secs_f64() / sqlite_load_time.as_secs_f64().max(1e-9)
    );
    println!(
        "existing_hash: sqlite-vec {sqlite_hash_time:?} vs embedvec {embedvec_hash_time:?} \
         ({:.2}x)",
        embedvec_hash_time.as_secs_f64() / sqlite_hash_time.as_secs_f64().max(1e-9)
    );
    println!(
        "query (unscoped): sqlite-vec {sqlite_query_time_unscoped:?} vs embedvec \
         {embedvec_query_time_unscoped:?} ({:.2}x)",
        embedvec_query_time_unscoped.as_secs_f64()
            / sqlite_query_time_unscoped.as_secs_f64().max(1e-9)
    );
    println!("disk: sqlite-vec {sqlite_disk_bytes} bytes vs embedvec {embedvec_disk_bytes} bytes");
}
