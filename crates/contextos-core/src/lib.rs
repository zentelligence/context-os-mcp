#![forbid(unsafe_code)]

mod path;
mod pipeline;
mod routing;
mod tags;
mod vault;

pub use tags::extract_tags;

pub use path::{PathError, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultRootInput, VaultSet};
pub use pipeline::{
    AppendMutation, AppendOutcome, AppliesMutations, Clock, ContentHash, ContentHashError, CreateDirectoryMutation,
    CreateDirectoryOutcome, DeleteMode, DeleteMutation, DeleteOutcome, MoveMutation, MoveOutcome, OpKind,
    OperationEvent, OperationWarning, Origin, PipelineConfig, PipelineResult, RestoreMutation, RoutedPipelineConfig,
    RoutedWritePipeline, SystemClock, WriteMutation, WriteOutcome, WritePipeline,
};
pub use routing::{
    LogsOperations, MaintainsIndexes, NoSearchUpdates, NoSubstrateServices, OperationRoute, OperationRouter,
    OperationRouterConfig, OperationService, RoutesOperations, UpdatesSearch, VersionsVault,
};
pub use vault::{AppendsVault, ListsVault, MovesVault, ReadsVault, VaultEntry, VaultEntryKind, VaultText, WritesVault};
