#![forbid(unsafe_code)]

mod discover;
mod error;
mod mutate;
mod read;

pub use contextos_core::ContentHash;
pub use discover::{
    AllowedDirectory, DirectoryEntry, DirectoryListing, DirectoryTreeRequest, EntryKind, FileInfo,
    FileInfoRequest, ListDirectoryRequest, ListDirectoryWithSizesRequest, SearchFilesRequest,
    SortBy, TreeNode, default_hidden_patterns,
};
pub use error::{FsError, FsErrorInfo};
pub use mutate::{
    EditFileRequest, EditFileResult, FilesystemService, FilesystemServiceConfig,
    RoutedFilesystemServiceConfig, TextEdit,
};
pub use read::{
    Attachment, AttachmentRequest, Filesystem, FilesystemConfig, FsLimits, GuardsAtomicWrites,
    LineRange, ReadLimit, ReadManyRequest, ReadManyResult, ReadTextRequest, ReadTextResult,
    mime_type_for_extension,
};

impl contextos_core::ReadsVault for Filesystem {
    type Error = FsError;

    fn read_optional_text(
        &self,
        path: &contextos_core::VaultPath,
    ) -> Result<Option<contextos_core::VaultText>, Self::Error> {
        match self.read_text(&ReadTextRequest {
            path: path.clone(),
            limit: None,
        }) {
            Ok(result) => Ok(Some(contextos_core::VaultText {
                content: result.content,
                content_hash: result.content_hash,
            })),
            Err(FsError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl contextos_core::ListsVault for Filesystem {
    type Error = FsError;

    fn list(
        &self,
        path: &contextos_core::VaultPath,
    ) -> Result<Vec<contextos_core::VaultEntry>, Self::Error> {
        Ok(self
            .list_directory(&ListDirectoryRequest { path: path.clone() })?
            .entries
            .into_iter()
            .map(contextos_core::VaultEntry::from)
            .collect())
    }
}

impl From<EntryKind> for contextos_core::VaultEntryKind {
    fn from(value: EntryKind) -> Self {
        match value {
            EntryKind::File => Self::File,
            EntryKind::Directory => Self::Directory,
        }
    }
}

impl From<DirectoryEntry> for contextos_core::VaultEntry {
    fn from(value: DirectoryEntry) -> Self {
        Self {
            name: value.name,
            kind: contextos_core::VaultEntryKind::from(value.kind),
        }
    }
}
