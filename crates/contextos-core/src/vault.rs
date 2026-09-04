use crate::{
    AppendMutation, AppendOutcome, ContentHash, DeleteMutation, DeleteOutcome, MoveMutation, MoveOutcome,
    PipelineResult, RestoreMutation, VaultPath, WriteMutation, WriteOutcome,
};

/// UTF-8 vault content plus its optimistic-concurrency identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultText {
    pub content: String,
    pub content_hash: ContentHash,
}

/// Filesystem kind exposed through the vault discovery port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultEntryKind {
    File,
    Directory,
}

/// One direct child exposed through the vault discovery port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultEntry {
    pub name: String,
    pub kind: VaultEntryKind,
}

/// Consumer-owned port for bounded optional UTF-8 reads.
pub trait ReadsVault: Send + Sync {
    type Error;

    /// Reads text, returning `None` only when the path does not exist.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error for every failure other than absence.
    fn read_optional_text(&self, path: &VaultPath) -> Result<Option<VaultText>, Self::Error>;
}

/// Consumer-owned port for direct-child discovery.
pub trait ListsVault: Send + Sync {
    type Error;

    /// Lists direct children in deterministic name order.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed discovery error.
    fn list(&self, path: &VaultPath) -> Result<Vec<VaultEntry>, Self::Error>;
}

/// Consumer-owned port for append-only vault records.
pub trait AppendsVault: Send + Sync {
    type Error;

    /// Appends one complete record through the shared mutation pipeline.
    ///
    /// # Errors
    ///
    /// Returns the primary append error. Secondary failures remain in the
    /// successful result's warnings.
    fn append(&self, request: &AppendMutation) -> Result<PipelineResult<AppendOutcome>, Self::Error>;
}

/// Consumer-owned port for atomic vault renames.
pub trait MovesVault: Send + Sync {
    type Error;

    /// Moves one path through the shared mutation pipeline without replacing
    /// an existing destination.
    ///
    /// # Errors
    ///
    /// Returns the primary move error. Secondary failures remain in the
    /// successful result's warnings.
    fn move_path(&self, request: &MoveMutation) -> Result<PipelineResult<MoveOutcome>, Self::Error>;
}

/// Consumer-owned port for atomic, conflict-aware vault persistence.
pub trait WritesVault: Send + Sync {
    type Error;

    /// Persists one validated mutation through the shared write pipeline.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in the
    /// successful result's warnings.
    fn persist(&self, request: &WriteMutation) -> Result<PipelineResult<WriteOutcome>, Self::Error>;

    /// Materialises historical content as a new forward mutation.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in the
    /// successful result's warnings.
    fn restore(&self, request: &RestoreMutation) -> Result<PipelineResult<WriteOutcome>, Self::Error>;

    /// Deletes a validated file or empty directory through the shared pipeline.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in the
    /// successful result's warnings.
    fn delete(&self, request: &DeleteMutation) -> Result<PipelineResult<DeleteOutcome>, Self::Error>;
}
