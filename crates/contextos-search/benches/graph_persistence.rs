//! Performance evidence for the Phase 11 graph-backend benchmark notes
//! (location recorded in `CLAUDE.local.md`).
//!
//! Unlike `embedvec_prototype.rs` (a throwaway trait implementation used to
//! evaluate a dependency before committing to it), this benchmark exercises
//! the real, shipped `LinkGraph`/`StoresGraph` implementations in `src/`
//! directly: `GraphBackend::Serde`, `Fjall`, and `Sqlite`. Phase 11's own
//! gate (its change brief, location recorded in `CLAUDE.local.md`) covered
//! correctness
//! only (round trips, the concurrency-open proof); this file measures what
//! that gate left open: bulk-load, reopen, and incremental-write latency per
//! backend at a realistic vault scale, on-disk size, whether query latency
//! is genuinely backend-independent (it should be, by construction: every
//! `LinkGraph` query method reads only the in-memory `petgraph` structure,
//! never the store), and — the one thing no prior test measured — actual
//! throughput and correctness under sustained concurrent writes from two
//! independent `sqlite`-backed instances, plus an empirical demonstration of
//! the `serde` backend's documented lost-update hazard under the same
//! access pattern.
//!
//! `unwrap`/`expect`/`panic` are allowed only in this file, matching
//! `embedvec_prototype.rs`'s precedent: the workspace's `[lints]` table
//! exists to keep production paths honest about failure, which does not
//! apply to a disposable measurement harness meant to abort loudly on the
//! first unexpected condition. `cast_precision_loss` is allowed for the same
//! reason it is harmless here: byte/count values are formatted as
//! human-readable output only, never used for a comparison or stored value.
//! `cast_possible_truncation` is allowed for the same reason: every such
//! cast here is either a PRNG output reduced `% linked_range` immediately
//! after (any truncation still yields a valid, merely less-random index,
//! never an out-of-bounds one), or a small, benchmark-fixed sample count
//! (at most a few hundred) cast to `u32` for `Duration` division, far below
//! `u32::MAX` on any target this benchmark runs on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::path::Path;
use std::time::{Duration, Instant};

use contextos_obsidian::ObsidianLink;
use contextos_search::{GraphBackend, GraphDirection, LinkGraph, LinkGraphConfig};

/// Deterministic xorshift64 PRNG, reproducible run to run, matching
/// `embedvec_prototype.rs`'s own rationale for avoiding an extra dependency.
struct Xorshift64(u64);

impl Xorshift64 {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
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

fn link_to(target: &str) -> ObsidianLink {
    ObsidianLink {
        target: target.to_owned(),
        display: None,
        heading: None,
        block: None,
        embed: false,
    }
}

/// Builds `note_count` synthetic notes across 20 folders ("areas"), each
/// (outside the trailing `orphan_count` notes) linking to the next note in
/// sequence (guaranteeing a long resolvable chain for `path_between`) plus
/// `links_per_note - 1` further deterministic links, and one deliberately
/// unresolved (phantom) link every 50th note. The trailing `orphan_count`
/// notes carry no links and are never linked to, so `orphans()` has a known,
/// exact answer to check against.
fn build_vault(
    note_count: usize,
    links_per_note: usize,
    orphan_count: usize,
) -> Vec<(String, String, Vec<ObsidianLink>)> {
    let mut rng = Xorshift64(0x2545_F491_4F6C_DD1D);
    let linked_range = note_count.saturating_sub(orphan_count);
    let paths: Vec<String> = (0..note_count).map(|i| format!("area{}/note{i}.md", i % 20)).collect();

    let mut notes = Vec::with_capacity(note_count);
    for (i, path) in paths.iter().enumerate() {
        let title = format!("Note {i}");
        let mut links = Vec::new();
        if i < linked_range {
            if i + 1 < linked_range {
                links.push(link_to(&paths[i + 1]));
            }
            for _ in 1..links_per_note {
                let target = (rng.next_u64() as usize) % linked_range;
                if target != i {
                    links.push(link_to(&paths[target]));
                }
            }
            if i % 50 == 0 {
                links.push(link_to(&format!("missing/ghost{i}.md")));
            }
        }
        notes.push((path.clone(), title, links));
    }
    notes
}

fn open(store_directory: &Path, backend: GraphBackend) -> LinkGraph {
    LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.to_path_buf(),
        backend,
    })
    .expect("graph opens")
}

