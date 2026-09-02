use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use contextos_core::{
    AppendMutation, AppendOutcome, AppendsVault, AppliesMutations, Clock, ContentHash,
    CreateDirectoryMutation, CreateDirectoryOutcome, DeleteMode, DeleteMutation, DeleteOutcome,
    MoveMutation, MoveOutcome, MovesVault, NoSubstrateServices, OperationEvent, OperationWarning,
    PipelineResult, RestoreMutation, RoutedPipelineConfig, RoutedWritePipeline, RoutesOperations,
    VaultPath, VaultPathInput, WriteMutation, WriteOutcome, WritesVault,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use similar::TextDiff;
use tempfile::NamedTempFile;

use crate::discover::hash_file;
use crate::{Filesystem, FsError, ReadTextRequest};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditFileRequest {
    pub path: VaultPath,
    pub edits: Vec<TextEdit>,
    pub dry_run: bool,
    pub expected_hash: Option<ContentHash>,
    pub force: bool,
    pub origin: contextos_core::Origin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileResult {
    pub path: String,
    pub diff: String,
    pub applied: bool,
    pub content_hash: ContentHash,
    #[serde(skip)]
    pub event: Option<OperationEvent>,
    pub warnings: Vec<OperationWarning>,
}

#[derive(Clone, Debug)]
pub struct FilesystemServiceConfig<C> {
    pub filesystem: Filesystem,
    pub clock: C,
}

#[derive(Clone, Debug)]
pub struct RoutedFilesystemServiceConfig<C, R> {
    pub filesystem: Filesystem,
    pub clock: C,
    pub services: R,
}

#[derive(Clone, Debug)]
pub struct FilesystemService<C, R = NoSubstrateServices> {
    filesystem: Filesystem,
    pipeline: RoutedWritePipeline<Filesystem, C, R>,
}

impl<C> From<FilesystemServiceConfig<C>> for FilesystemService<C, NoSubstrateServices>
where
    C: Clone,
{
    fn from(value: FilesystemServiceConfig<C>) -> Self {
        let pipeline = RoutedWritePipeline::from(RoutedPipelineConfig {
            adapter: value.filesystem.clone(),
            clock: value.clock,
            services: NoSubstrateServices,
        });
        Self {
            filesystem: value.filesystem,
            pipeline,
        }
    }
}

impl<C, R> From<RoutedFilesystemServiceConfig<C, R>> for FilesystemService<C, R>
where
    C: Clone,
{
    fn from(value: RoutedFilesystemServiceConfig<C, R>) -> Self {
        let pipeline = RoutedWritePipeline::from(RoutedPipelineConfig {
            adapter: value.filesystem.clone(),
            clock: value.clock,
            services: value.services,
        });
        Self {
            filesystem: value.filesystem,
            pipeline,
        }
    }
}

impl<C, R> FilesystemService<C, R>
where
    C: Clock,
    R: RoutesOperations,
{
    /// Writes a file through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement, conflict, or persistence error.
    pub fn write_file(
        &self,
        request: &WriteMutation,
    ) -> Result<PipelineResult<WriteOutcome>, FsError> {
        self.pipeline.write(request)
    }

    /// Restores historical content through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement, conflict, or persistence error.
    pub fn restore_file(
        &self,
        request: &RestoreMutation,
    ) -> Result<PipelineResult<WriteOutcome>, FsError> {
        self.pipeline.restore(request)
    }

    /// Deletes one file or empty directory through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement or deletion error.
    pub fn delete_path(
        &self,
        request: &DeleteMutation,
    ) -> Result<PipelineResult<DeleteOutcome>, FsError> {
        self.pipeline.delete(request)
    }

    /// Appends one complete record through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement or append error.
    pub fn append_file(
        &self,
        request: &AppendMutation,
    ) -> Result<PipelineResult<AppendOutcome>, FsError> {
        self.pipeline.append(request)
    }

    /// Creates a directory tree through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement or persistence error.
    pub fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<PipelineResult<CreateDirectoryOutcome>, FsError> {
        self.pipeline.create_directory(request)
    }

    /// Moves a path through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement, destination, or persistence error.
    pub fn move_file(
        &self,
        request: &MoveMutation,
    ) -> Result<PipelineResult<MoveOutcome>, FsError> {
        self.pipeline.move_path(request)
    }

    /// Applies exact-match edits transactionally, or returns a dry-run diff.
    ///
    /// # Errors
    ///
    /// Returns a typed read, edit-match, conflict, or persistence error. No
    /// write occurs unless every edit has exactly one match.
    pub fn edit_file(&self, request: &EditFileRequest) -> Result<EditFileResult, FsError> {
        let current = self.filesystem.read_text(&ReadTextRequest {
            path: request.path.clone(),
            limit: None,
        })?;
        let mut modified = current.content.clone();
        for edit in &request.edits {
            apply_exact_edit(&mut modified, edit, &request.path)?;
        }
        let diff = TextDiff::from_lines(&current.content, &modified)
            .unified_diff()
            .header("original", "modified")
            .to_string();
        let mut hasher = Sha256::new();
        hasher.update(modified.as_bytes());
        let modified_hash = ContentHash::from(<[u8; 32]>::from(hasher.finalize()));

        if request.dry_run {
            return Ok(EditFileResult {
                path: request.path.relative().to_string_lossy().into_owned(),
                diff,
                applied: false,
                content_hash: modified_hash,
                event: None,
                warnings: Vec::new(),
            });
        }

        let write = self.pipeline.write(&WriteMutation {
            path: request.path.clone(),
            content: modified,
            expected_hash: request.expected_hash.clone().or(Some(current.content_hash)),
            force: request.force,
            origin: request.origin.clone(),
        })?;
        Ok(EditFileResult {
            path: request.path.relative().to_string_lossy().into_owned(),
            diff,
            applied: true,
            content_hash: write.value.content_hash,
            event: write.event,
            warnings: write.warnings,
        })
    }
}

impl<C, R> WritesVault for FilesystemService<C, R>
where
    C: Clock,
    R: RoutesOperations,
{
    type Error = FsError;

    fn persist(
        &self,
        request: &WriteMutation,
    ) -> Result<PipelineResult<WriteOutcome>, Self::Error> {
        self.write_file(request)
    }

    fn restore(
        &self,
        request: &RestoreMutation,
    ) -> Result<PipelineResult<WriteOutcome>, Self::Error> {
        self.restore_file(request)
    }

    fn delete(
        &self,
        request: &DeleteMutation,
    ) -> Result<PipelineResult<DeleteOutcome>, Self::Error> {
        self.delete_path(request)
    }
}

impl<C, R> AppendsVault for FilesystemService<C, R>
where
    C: Clock,
    R: RoutesOperations,
{
    type Error = FsError;

    fn append(
        &self,
        request: &AppendMutation,
    ) -> Result<PipelineResult<AppendOutcome>, Self::Error> {
        self.append_file(request)
    }
}

impl<C, R> MovesVault for FilesystemService<C, R>
where
    C: Clock,
    R: RoutesOperations,
{
    type Error = FsError;

    fn move_path(
        &self,
        request: &MoveMutation,
    ) -> Result<PipelineResult<MoveOutcome>, Self::Error> {
        self.move_file(request)
    }
}

fn apply_exact_edit(
    content: &mut String,
    edit: &TextEdit,
    path: &VaultPath,
) -> Result<(), FsError> {
    let mut occurrences = content.match_indices(&edit.old_text);
    let Some((start, _)) = occurrences.next() else {
        let standard_path: &Path = path.into();
        return Err(FsError::EditNotFound {
            path: standard_path.to_path_buf(),
        });
    };
    if occurrences.next().is_some() {
        let standard_path: &Path = path.into();
        return Err(FsError::EditAmbiguous {
            path: standard_path.to_path_buf(),
        });
    }
    let end = start + edit.old_text.len();
    content.replace_range(start..end, &edit.new_text);
    Ok(())
}

impl AppliesMutations for Filesystem {
    type Error = FsError;

    fn write(&self, request: &WriteMutation) -> Result<WriteOutcome, Self::Error> {
        let target = self.revalidate(&request.path)?;
        let existed = target.exists();
        validate_expected_hash(&target, existed, request)?;
        let parent = target.parent().ok_or_else(|| FsError::CreateParent {
            path: target.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "target has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| FsError::CreateParent {
            path: parent.to_path_buf(),
            source,
        })?;
        self.revalidate(&request.path)?;

        let mut temporary =
            NamedTempFile::new_in(parent).map_err(|source| FsError::CreateTemporary {
                path: target.clone(),
                source,
            })?;
        temporary
            .write_all(request.content.as_bytes())
            .map_err(|source| FsError::WriteTemporary {
                path: target.clone(),
                source,
            })?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| FsError::FlushTemporary {
                path: target.clone(),
                source,
            })?;
        self.atomic_write_guard
            .after_flush(&target)
            .map_err(|source| FsError::AtomicWriteInterrupted {
                path: target.clone(),
                source,
            })?;
        temporary
            .persist(&target)
            .map_err(|error| FsError::PersistTemporary {
                path: target.clone(),
                source: error.error,
            })?;

        let mut hasher = Sha256::new();
        hasher.update(request.content.as_bytes());
        Ok(WriteOutcome {
            path: request.path.clone(),
            bytes_written: request.content.len(),
            content_hash: ContentHash::from(<[u8; 32]>::from(hasher.finalize())),
            created: !existed,
        })
    }

    fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<CreateDirectoryOutcome, Self::Error> {
        let target = self.revalidate(&request.path)?;
        if target.exists() {
            if target.is_dir() {
                return Ok(CreateDirectoryOutcome {
                    path: request.path.clone(),
                    created: false,
                });
            }
            return Err(FsError::NotDirectory { path: target });
        }
        fs::create_dir_all(&target).map_err(|source| FsError::CreateDirectory {
            path: target.clone(),
            source,
        })?;
        self.revalidate(&request.path)?;
        Ok(CreateDirectoryOutcome {
            path: request.path.clone(),
            created: true,
        })
    }

    fn move_path(&self, request: &MoveMutation) -> Result<MoveOutcome, Self::Error> {
        let source = self.revalidate(&request.source)?;
        if !source.exists() {
            return Err(FsError::NotFound { path: source });
        }
        let destination = self.revalidate(&request.destination)?;
        if destination.exists() {
            return Err(FsError::DestinationExists { path: destination });
        }
        let parent = destination.parent().ok_or_else(|| FsError::CreateParent {
            path: destination.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "destination has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| FsError::CreateParent {
            path: parent.to_path_buf(),
            source,
        })?;
        self.revalidate(&request.destination)?;
        fs::rename(&source, &destination).map_err(|error| FsError::MovePath {
            source_path: source,
            destination,
            source: error,
        })?;
        Ok(MoveOutcome {
            source: request.source.clone(),
            destination: request.destination.clone(),
        })
    }

    fn delete(&self, request: &DeleteMutation) -> Result<DeleteOutcome, Self::Error> {
        let target = self.revalidate(&request.path)?;
        let metadata = target.metadata().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                FsError::NotFound {
                    path: target.clone(),
                }
            } else {
                FsError::ReadMetadata {
                    path: target.clone(),
                    source,
                }
            }
        })?;
        if metadata.is_dir() {
            if request.mode != DeleteMode::HardRecursive {
                let mut entries =
                    fs::read_dir(&target).map_err(|source| FsError::ReadDirectory {
                        path: target.clone(),
                        source,
                    })?;
                if entries
                    .next()
                    .transpose()
                    .map_err(|source| FsError::ReadDirectoryEntry {
                        path: target.clone(),
                        source,
                    })?
                    .is_some()
                {
                    return Err(FsError::DirectoryNotEmpty { path: target });
                }
            }
        } else if !metadata.is_file() {
            return Err(FsError::NotFile { path: target });
        }
        match request.mode {
            DeleteMode::Trash => {
                trash_context()
                    .delete(&target)
                    .map_err(|source| FsError::TrashPath {
                        path: target.clone(),
                        source,
                    })?;
            }
            DeleteMode::Hard if metadata.is_dir() => {
                fs::remove_dir(&target).map_err(|source| FsError::DeletePath {
                    path: target.clone(),
                    source,
                })?;
            }
            DeleteMode::Hard => {
                fs::remove_file(&target).map_err(|source| FsError::DeletePath {
                    path: target.clone(),
                    source,
                })?;
            }
            DeleteMode::HardRecursive if metadata.is_dir() => {
                fs::remove_dir_all(&target).map_err(|source| FsError::DeletePath {
                    path: target.clone(),
                    source,
                })?;
            }
            DeleteMode::HardRecursive => {
                fs::remove_file(&target).map_err(|source| FsError::DeletePath {
                    path: target.clone(),
                    source,
                })?;
            }
        }
        Ok(DeleteOutcome {
            path: request.path.clone(),
            deleted: true,
            trashed: request.mode == DeleteMode::Trash,
        })
    }

    fn append(&self, request: &AppendMutation) -> Result<AppendOutcome, Self::Error> {
        let target = self.revalidate(&request.path)?;
        let existed = target.exists();
        let parent = target.parent().ok_or_else(|| FsError::CreateParent {
            path: target.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "append target has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| FsError::CreateParent {
            path: parent.to_path_buf(),
            source,
        })?;
        self.revalidate(&request.path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|source| FsError::OpenAppend {
                path: target.clone(),
                source,
            })?;
        let empty = file
            .metadata()
            .map_err(|source| FsError::ReadMetadata {
                path: target.clone(),
                source,
            })?
            .len()
            == 0;
        let mut appended = String::new();
        if empty {
            appended.push_str(&request.preamble);
        }
        appended.push_str(&request.content);
        file.write_all(appended.as_bytes())
            .map_err(|source| FsError::AppendContent {
                path: target.clone(),
                source,
            })?;
        file.sync_data().map_err(|source| FsError::FlushAppend {
            path: target,
            source,
        })?;
        Ok(AppendOutcome {
            path: request.path.clone(),
            bytes_appended: appended.len(),
            created: !existed,
        })
    }
}

