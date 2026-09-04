//! Background semantic-index maintenance.
//!
//! The live write pipeline (`SearchCollection::update`,
//! `VaultSearchService::update`) only enqueues a changed path onto the
//! embedding worker's pending queue; nothing else ever drains it. Left
//! alone, a long-running server's semantic index stays frozen at whatever
//! it held when the process started, only ever advancing when an operator
//! explicitly calls `query_index_rebuild`, `vault_index_rebuild`, or the
//! `contextos index` CLI subcommand. This module closes that gap: one task
//! per vault with semantic search enabled repeatedly drains that vault's
//! queue on its own, so paths written through this server's own tools get
//! embedded with no operator action required.
//!
//! This deliberately calls [`VaultSearchService::drain_semantic_queue`],
//! never `rebuild`/`rebuild_with_budget`: those walk and re-enqueue the
//! *entire* vault whenever the queue is empty (by design, for an
//! occasional operator-triggered rebuild). Calling that on a timer turns
//! an occasional catch-up scan into a continuous full-vault re-walk and
//! re-hash every idle tick, forever — this was shipped once, saturated a
//! production vault's CPU, and is exactly the failure this comment exists
//! to prevent a repeat of. Content edited outside this server (for
//! example directly in Obsidian) is not discovered by this task at all;
//! that still needs an explicit `query_index_rebuild`/`vault_index_rebuild`
//! call or `contextos index`.

use std::sync::Arc;
use std::time::Duration;

use contextos_search::{SearchError, VaultSearchService};
use tokio_util::sync::CancellationToken;

/// How long a drain task waits before checking again once a vault's
/// semantic queue has been fully drained (`remaining == 0`). Chosen to
/// stay responsive to live edits without repeatedly polling a queue that
/// is usually empty between edits.
const IDLE_INTERVAL: Duration = Duration::from_secs(30);

/// Spawns one background task per `services` entry with semantic search
/// enabled, each repeatedly draining that vault's embedding queue up to
/// `budget_seconds[index]` per pass until `shutdown` is cancelled. Vaults
/// with semantic search disabled (`None`, or a service without a semantic
/// capability) are skipped entirely: there is nothing to drain.
///
/// `budget_seconds` is index-aligned with `services`, matching
/// `ContextOsServer::rebuild_budget_seconds`; a missing entry falls back
/// to the same default `query_index_rebuild` itself uses when a vault's
/// budget cannot be looked up.
pub(crate) fn spawn(
    services: &[Option<Arc<VaultSearchService>>],
    budget_seconds: &[u64],
    shutdown: &CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    const FALLBACK_REBUILD_BUDGET_SECONDS: u64 = 25;

    services
        .iter()
        .enumerate()
        .filter_map(|(index, service)| service.clone().map(|service| (index, service)))
        .filter(|(_, service)| service.semantic_enabled())
        .map(|(index, service)| {
            let budget = Duration::from_secs(
                budget_seconds
                    .get(index)
                    .copied()
                    .unwrap_or(FALLBACK_REBUILD_BUDGET_SECONDS),
            );
            tokio::spawn(drain_loop(service, budget, shutdown.clone()))
        })
        .collect()
}