struct BackendResult {
    backend: GraphBackend,
    bulk_load: Duration,
    reopen: Duration,
    incremental_upsert_total: Duration,
    incremental_upsert_count: usize,
    neighbours_total: Duration,
    neighbours_count: usize,
    backlinks_total: Duration,
    backlinks_count: usize,
    orphans: Duration,
    path_between: Duration,
    disk_bytes: u64,
    rss_delta_kb: Option<u64>,
}

fn bench_backend(
    backend: GraphBackend,
    vault: &[(String, String, Vec<ObsidianLink>)],
    note_count: usize,
    orphan_count: usize,
    query_samples: usize,
    upsert_samples: usize,
) -> BackendResult {
    let dir = tempfile::tempdir().expect("tempdir");
    let store_directory = dir.path().join(".contextos").join("graph");

    let rss_before = rss_kb();
    let mut graph = open(&store_directory, backend);
    assert!(graph.needs_rebuild(), "fresh store starts flagged");

    let started = Instant::now();
    graph.rebuild(vault).expect("rebuild succeeds");
    let bulk_load = started.elapsed();
    assert!(!graph.needs_rebuild());
    drop(graph);
    let rss_after = rss_kb();

    let started = Instant::now();
    let mut reopened = open(&store_directory, backend);
    let reopen = started.elapsed();
    assert!(!reopened.needs_rebuild(), "reopened store trusts its data");

    // Incremental single-note edits: `upsert_note`'s documented contract is
    // that it writes only the touched note's records, not the whole graph.
    // This measures the actual cost of that claim per backend, on a graph
    // already at full vault scale, the case where `serde`'s whole-file
    // rewrite is expected to diverge sharply from `fjall`/`sqlite`.
    let mut rng = Xorshift64(0x9E37_79B9_7F4A_7C15);
    let linked_range = note_count.saturating_sub(orphan_count);
    let started = Instant::now();
    for _ in 0..upsert_samples {
        let idx = (rng.next_u64() as usize) % linked_range;
        let (path, title, links) = &vault[idx];
        reopened.upsert_note(path, title, links).expect("upsert succeeds");
    }
    let incremental_upsert_total = started.elapsed();

    let started = Instant::now();
    for _ in 0..query_samples {
        let idx = (rng.next_u64() as usize) % linked_range;
        let (path, ..) = &vault[idx];
        reopened
            .neighbours(path, 2, GraphDirection::Both)
            .expect("neighbours succeeds");
    }
    let neighbours_total = started.elapsed();

    let started = Instant::now();
    for _ in 0..query_samples {
        let idx = (rng.next_u64() as usize) % linked_range;
        let (path, ..) = &vault[idx];
        reopened.backlinks(path).expect("backlinks succeeds");
    }
    let backlinks_total = started.elapsed();

    let started = Instant::now();
    let orphan_view = reopened.orphans().expect("orphans succeeds");
    let orphans = started.elapsed();
    assert_eq!(
        orphan_view.nodes.len(),
        orphan_count,
        "orphan count matches the vault's known-exact answer, backend {backend:?}"
    );

    let started = Instant::now();
    reopened
        .path_between(&vault[0].0, &vault[10].0, GraphDirection::Out)
        .expect("path_between succeeds");
    let path_between = started.elapsed();

    let disk_bytes = dir_size_bytes(&store_directory);
    drop(reopened);

    BackendResult {
        backend,
        bulk_load,
        reopen,
        incremental_upsert_total,
        incremental_upsert_count: upsert_samples,
        neighbours_total,
        neighbours_count: query_samples,
        backlinks_total,
        backlinks_count: query_samples,
        orphans,
        path_between,
        disk_bytes,
        rss_delta_kb: rss_before.zip(rss_after).map(|(b, a)| a.saturating_sub(b)),
    }
}

