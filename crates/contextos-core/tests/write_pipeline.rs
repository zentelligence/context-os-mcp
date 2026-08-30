use std::convert::Infallible;
use std::path::{Path, PathBuf};

use contextos_core::{
    AppendMutation, AppendOutcome, AppliesMutations, Clock, ContentHash, CreateDirectoryMutation,
    CreateDirectoryOutcome, DeleteMutation, DeleteOutcome, MoveMutation, MoveOutcome, OpKind,
    OperationEvent, OperationWarning, Origin, PipelineConfig, RoutedPipelineConfig,
    RoutedWritePipeline, RoutesOperations, VaultPath, VaultPathInput, VaultRoot, VaultRootInput,
    VaultSet, WriteMutation, WriteOutcome, WritePipeline,
};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

#[derive(Debug, Default)]
struct RecordingAdapter {
    directory_created: bool,
}

impl AppliesMutations for RecordingAdapter {
    type Error = Infallible;

    fn write(&self, request: &WriteMutation) -> Result<WriteOutcome, Self::Error> {
        Ok(WriteOutcome {
            path: request.path.clone(),
            bytes_written: request.content.len(),
            content_hash: ContentHash::from([0_u8; 32]),
            created: true,
        })
    }

    fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<CreateDirectoryOutcome, Self::Error> {
        Ok(CreateDirectoryOutcome {
            path: request.path.clone(),
            created: self.directory_created,
        })
    }

    fn move_path(&self, request: &MoveMutation) -> Result<MoveOutcome, Self::Error> {
        Ok(MoveOutcome {
            source: request.source.clone(),
            destination: request.destination.clone(),
        })
    }

    fn delete(&self, request: &DeleteMutation) -> Result<DeleteOutcome, Self::Error> {
        Ok(DeleteOutcome {
            path: request.path.clone(),
            deleted: true,
            trashed: false,
        })
    }

    fn append(&self, request: &AppendMutation) -> Result<AppendOutcome, Self::Error> {
        Ok(AppendOutcome {
            path: request.path.clone(),
            bytes_appended: request.content.len(),
            created: true,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-07-18 10:11:12 +10:00)
    }
}

#[derive(Clone, Copy, Debug)]
struct FailingServices;

impl RoutesOperations for FailingServices {
    fn route(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        vec![
            OperationWarning {
                code: "index/stale".to_owned(),
                message: "index update queued for healing".to_owned(),
            },
            OperationWarning {
                code: "git/stage".to_owned(),
                message: "Git staging is degraded".to_owned(),
            },
        ]
    }
}

fn path(root: PathBuf, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    // `tempdir()` names its directory something like `.tmpjFXxK1` on
    // Windows, whose leading `.` is not a valid URI scheme token (`FR-96`),
    // so this fixture gives its vault an explicit, valid name rather than
    // relying on the temp directory's own basename.
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: root,
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    Ok(VaultPath::try_from(VaultPathInput { roots: &roots, raw })?)
}

#[test]
fn nfr_03_pipeline_persists_then_emits_one_operation_event()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let note = path(vault.path().to_path_buf(), "note.md")?;
    let pipeline = WritePipeline::from(PipelineConfig {
        adapter: RecordingAdapter {
            directory_created: true,
        },
        clock: FixedClock,
    });

    let result = pipeline.write(&WriteMutation {
        path: note.clone(),
        content: "hello".to_owned(),
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    })?;

    assert!(result.value.created);
    let event = result.event.as_ref().ok_or("write must emit an event")?;
    assert_eq!(event.kind, OpKind::Create);
    assert_eq!(event.paths, vec![note]);
    assert_eq!(event.origin, Origin::Tool("fs_write_file".to_owned()));
    assert_eq!(event.at, datetime!(2026-07-18 10:11:12 +10:00));
    assert!(result.warnings.is_empty());
    Ok(())
}

#[test]
fn phase_2_secondary_failures_warn_without_failing_a_completed_write()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let note = path(vault.path().to_path_buf(), "note.md")?;
    let pipeline = RoutedWritePipeline::from(RoutedPipelineConfig {
        adapter: RecordingAdapter::default(),
        clock: FixedClock,
        services: FailingServices,
    });

    let result = pipeline.write(&WriteMutation {
        path: note,
        content: "persisted".to_owned(),
        expected_hash: None,
        force: false,
        origin: Origin::Tool("fs_write_file".to_owned()),
    })?;

    assert!(result.value.created);
    assert_eq!(result.warnings.len(), 2);
    assert_eq!(result.warnings[0].code, "index/stale");
    assert_eq!(result.warnings[1].code, "git/stage");
    Ok(())
}

#[test]
fn fr_05_idempotent_directory_creation_still_has_a_typed_outcome()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let directory = path(vault.path().to_path_buf(), "notes")?;
    let pipeline = WritePipeline::from(PipelineConfig {
        adapter: RecordingAdapter {
            directory_created: true,
        },
        clock: FixedClock,
    });

    let result = pipeline.create_directory(&CreateDirectoryMutation {
        path: directory,
        origin: Origin::Tool("fs_create_directory".to_owned()),
    })?;

    assert!(result.value.created);
    assert_eq!(
        result.event.as_ref().map(|event| event.kind),
        Some(OpKind::Create)
    );
    Ok(())
}

#[test]
fn fr_05_idempotent_no_change_does_not_emit_an_operation_event()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let directory = path(vault.path().to_path_buf(), "notes")?;
    let pipeline = WritePipeline::from(PipelineConfig {
        adapter: RecordingAdapter::default(),
        clock: FixedClock,
    });

    let result = pipeline.create_directory(&CreateDirectoryMutation {
        path: directory,
        origin: Origin::Tool("fs_create_directory".to_owned()),
    })?;

    assert!(!result.value.created);
    assert!(result.event.is_none());
    Ok(())
}

#[test]
fn content_hash_try_from_rejects_non_sha256_text() {
    let result = ContentHash::try_from("not-a-sha256");

    assert!(result.is_err());
}

#[test]
fn vault_path_converts_to_path_reference_without_a_free_form_helper()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let path = path(vault.path().to_path_buf(), "note.md")?;
    let standard_path: &Path = (&path).into();

    // `note.md` does not exist yet, so the production path resolves
    // the vault (the existing ancestor) rather than the raw, possibly
    // unresolved tempdir path (e.g. under Windows 8.3 short names).
    assert_eq!(
        standard_path,
        dunce::canonicalize(vault.path())?.join("note.md")
    );
    Ok(())
}