async fn drain_loop(
    service: Arc<VaultSearchService>,
    budget: Duration,
    shutdown: CancellationToken,
) {
    let budget = time::Duration::seconds(i64::try_from(budget.as_secs()).unwrap_or(i64::MAX));
    loop {
        let pass_service = Arc::clone(&service);
        let outcome = tokio::select! {
            () = shutdown.cancelled() => return,
            result = tokio::task::spawn_blocking(move || {
                pass_service.drain_semantic_queue(Some(budget))
            }) => result,
        };
        let remaining = match outcome {
            Ok(Ok(report)) => report.remaining,
            Ok(Err(error)) => {
                warn_drain_failed(&error);
                0
            }
            Err(join_error) => {
                tracing::warn!(
                    error = %join_error,
                    "background semantic drain task panicked; will retry after the idle interval"
                );
                0
            }
        };
        let wait = if remaining > 0 {
            Duration::ZERO
        } else {
            IDLE_INTERVAL
        };
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

fn warn_drain_failed(error: &SearchError) {
    tracing::warn!(
        code = error.code(),
        error = %error,
        "background semantic drain pass failed; will retry after the idle interval"
    );
}

#[cfg(test)]
mod tests {
    use contextos_core::{
        OpKind, OperationEvent, Origin, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot,
        VaultRootId, VaultRootInput, VaultSet,
    };
    use contextos_search::{FakeEmbedder, GraphBackend, SemanticConfig, VaultSearchConfig};
    use time::OffsetDateTime;

    use super::*;

    fn vault_dir() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
        Ok(tempfile::Builder::new().prefix("vault").tempdir()?)
    }

    fn semantic_service_at(
        vault: &tempfile::TempDir,
    ) -> Result<Arc<VaultSearchService>, Box<dyn std::error::Error>> {
        Ok(Arc::new(VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: vault.path().to_path_buf(),
            excludes: vec![],
            state_directory: vault.path().join(".contextos"),
            text_enabled: false,
            graph_enabled: false,
            graph_backend: GraphBackend::default(),
            semantic: Some(SemanticConfig {
                embedder: Box::new(FakeEmbedder::default()),
                vector_store_path: vault.path().join(".contextos/vectors.db"),
            }),
        })?))
    }

    fn enqueue_a_note(
        vault: &tempfile::TempDir,
        service: &VaultSearchService,
        relative: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        std::fs::write(vault.path().join(relative), "# Note\n\nSome prose.\n")?;
        let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
            path: vault.path().to_path_buf(),
            managed: true,
            name: Some("vault".to_owned()),
        })?])?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: relative,
        })?;
        let Ok(()) = service.update(&OperationEvent {
            kind: OpKind::Create,
            paths: vec![path],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "test".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        }) else {
            return Err("expected the combined search update to succeed".into());
        };
        Ok(())
    }

    /// A vault with semantic search disabled is skipped entirely: no task
    /// is spawned for it, so an operator never sees a drain loop churning
    /// on a vault that has nothing to drain.
    #[tokio::test]
    async fn a_vault_with_semantic_disabled_gets_no_drain_task()
    -> Result<(), Box<dyn std::error::Error>> {
        let vault = vault_dir()?;
        let service = Arc::new(VaultSearchService::try_from(VaultSearchConfig {
            root_id: VaultRootId::try_from(0_usize)?,
            root: vault.path().to_path_buf(),
            excludes: vec![],
            state_directory: vault.path().join(".contextos"),
            text_enabled: true,
            graph_enabled: true,
            graph_backend: GraphBackend::default(),
            semantic: None,
        })?);
        let shutdown = CancellationToken::new();

        let tasks = spawn(&[Some(service)], &[25], &shutdown);

        assert!(tasks.is_empty());
        shutdown.cancel();
        Ok(())
    }

    /// The core contract this module exists for: a path enqueued through
    /// the live write pipeline (mirroring a real edit routed through the
    /// server's own filesystem tools), with nobody ever calling
    /// `query_index_rebuild`/`contextos index`, still ends up embedded
    /// once the background task gets a chance to run.
    #[tokio::test(start_paused = true)]
    async fn an_enqueued_path_is_embedded_without_any_explicit_rebuild_call()
    -> Result<(), Box<dyn std::error::Error>> {
        let vault = vault_dir()?;
        let service = semantic_service_at(&vault)?;
        enqueue_a_note(&vault, &service, "note.md")?;
        assert_eq!(service.status()?.semantic.documents, 0);

        let shutdown = CancellationToken::new();
        let tasks = spawn(&[Some(Arc::clone(&service))], &[25], &shutdown);
        assert_eq!(tasks.len(), 1);

        wait_until(&service, |status| status.semantic.documents == 1).await?;

        shutdown.cancel();
        for task in tasks {
            task.await?;
        }
        Ok(())
    }

    /// Regression test for a real incident (v0.13.2): the background task
    /// once called `rebuild`/`rebuild_with_budget`, which walks and
    /// re-enqueues the *entire* vault whenever the queue is empty. On a
    /// 30-second timer that turned into a continuous full-vault re-walk
    /// and re-hash that saturated a production machine's CPU. A file
    /// sitting in the vault but never routed through `update` (mirroring
    /// content edited directly in Obsidian, bypassing this server's own
    /// tools entirely) must never be discovered or embedded by this
    /// background task, no matter how many idle cycles pass.
    #[tokio::test(start_paused = true)]
    async fn a_file_present_in_the_vault_but_never_enqueued_is_never_embedded()
    -> Result<(), Box<dyn std::error::Error>> {
        let vault = vault_dir()?;
        let service = semantic_service_at(&vault)?;
        std::fs::write(
            vault.path().join("never-enqueued.md"),
            "# Untracked\n\nWritten directly to disk, not through `update`.\n",
        )?;

        let shutdown = CancellationToken::new();
        let tasks = spawn(&[Some(Arc::clone(&service))], &[25], &shutdown);

        // Advance well past several idle cycles: with paused time this
        // resolves instantly in real time, but proves the file stays
        // undiscovered across many ticks, not just the first one.
        tokio::time::sleep(IDLE_INTERVAL * 5).await;

        assert_eq!(service.status()?.semantic.documents, 0);
        shutdown.cancel();
        for task in tasks {
            task.await?;
        }
        Ok(())
    }

    /// Once a vault's queue is fully drained, the loop backs off to the
    /// idle interval rather than busy-polling; a later enqueue is still
    /// picked up on the next pass instead of being missed forever.
    #[tokio::test(start_paused = true)]
    async fn a_later_enqueue_after_the_queue_drains_is_still_picked_up_on_the_next_pass()
    -> Result<(), Box<dyn std::error::Error>> {
        let vault = vault_dir()?;
        let service = semantic_service_at(&vault)?;
        enqueue_a_note(&vault, &service, "first.md")?;

        let shutdown = CancellationToken::new();
        let tasks = spawn(&[Some(Arc::clone(&service))], &[25], &shutdown);

        wait_until(&service, |status| status.semantic.documents == 1).await?;

        enqueue_a_note(&vault, &service, "second.md")?;
        wait_until(&service, |status| status.semantic.documents == 2).await?;

        shutdown.cancel();
        for task in tasks {
            task.await?;
        }
        Ok(())
    }

    /// Cancelling `shutdown` stops every spawned task promptly, rather
    /// than leaving it looping forever: `main`'s own shutdown sequence
    /// awaits these handles, so a task that never returns would hang the
    /// whole server's graceful shutdown.
    #[tokio::test(start_paused = true)]
    async fn cancelling_shutdown_stops_the_task_promptly() -> Result<(), Box<dyn std::error::Error>>
    {
        let vault = vault_dir()?;
        let service = semantic_service_at(&vault)?;

        let shutdown = CancellationToken::new();
        let tasks = spawn(&[Some(service)], &[25], &shutdown);

        shutdown.cancel();
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(5), task).await??;
        }
        Ok(())
    }

    /// Polls `service.status()` until `done` is satisfied, bounded by
    /// `IDLE_INTERVAL` plus headroom rather than a fixed iteration count:
    /// with paused time, each `sleep` here advances the virtual clock
    /// instantly, so this resolves in real time as fast as the drain
    /// loop's own polling does, regardless of how long `IDLE_INTERVAL` is.
    async fn wait_until(
        service: &VaultSearchService,
        mut done: impl FnMut(&contextos_search::IndexStatusReport) -> bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = IDLE_INTERVAL + Duration::from_secs(30);
        match tokio::time::timeout(deadline, poll_status(service, &mut done)).await {
            Ok(result) => result,
            Err(_) => Err("condition was never met".into()),
        }
    }

    async fn poll_status(
        service: &VaultSearchService,
        done: &mut impl FnMut(&contextos_search::IndexStatusReport) -> bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let status = service.status()?;
            if done(&status) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
