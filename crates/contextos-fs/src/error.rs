use std::io;
use std::path::{Path, PathBuf};

use contextos_core::PathError;
use serde::Serialize;
use thiserror::Error;

/// Stable, serialisable error detail returned for an item in a batch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FsErrorInfo {
    pub code: &'static str,
    pub message: String,
    pub remediation: &'static str,
}

impl From<&FsError> for FsErrorInfo {
    fn from(value: &FsError) -> Self {
        Self {
            code: value.code(),
            message: value.to_string(),
            remediation: value.remediation(),
        }
    }
}

/// Filesystem failures with stable MCP-facing classifications.
#[derive(Debug, Error)]
pub enum FsError {
    #[error("path does not belong to an allowed vault root: {path}")]
    OutsideRoot { path: PathBuf },
    #[error("path is not a directory: {path}")]
    NotDirectory { path: PathBuf },
    #[error("path changed or no longer passes vault validation: {path}")]
    PathValidation {
        path: PathBuf,
        #[source]
        source: PathError,
    },
    #[error("path does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("path is not a regular file: {path}")]
    NotFile { path: PathBuf },
    #[error("file is {size} bytes, above the unlimited-read maximum of {maximum} bytes: {path}")]
    TooLarge {
        path: PathBuf,
        size: u64,
        maximum: u64,
    },
    #[error("file is binary or is not valid UTF-8: {path}")]
    Binary { path: PathBuf },
    #[error("file changed since the caller observed it: {path}")]
    Conflict { path: PathBuf },
    #[error("destination already exists: {path}")]
    DestinationExists { path: PathBuf },
    #[error("exact edit text was not found: {path}")]
    EditNotFound { path: PathBuf },
    #[error("exact edit text occurs more than once: {path}")]
    EditAmbiguous { path: PathBuf },
    #[error("batch contains {count} paths, above the configured maximum of {maximum}")]
    BatchTooLarge { count: usize, maximum: usize },
    #[error("filesystem configuration has {limit_count} limit sets for {root_count} vault roots")]
    LimitCountMismatch {
        root_count: usize,
        limit_count: usize,
    },
    #[error("filesystem limits for vault root {root_index} must be greater than zero")]
    InvalidLimits { root_index: usize },
    #[error(
        "filesystem configuration has {hidden_count} hidden-pattern sets for {root_count} vault roots"
    )]
    HiddenCountMismatch {
        root_count: usize,
        hidden_count: usize,
    },
    #[error("line range must be 1-based, inclusive, and ordered: {from}..={to}")]
    InvalidRange { from: usize, to: usize },
    #[error("glob pattern is invalid: {pattern}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("exclude pattern is invalid: {pattern}")]
    InvalidExclude {
        pattern: String,
        #[source]
        source: ignore::Error,
    },
    #[error("directory could not be read: {path}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("directory entry could not be read beneath: {path}")]
    ReadDirectoryEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("directory walk failed beneath: {path}")]
    WalkDirectory {
        path: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("path does not share its traversal root: {path}")]
    PrefixMismatch {
        path: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
    #[error("file metadata could not be read: {path}")]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("file could not be opened for reading: {path}")]
    OpenRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("file content could not be read: {path}")]
    ReadContent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parent directory could not be created: {path}")]
    CreateParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("temporary file could not be created beside target: {path}")]
    CreateTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("temporary content could not be written: {path}")]
    WriteTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("temporary content could not be flushed durably: {path}")]
    FlushTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("atomic write was interrupted after temporary content was flushed: {path}")]
    AtomicWriteInterrupted {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("temporary content could not atomically replace target: {path}")]
    PersistTemporary {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("directory tree could not be created: {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("path could not be moved from {source_path} to {destination}")]
    MovePath {
        source_path: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("directory must be empty before deletion: {path}")]
    DirectoryNotEmpty { path: PathBuf },
    #[error("path could not be moved to the platform trash: {path}")]
    TrashPath {
        path: PathBuf,
        #[source]
        source: trash::Error,
    },
    #[error("path could not be deleted: {path}")]
    DeletePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("file could not be opened for append: {path}")]
    OpenAppend {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("content could not be appended: {path}")]
    AppendContent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("appended content could not be flushed durably: {path}")]
    FlushAppend {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl FsError {
    /// Returns the offending path when this failure is path-specific.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::BatchTooLarge { .. }
            | Self::LimitCountMismatch { .. }
            | Self::InvalidLimits { .. }
            | Self::HiddenCountMismatch { .. }
            | Self::InvalidRange { .. }
            | Self::InvalidGlob { .. }
            | Self::InvalidExclude { .. } => None,
            Self::OutsideRoot { path }
            | Self::NotDirectory { path }
            | Self::PathValidation { path, .. }
            | Self::NotFound { path }
            | Self::NotFile { path }
            | Self::TooLarge { path, .. }
            | Self::Binary { path }
            | Self::Conflict { path }
            | Self::DestinationExists { path }
            | Self::EditNotFound { path }
            | Self::EditAmbiguous { path }
            | Self::ReadDirectory { path, .. }
            | Self::ReadDirectoryEntry { path, .. }
            | Self::WalkDirectory { path, .. }
            | Self::PrefixMismatch { path, .. }
            | Self::ReadMetadata { path, .. }
            | Self::OpenRead { path, .. }
            | Self::ReadContent { path, .. }
            | Self::CreateParent { path, .. }
            | Self::CreateTemporary { path, .. }
            | Self::WriteTemporary { path, .. }
            | Self::FlushTemporary { path, .. }
            | Self::AtomicWriteInterrupted { path, .. }
            | Self::PersistTemporary { path, .. }
            | Self::CreateDirectory { path, .. }
            | Self::DirectoryNotEmpty { path }
            | Self::TrashPath { path, .. }
            | Self::DeletePath { path, .. }
            | Self::OpenAppend { path, .. }
            | Self::AppendContent { path, .. }
            | Self::FlushAppend { path, .. } => Some(path),
            Self::MovePath { source_path, .. } => Some(source_path),
        }
    }

    /// Stable machine-readable code for MCP error data.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OutsideRoot { .. } => "path/outside-root",
            Self::PathValidation { source, .. } => source.code(),
            Self::NotFound { .. } => "path/not-found",
            Self::Conflict { .. } => "io/conflict",
            Self::DestinationExists { .. } => "io/destination-exists",
            Self::EditNotFound { .. } => "edit/not-found",
            Self::EditAmbiguous { .. } => "edit/ambiguous",
            Self::TooLarge { .. } => "io/too-large",
            Self::Binary { .. } => "io/binary",
            Self::InvalidRange { .. } => "io/invalid-range",
            Self::InvalidGlob { .. } | Self::InvalidExclude { .. } => "io/invalid-pattern",
            Self::BatchTooLarge { .. } => "io/batch-too-large",
            Self::AtomicWriteInterrupted { .. } => "io/atomic-write-interrupted",
            Self::DirectoryNotEmpty { .. } => "io/directory-not-empty",
            Self::TrashPath { .. } | Self::DeletePath { .. } => "io/delete",
            Self::LimitCountMismatch { .. }
            | Self::InvalidLimits { .. }
            | Self::HiddenCountMismatch { .. } => "server/configuration",
            Self::NotFile { .. }
            | Self::NotDirectory { .. }
            | Self::ReadDirectory { .. }
            | Self::ReadDirectoryEntry { .. }
            | Self::WalkDirectory { .. }
            | Self::PrefixMismatch { .. }
            | Self::ReadMetadata { .. }
            | Self::OpenRead { .. }
            | Self::ReadContent { .. }
            | Self::CreateParent { .. }
            | Self::CreateTemporary { .. }
            | Self::WriteTemporary { .. }
            | Self::FlushTemporary { .. }
            | Self::PersistTemporary { .. }
            | Self::CreateDirectory { .. }
            | Self::MovePath { .. }
            | Self::OpenAppend { .. }
            | Self::AppendContent { .. }
            | Self::FlushAppend { .. } => "io/filesystem",
        }
    }

    /// Actionable remediation suitable for an MCP error response.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::OutsideRoot { .. } => "Use a path from an allowed vault root.",
            Self::PathValidation { source, .. } => source.remediation(),
            Self::NotFound { .. } => "Check the path and list its parent directory.",
            Self::Conflict { .. } => {
                "Read the current content and retry with its hash, or pass force explicitly."
            }
            Self::DestinationExists { .. } => "Choose a destination that does not exist.",
            Self::EditNotFound { .. } => {
                "Read the current file and supply text that exists exactly once."
            }
            Self::EditAmbiguous { .. } => {
                "Include more surrounding text so the edit target is unique."
            }
            Self::LimitCountMismatch { .. }
            | Self::InvalidLimits { .. }
            | Self::HiddenCountMismatch { .. } => {
                "Correct the configured limits and hidden patterns for every vault root and restart the server."
            }
            Self::NotFile { .. } => "Pass the path of a regular file.",
            Self::TooLarge { .. } => "Pass head, tail, or an explicit line range.",
            Self::Binary { .. } => "Use fs_attach_file for binary content.",
            Self::BatchTooLarge { .. } => "Split the request into smaller batches.",
            Self::InvalidRange { .. } => {
                "Use positive 1-based line numbers with from_line no greater than to_line."
            }
            Self::InvalidGlob { .. } | Self::InvalidExclude { .. } => {
                "Use a valid gitignore-style glob pattern."
            }
            Self::NotDirectory { .. } => "Pass the path of a directory.",
            Self::DirectoryNotEmpty { .. } => "Delete the directory contents first.",
            Self::ReadDirectory { .. }
            | Self::ReadDirectoryEntry { .. }
            | Self::WalkDirectory { .. }
            | Self::PrefixMismatch { .. } => {
                "Check that the directory is readable and retry the operation."
            }
            Self::ReadMetadata { .. } | Self::OpenRead { .. } | Self::ReadContent { .. } => {
                "Check that the file is readable and retry the operation."
            }
            Self::CreateParent { .. }
            | Self::CreateTemporary { .. }
            | Self::WriteTemporary { .. }
            | Self::FlushTemporary { .. }
            | Self::AtomicWriteInterrupted { .. }
            | Self::PersistTemporary { .. }
            | Self::CreateDirectory { .. }
            | Self::MovePath { .. }
            | Self::TrashPath { .. }
            | Self::DeletePath { .. }
            | Self::OpenAppend { .. }
            | Self::AppendContent { .. }
            | Self::FlushAppend { .. } => {
                "Check vault permissions and available storage, then retry the operation."
            }
        }
    }
}