fn print_result(result: &BackendResult) {
    println!("\n--- {:?} ---", result.backend);
    println!("bulk load (rebuild): {:?}", result.bulk_load);
    println!("reopen (cold start): {:?}", result.reopen);
    println!(
        "incremental upsert_note x{}: {:?} ({:?}/call avg)",
        result.incremental_upsert_count,
        result.incremental_upsert_total,
        result.incremental_upsert_total / result.incremental_upsert_count.max(1) as u32
    );
    println!(
        "neighbours(depth=2) x{}: {:?} ({:?}/call avg)",
        result.neighbours_count,
        result.neighbours_total,
        result.neighbours_total / result.neighbours_count.max(1) as u32
    );
    println!(
        "backlinks x{}: {:?} ({:?}/call avg)",
        result.backlinks_count,
        result.backlinks_total,
        result.backlinks_total / result.backlinks_count.max(1) as u32
    );
    println!("orphans (whole-graph scan): {:?}", result.orphans);
    println!("path_between (10-hop chain): {:?}", result.path_between);
    println!(
        "on-disk size: {} bytes ({:.2} MiB)",
        result.disk_bytes,
        result.disk_bytes as f64 / (1024.0 * 1024.0)
    );
    if let Some(delta) = result.rss_delta_kb {
        println!("RSS delta (bulk load): {delta} KiB");
    }
}

/// Sustained concurrent writes from two independent `sqlite`-backed
/// instances against the same store directory, the real-world multi-writer
/// scenario, rather than the correctness-only "both opens succeed" proof
/// `fr_108_two_sqlite_backed_instances_open_concurrently` already covers.
/// Measures actual throughput and confirms every write from
/// both threads survives, then compares against a single-writer baseline
/// performing the same total write count to quantify WAL/busy-timeout
/// contention overhead.
fn bench_sqlite_concurrency(writes_per_thread: usize) {
    println!("\n=== sqlite concurrent-write contention ===");

    let dir = tempfile::tempdir().expect("tempdir");
    let store_directory = dir.path().join(".contextos").join("graph");
    let a_dir = store_directory.clone();
    let b_dir = store_directory.clone();

    let started = Instant::now();
    let writer_a = std::thread::spawn(move || {
        let mut graph = open(&a_dir, GraphBackend::Sqlite);
        for i in 0..writes_per_thread {
            graph
                .upsert_note(&format!("concurrent/a{i}.md"), "A", &[])
                .expect("thread A upsert succeeds");
        }
    });
    let writer_b = std::thread::spawn(move || {
        let mut graph = open(&b_dir, GraphBackend::Sqlite);
        for i in 0..writes_per_thread {
            graph
                .upsert_note(&format!("concurrent/b{i}.md"), "B", &[])
                .expect("thread B upsert succeeds");
        }
    });
    writer_a.join().expect("thread A completes");
    writer_b.join().expect("thread B completes");
    let concurrent_elapsed = started.elapsed();

    let mut verify = open(&store_directory, GraphBackend::Sqlite);
    let total_writes = writes_per_thread * 2;
    assert_eq!(
        verify.full_view().expect("full_view succeeds").nodes.len(),
        total_writes,
        "every write from both concurrent threads survived with no lost updates"
    );
    drop(verify);
    println!(
        "two threads x {writes_per_thread} upsert_note calls each ({total_writes} total), \
         fully concurrent: {concurrent_elapsed:?}, all {total_writes} writes verified present, \
         zero errors"
    );

    // Single-writer baseline: the same total write count, one instance, no
    // contention, to isolate the concurrency overhead the two-writer run
    // above pays for WAL/busy-timeout coordination.
    let baseline_dir = tempfile::tempdir().expect("tempdir");
    let baseline_store = baseline_dir.path().join(".contextos").join("graph");
    let mut baseline_graph = open(&baseline_store, GraphBackend::Sqlite);
    let started = Instant::now();
    for i in 0..total_writes {
        baseline_graph
            .upsert_note(&format!("baseline/n{i}.md"), "N", &[])
            .expect("baseline upsert succeeds");
    }
    let baseline_elapsed = started.elapsed();
    println!("single-writer baseline, same {total_writes} total upsert_note calls: {baseline_elapsed:?}");
    println!(
        "concurrency overhead: {:.2}x versus single-writer baseline for the same total write \
         count (WAL mode plus a five-second busy_timeout absorbing lock contention as waiting, \
         not failure)",
        concurrent_elapsed.as_secs_f64() / baseline_elapsed.as_secs_f64().max(1e-9)
    );
}

