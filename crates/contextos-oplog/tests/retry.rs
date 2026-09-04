use std::sync::{Arc, Mutex};

use contextos_core::{
    AppendMutation, AppendOutcome, AppendsVault, LogsOperations, OpKind, OperationEvent, Origin, PipelineResult,
    VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet,
};
use contextos_oplog::{OperationLog, OperationLogConfig};
use tempfile::tempdir;
use thiserror::Error;
use time::macros::datetime;

#[derive(Clone, Debug)]
struct FailsOnceAppender {
    state: Arc<Mutex<AppenderState>>,
}

#[derive(Debug)]
struct AppenderState {
    failures_remaining: usize,
    records: Vec<String>,
}

#[derive(Debug, Error)]
#[error("injected append failure")]
struct InjectedFailure;

impl AppendsVault for FailsOnceAppender {
    type Error = InjectedFailure;

    fn append(&self, request: &AppendMutation) -> Result<PipelineResult<AppendOutcome>, Self::Error> {
        let mut state = self.state.lock().map_err(|_| InjectedFailure)?;
        if state.failures_remaining > 0 {
            state.failures_remaining -= 1;
            return Err(InjectedFailure);
        }
        state.records.push(request.content.clone());
        Ok(PipelineResult {
            value: AppendOutcome {
                path: request.path.clone(),
                bytes_appended: request.content.len(),
                created: false,
            },
            event: Some(OperationEvent {
                kind: OpKind::Modify,
                paths: vec![request.path.clone()],
                origin: request.origin.clone(),
                summary: "Appended operation log".to_owned(),
                at: datetime!(2026-07-18 18:30:00 +10:00),
            }),
            warnings: Vec::new(),
        })
    }
}

fn event(path: VaultPath, minute: u8, summary: &str) -> OperationEvent {
    OperationEvent {
        kind: OpKind::Modify,
        paths: vec![path],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: summary.to_owned(),
        at: datetime!(2026-07-18 18:00:00 +10:00)
            .replace_minute(minute)
            .unwrap_or(datetime!(2026-07-18 18:00:00 +10:00)),
    }
}

#[test]
fn failed_append_is_buffered_and_retried_before_the_next_entry() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let note = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;
    let state = Arc::new(Mutex::new(AppenderState {
        failures_remaining: 1,
        records: Vec::new(),
    }));
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots,
        relative_directory: "memory/log".to_owned(),
        appender: FailsOnceAppender {
            state: Arc::clone(&state),
        },
    })?;

    let first = log.append(&event(note.clone(), 1, "First entry"));
    let second = log
        .append(&event(note, 2, "Second entry"))
        .map_err(|warning| warning.message)?;
    let records = state.lock().map_err(|_| InjectedFailure)?.records.clone();

    assert!(first.is_err());
    assert_eq!(second.len(), 2);
    assert_eq!(records.len(), 2);
    assert!(records[0].contains("18:01:00"));
    assert!(records[0].contains("First entry"));
    assert!(records[1].contains("18:02:00"));
    assert!(records[1].contains("Second entry"));
    Ok(())
}

#[test]
fn graceful_flush_retries_a_buffered_entry_without_creating_another_record() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "note")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    let note = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: "note.md",
    })?;
    let state = Arc::new(Mutex::new(AppenderState {
        failures_remaining: 1,
        records: Vec::new(),
    }));
    let log = OperationLog::try_from(OperationLogConfig {
        root,
        roots,
        relative_directory: "memory/log".to_owned(),
        appender: FailsOnceAppender {
            state: Arc::clone(&state),
        },
    })?;
    assert!(log.append(&event(note, 1, "Buffered entry")).is_err());

    let events = log.flush()?;
    let records = state.lock().map_err(|_| InjectedFailure)?.records.clone();

    assert_eq!(events.len(), 1);
    assert_eq!(records.len(), 1);
    assert!(records[0].contains("Buffered entry"));
    assert!(log.flush()?.is_empty());
    Ok(())
}