/// Builds the `TrashContext` used for `DeleteMode::Trash`. macOS's default
/// delete method shells out to `osascript` to ask Finder to perform the
/// move; besides being slower and needing Automation permission granted to
/// this process, Finder's involvement can leave a `.DS_Store` behind in the
/// containing directory as a side effect of it visiting that directory,
/// which then makes a directory the caller just emptied look non-empty
/// again. `NsFileManager` performs the same trash move directly, without
/// invoking Finder.
#[cfg(target_os = "macos")]
fn trash_context() -> trash::TrashContext {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context
}

#[cfg(not(target_os = "macos"))]
fn trash_context() -> trash::TrashContext {
    trash::TrashContext::default()
}

impl Filesystem {
    fn revalidate(&self, path: &VaultPath) -> Result<PathBuf, FsError> {
        let standard_path: &Path = path.into();
        if !self.roots.authorises(path) {
            return Err(FsError::OutsideRoot {
                path: standard_path.to_path_buf(),
            });
        }
        let raw = standard_path.to_string_lossy();
        let refreshed = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: &raw,
        })
        .map_err(|source| FsError::PathValidation {
            path: standard_path.to_path_buf(),
            source,
        })?;
        if refreshed != *path {
            return Err(FsError::Conflict {
                path: standard_path.to_path_buf(),
                current_hash: None,
            });
        }
        Ok(standard_path.to_path_buf())
    }
}

fn validate_expected_hash(
    target: &Path,
    existed: bool,
    request: &WriteMutation,
) -> Result<(), FsError> {
    if request.force {
        return Ok(());
    }
    match (existed, request.expected_hash.as_ref()) {
        (false, None) => Ok(()),
        (true, None) => Err(FsError::ExpectedHashRequired {
            path: target.to_path_buf(),
            current_hash: hash_file(target)?,
        }),
        (true, Some(expected)) => {
            let current = hash_file(target)?;
            if &current == expected {
                Ok(())
            } else {
                Err(FsError::Conflict {
                    path: target.to_path_buf(),
                    current_hash: Some(current),
                })
            }
        }
        (false, Some(_)) => Err(FsError::Conflict {
            path: target.to_path_buf(),
            current_hash: None,
        }),
    }
}