/// Empirically demonstrates the `serde` backend's documented lost-update
/// hazard: it has no locking of its own, and `persist` writes only its own
/// in-memory mirror, never re-reading the file first. Two instances opened
/// around the same time, each writing a different note, is exactly the
/// access pattern `serde_store.rs`'s own module doc warns is unsafe; this
/// confirms the warning is not merely theoretical.
fn demonstrate_serde_concurrent_hazard() {
    println!("\n=== serde backend: concurrent-instance hazard (correctness, not throughput) ===");

    let dir = tempfile::tempdir().expect("tempdir");
    let store_directory = dir.path().join(".contextos").join("graph");

    // Both instances open the same, initially empty store before either has
    // written anything, exactly as two Cowork-style processes starting up
    // together would.
    let mut instance_a = open(&store_directory, GraphBackend::Serde);
    let mut instance_b = open(&store_directory, GraphBackend::Serde);

    instance_a
        .upsert_note("a.md", "A", &[])
        .expect("instance A upsert succeeds");
    // Instance B's in-memory mirror never learned about A's write: its own
    // next persist call rewrites the whole file from what it has, silently
    // discarding A's already-committed note.
    instance_b
        .upsert_note("b.md", "B", &[])
        .expect("instance B upsert succeeds");
    drop(instance_a);
    drop(instance_b);

    let mut verify = open(&store_directory, GraphBackend::Serde);
    let surviving: Vec<String> = verify
        .full_view()
        .expect("full_view succeeds")
        .nodes
        .into_iter()
        .map(|node| node.path)
        .collect();
    println!("surviving notes after both instances wrote: {surviving:?}");
    assert_eq!(
        surviving,
        vec!["b.md".to_owned()],
        "confirms the lost-update hazard: A's note is silently gone, not merged, not an error"
    );
    println!(
        "CONFIRMED: instance A's note was silently lost when instance B's later persist \
         rewrote the whole file from its own, stale-relative-to-A in-memory state. No error \
         was raised on either side. This is the concrete failure mode `serde_store.rs`'s module \
         documentation warns against; the sqlite backend exists specifically so this scenario \
         (the Cowork general-connector-plus-per-task-instance pattern) does not lose data."
    );
}

fn main() {
    let note_count: usize = std::env::var("GRAPH_BENCH_NOTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4000);
    let links_per_note = 6;
    let orphan_count = 50;
    let query_samples = 200;
    let upsert_samples = 100;

    println!("=== link-graph persistence backend benchmark ===");
    println!(
        "notes={note_count} links_per_note={links_per_note} orphan_count={orphan_count} \
         query_samples={query_samples} upsert_samples={upsert_samples}"
    );

    let vault = build_vault(note_count, links_per_note, orphan_count);
    let total_links: usize = vault.iter().map(|(_, _, links)| links.len()).sum();
    println!("total wikilinks generated: {total_links}");

    let results: Vec<BackendResult> = [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite]
        .into_iter()
        .map(|backend| bench_backend(backend, &vault, note_count, orphan_count, query_samples, upsert_samples))
        .collect();

    for result in &results {
        print_result(result);
    }

    println!("\n=== summary: bulk load / reopen / incremental upsert (avg) ===");
    for result in &results {
        println!(
            "{:?}: bulk load {:?}, reopen {:?}, upsert avg {:?}, disk {:.2} MiB",
            result.backend,
            result.bulk_load,
            result.reopen,
            result.incremental_upsert_total / result.incremental_upsert_count.max(1) as u32,
            result.disk_bytes as f64 / (1024.0 * 1024.0)
        );
    }

    println!(
        "\nquery latency (neighbours/backlinks) is expected to be near-identical across \
         backends: every LinkGraph query method reads only the in-memory petgraph structure \
         built once at open time, never the store (confirmed by reading service.rs's \
         graph_neighbours/graph_backlinks/graph_orphans/graph_path, each a Mutex<LinkGraph> lock \
         plus an in-memory call). The numbers above are reported to confirm this empirically, \
         not because backend choice was expected to matter here."
    );

    bench_sqlite_concurrency(200);
    demonstrate_serde_concurrent_hazard();
}
