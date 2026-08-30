use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;

use crate::{RoutesOperations, VaultPath};

/// Validated SHA-256 identity rendered as lowercase hexadecimal.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl<'a> From<&'a ContentHash> for &'a str {
    fn from(value: &'a ContentHash) -> Self {
        &value.0
    }
}

impl From<[u8; 32]> for ContentHash {
    fn from(value: [u8; 32]) -> Self {
        let mut encoded = String::with_capacity(64);
        for byte in value {
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
        Self(encoded)
    }
}

const fn hex_digit(value: u8) -> char {
    match value {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

impl TryFrom<&str> for ContentHash {
    type Error = ContentHashError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.len() != 64 {
            return Err(ContentHashError::InvalidLength {
                actual: value.len(),
            });
        }
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContentHashError::InvalidCharacter);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl TryFrom<String> for ContentHash {
    type Error = ContentHashError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ContentHashError {
    #[error("SHA-256 hash must contain 64 hexadecimal characters, received {actual}")]
    InvalidLength { actual: usize },
    #[error("SHA-256 hash contains a non-hexadecimal character")]
    InvalidCharacter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    Tool(String),
    Internal(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpKind {
    Create,
    Modify,
    Move,
    Delete,
    Restore,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationEvent {
    pub kind: OpKind,
    pub paths: Vec<VaultPath>,
    pub origin: Origin,
    pub summary: String,
    pub at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationWarning {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteMutation {
    pub path: VaultPath,
    pub content: String,
    pub expected_hash: Option<ContentHash>,
    pub force: bool,
    pub origin: Origin,
}

/// Historical UTF-8 content materialised as a forward recovery mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreMutation {
    pub path: VaultPath,
    pub content: String,
    pub expected_hash: Option<ContentHash>,
    pub origin: Origin,
}

/// Deletion policy selected after configuration authorisation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
    Trash,
    Hard,
    /// Gated service-only removal for an MCP-owned recovery subtree.
    HardRecursive,
}

/// One validated file or empty-directory deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteMutation {
    pub path: VaultPath,
    pub mode: DeleteMode,
    pub origin: Origin,
}

/// Completed deletion identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteOutcome {
    pub path: VaultPath,
    pub deleted: bool,
    pub trashed: bool,
}

/// Append-only text mutation with content used only to initialise an empty file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendMutation {
    pub path: VaultPath,
    pub preamble: String,
    pub content: String,
    pub origin: Origin,
}

/// Completed append identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppendOutcome {
    pub path: VaultPath,
    pub bytes_appended: usize,
    pub created: bool,
}

impl From<&RestoreMutation> for WriteMutation {
    fn from(value: &RestoreMutation) -> Self {
        Self {
            path: value.path.clone(),
            content: value.content.clone(),
            expected_hash: value.expected_hash.clone(),
            force: false,
            origin: value.origin.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteOutcome {
    pub path: VaultPath,
    pub bytes_written: usize,
    pub content_hash: ContentHash,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDirectoryMutation {
    pub path: VaultPath,
    pub origin: Origin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDirectoryOutcome {
    pub path: VaultPath,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveMutation {
    pub source: VaultPath,
    pub destination: VaultPath,
    pub origin: Origin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveOutcome {
    pub source: VaultPath,
    pub destination: VaultPath,
}

/// Persistence port implemented by the native filesystem adapter.
pub trait AppliesMutations: Send + Sync {
    type Error;

    /// Persists an atomic file create or replacement.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed persistence or conflict error.
    fn write(&self, request: &WriteMutation) -> Result<WriteOutcome, Self::Error>;

    /// Creates a directory tree idempotently.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed persistence error.
    fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<CreateDirectoryOutcome, Self::Error>;

    /// Atomically renames a file or directory without replacing a destination.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed persistence error.
    fn move_path(&self, request: &MoveMutation) -> Result<MoveOutcome, Self::Error>;

    /// Deletes one file or empty directory using the authorised mode.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed confinement or deletion error.
    fn delete(&self, request: &DeleteMutation) -> Result<DeleteOutcome, Self::Error>;

    /// Appends one complete text record, initialising an empty file if needed.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed confinement or append error.
    fn append(&self, request: &AppendMutation) -> Result<AppendOutcome, Self::Error>;
}

/// Injectable time source for deterministic operation events.
pub trait Clock: Clone + Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[derive(Clone, Debug)]
pub struct PipelineConfig<A, C> {
    pub adapter: A,
    pub clock: C,
}

#[derive(Clone, Debug)]
pub struct WritePipeline<A, C> {
    adapter: A,
    clock: C,
}

impl<A, C> From<PipelineConfig<A, C>> for WritePipeline<A, C> {
    fn from(value: PipelineConfig<A, C>) -> Self {
        Self {
            adapter: value.adapter,
            clock: value.clock,
        }
    }
}

/// Dependencies for persistence followed by central service routing.
#[derive(Clone, Debug)]
pub struct RoutedPipelineConfig<A, C, R> {
    pub adapter: A,
    pub clock: C,
    pub services: R,
}

/// Mutation pipeline that preserves successful writes and accumulates warnings.
#[derive(Clone, Debug)]
pub struct RoutedWritePipeline<A, C, R> {
    pipeline: WritePipeline<A, C>,
    services: R,
}

impl<A, C, R> From<RoutedPipelineConfig<A, C, R>> for RoutedWritePipeline<A, C, R> {
    fn from(value: RoutedPipelineConfig<A, C, R>) -> Self {
        Self {
            pipeline: WritePipeline::from(PipelineConfig {
                adapter: value.adapter,
                clock: value.clock,
            }),
            services: value.services,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PipelineResult<T> {
    pub value: T,
    pub event: Option<OperationEvent>,
    pub warnings: Vec<OperationWarning>,
}

/// Renders a vault-relative path using forward slashes regardless of
/// platform, for operation-event summary text: `VaultPath::relative`
/// returns a native path, and `Path::display` alone would leak the native
/// separator (`\` on Windows) into persisted log lines and MCP tool output.
fn forward_slash_display(path: &VaultPath) -> String {
    path.relative()
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl<A, C> WritePipeline<A, C>
where
    A: AppliesMutations,
    C: Clock,
{
    /// Persists a write before constructing its operation event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when persistence
    /// does not complete.
    pub fn write(&self, request: &WriteMutation) -> Result<PipelineResult<WriteOutcome>, A::Error> {
        let value = self.adapter.write(request)?;
        let kind = if value.created {
            OpKind::Create
        } else {
            OpKind::Modify
        };
        let action = if value.created {
            "Created"
        } else {
            "Overwrote"
        };
        let event = OperationEvent {
            kind,
            paths: vec![value.path.clone()],
            origin: request.origin.clone(),
            summary: format!(
                "{action} {} ({} bytes)",
                forward_slash_display(&value.path),
                value.bytes_written
            ),
            at: self.clock.now(),
        };
        Ok(PipelineResult {
            value,
            event: Some(event),
            warnings: Vec::new(),
        })
    }

    /// Persists historical content before constructing a restore event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when persistence
    /// does not complete.
    pub fn restore(
        &self,
        request: &RestoreMutation,
    ) -> Result<PipelineResult<WriteOutcome>, A::Error> {
        let value = self.adapter.write(&WriteMutation::from(request))?;
        let event = OperationEvent {
            kind: OpKind::Restore,
            paths: vec![value.path.clone()],
            origin: request.origin.clone(),
            summary: format!(
                "Restored {} ({} bytes)",
                forward_slash_display(&value.path),
                value.bytes_written
            ),
            at: self.clock.now(),
        };
        Ok(PipelineResult {
            value,
            event: Some(event),
            warnings: Vec::new(),
        })
    }

    /// Deletes a path before constructing one delete event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when deletion does
    /// not complete.
    pub fn delete(
        &self,
        request: &DeleteMutation,
    ) -> Result<PipelineResult<DeleteOutcome>, A::Error> {
        let value = self.adapter.delete(request)?;
        let event = OperationEvent {
            kind: OpKind::Delete,
            paths: vec![value.path.clone()],
            origin: request.origin.clone(),
            summary: format!("Deleted {}", forward_slash_display(&value.path)),
            at: self.clock.now(),
        };
        Ok(PipelineResult {
            value,
            event: Some(event),
            warnings: Vec::new(),
        })
    }

    /// Appends content before constructing one internal mutation event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when the append does
    /// not complete.
    pub fn append(
        &self,
        request: &AppendMutation,
    ) -> Result<PipelineResult<AppendOutcome>, A::Error> {
        let value = self.adapter.append(request)?;
        let event = OperationEvent {
            kind: if value.created {
                OpKind::Create
            } else {
                OpKind::Modify
            },
            paths: vec![value.path.clone()],
            origin: request.origin.clone(),
            summary: format!(
                "Appended {} bytes to {}",
                value.bytes_appended,
                forward_slash_display(&value.path)
            ),
            at: self.clock.now(),
        };
        Ok(PipelineResult {
            value,
            event: Some(event),
            warnings: Vec::new(),
        })
    }

    /// Creates a directory tree before constructing its operation event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when persistence
    /// does not complete.
    pub fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<PipelineResult<CreateDirectoryOutcome>, A::Error> {
        let value = self.adapter.create_directory(request)?;
        let event = value.created.then(|| OperationEvent {
            kind: OpKind::Create,
            paths: vec![value.path.clone()],
            origin: request.origin.clone(),
            summary: format!("Created directory {}", forward_slash_display(&value.path)),
            at: self.clock.now(),
        });
        Ok(PipelineResult {
            value,
            event,
            warnings: Vec::new(),
        })
    }

    /// Moves a path before constructing one two-path operation event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error and emits no event when the move does
    /// not complete.
    pub fn move_path(
        &self,
        request: &MoveMutation,
    ) -> Result<PipelineResult<MoveOutcome>, A::Error> {
        let value = self.adapter.move_path(request)?;
        let event = OperationEvent {
            kind: OpKind::Move,
            paths: vec![value.source.clone(), value.destination.clone()],
            origin: request.origin.clone(),
            summary: format!(
                "Moved {} to {}",
                forward_slash_display(&value.source),
                forward_slash_display(&value.destination)
            ),
            at: self.clock.now(),
        };
        Ok(PipelineResult {
            value,
            event: Some(event),
            warnings: Vec::new(),
        })
    }
}

impl<A, C, R> RoutedWritePipeline<A, C, R>
where
    A: AppliesMutations,
    C: Clock,
    R: RoutesOperations,
{
    /// Persists a write, then routes its completed operation event.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn write(&self, request: &WriteMutation) -> Result<PipelineResult<WriteOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.write(request)?))
    }

    /// Persists historical content, then routes its restore event.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn restore(
        &self,
        request: &RestoreMutation,
    ) -> Result<PipelineResult<WriteOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.restore(request)?))
    }

    /// Deletes a path, then routes its completed delete event.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn delete(
        &self,
        request: &DeleteMutation,
    ) -> Result<PipelineResult<DeleteOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.delete(request)?))
    }

    /// Appends a complete record, then routes its completed mutation event.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn append(
        &self,
        request: &AppendMutation,
    ) -> Result<PipelineResult<AppendOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.append(request)?))
    }

    /// Persists a directory create, then routes a newly created outcome.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn create_directory(
        &self,
        request: &CreateDirectoryMutation,
    ) -> Result<PipelineResult<CreateDirectoryOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.create_directory(request)?))
    }

    /// Persists a move, then routes its single two-path operation event.
    ///
    /// # Errors
    ///
    /// Returns only primary persistence errors. Secondary failures are warnings.
    pub fn move_path(
        &self,
        request: &MoveMutation,
    ) -> Result<PipelineResult<MoveOutcome>, A::Error> {
        Ok(self.route_result(self.pipeline.move_path(request)?))
    }

    fn route_result<T>(&self, mut result: PipelineResult<T>) -> PipelineResult<T> {
        if let Some(event) = &result.event {
            result.warnings.extend(self.services.route(event));
        }
        result
    }
}
