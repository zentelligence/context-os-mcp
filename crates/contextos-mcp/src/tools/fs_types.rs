//! DTOs, request/result shapes, and structured-input helpers for the
//! plain-filesystem tools in [`crate::tools::fs`]. Split out purely to
//! keep `fs.rs` under the project's file-size limit; every item here is
//! `pub(crate)` for that sibling module to use.

use base64::Engine;
use contextos_core::{DeleteOutcome, MoveOutcome, PipelineResult, VaultSet, WriteOutcome};
use contextos_fs::{
    AllowedDirectory, Attachment, DirectoryEntry, DirectoryListing, EditFileResult, EntryKind, FileInfo, FsErrorInfo,
    LineRange, ReadLimit, ReadManyResult, ReadTextResult, SortBy, TextEdit, TreeNode,
};
use rmcp::model::ResourceContents;
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::resource_support::{optional_u64_schema, resource_uri_for};
use crate::server::WarningMessages;
use crate::tool_error::ToolError;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadTextToolResult {
    pub(crate) content: String,
    pub(crate) line_count: usize,
    pub(crate) content_hash: String,
    pub(crate) truncated: bool,
}

impl From<ReadTextResult> for ReadTextToolResult {
    fn from(value: ReadTextResult) -> Self {
        let content_hash: &str = (&value.content_hash).into();
        Self {
            content: value.content,
            line_count: value.line_count,
            content_hash: content_hash.to_owned(),
            truncated: value.truncated,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BatchErrorToolResult {
    code: String,
    message: String,
    remediation: String,
}

impl From<FsErrorInfo> for BatchErrorToolResult {
    fn from(value: FsErrorInfo) -> Self {
        Self {
            code: value.code.to_owned(),
            message: value.message,
            remediation: value.remediation.to_owned(),
        }
    }
}

impl From<contextos_core::PathError> for BatchErrorToolResult {
    fn from(value: contextos_core::PathError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
            remediation: value.remediation().to_owned(),
        }
    }
}

/// Reuses the single `ToolError` -> `ToolFailure` translation
/// (`tool_error.rs`) so a bulk `fs_delete_file` item's failure carries the
/// exact same stable code, message, and remediation a whole-call error
/// would, instead of a second, divergent mapping.
impl From<crate::tool_error::ToolFailure> for BatchErrorToolResult {
    fn from(value: crate::tool_error::ToolFailure) -> Self {
        Self {
            code: value.code,
            message: value.message,
            remediation: value.remediation,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadManyItemToolResult {
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content_hash: Option<String>,
    /// `true` when `content` is a bounded preview rather than the whole
    /// file, because it reached the `resource_link` threshold;
    /// `fs_read_multiple_files` has no per-file head/tail/range parameter,
    /// so this is the only reason a batch item is ever partial.
    /// `content_hash` always describes the complete file regardless.
    pub(crate) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<BatchErrorToolResult>,
}

impl From<ReadManyResult> for ReadManyItemToolResult {
    fn from(value: ReadManyResult) -> Self {
        let content_hash = value.content_hash.map(|hash| {
            let text: &str = (&hash).into();
            text.to_owned()
        });
        Self {
            path: value.path,
            content: value.content,
            content_hash,
            truncated: false,
            error: value.error.map(BatchErrorToolResult::from),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ReadManyToolResult {
    pub(crate) files: Vec<ReadManyItemToolResult>,
}

impl From<Vec<ReadManyResult>> for ReadManyToolResult {
    fn from(value: Vec<ReadManyResult>) -> Self {
        Self {
            files: value.into_iter().map(ReadManyItemToolResult::from).collect(),
        }
    }
}

pub(crate) enum AttachmentResource {
    Text {
        uri: String,
        mime_type: String,
        text: String,
    },
    Blob {
        uri: String,
        mime_type: String,
        blob: String,
    },
}

impl TryFrom<(Attachment, &VaultSet)> for AttachmentResource {
    type Error = ToolError;

    fn try_from((value, roots): (Attachment, &VaultSet)) -> Result<Self, Self::Error> {
        let uri = resource_uri_for(&value.vault_path, roots).map_err(|_| ToolError::AttachmentUriInvalid)?;
        Ok(if value.text {
            match String::from_utf8(value.bytes) {
                Ok(text) => Self::Text {
                    uri,
                    mime_type: value.mime_type,
                    text,
                },
                Err(error) => Self::Blob {
                    uri,
                    mime_type: value.mime_type,
                    blob: base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
                },
            }
        } else {
            Self::Blob {
                uri,
                mime_type: value.mime_type,
                blob: base64::engine::general_purpose::STANDARD.encode(value.bytes),
            }
        })
    }
}

impl From<AttachmentResource> for ResourceContents {
    fn from(value: AttachmentResource) -> Self {
        match value {
            AttachmentResource::Text { uri, mime_type, text } => Self::TextResourceContents {
                uri,
                mime_type: Some(mime_type),
                text,
                meta: None,
            },
            AttachmentResource::Blob { uri, mime_type, blob } => Self::BlobResourceContents {
                uri,
                mime_type: Some(mime_type),
                blob,
                meta: None,
            },
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct WriteToolResult {
    path: String,
    bytes_written: usize,
    content_hash: String,
    created: bool,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for WriteToolResult {
    fn from(result: PipelineResult<WriteOutcome>) -> Self {
        let content_hash: &str = (&result.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(result.warnings);
        Self {
            path: result.value.path.relative().to_string_lossy().into_owned(),
            bytes_written: result.value.bytes_written,
            content_hash: content_hash.to_owned(),
            created: result.value.created,
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct EditToolResult {
    path: String,
    diff: String,
    applied: bool,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<EditFileResult> for EditToolResult {
    fn from(value: EditFileResult) -> Self {
        let content_hash: &str = (&value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.path,
            diff: value.diff,
            applied: value.applied,
            content_hash: content_hash.to_owned(),
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct CreateDirectoryToolResult {
    path: String,
    created: bool,
    warnings: Vec<String>,
}

impl From<PipelineResult<contextos_core::CreateDirectoryOutcome>> for CreateDirectoryToolResult {
    fn from(result: PipelineResult<contextos_core::CreateDirectoryOutcome>) -> Self {
        let WarningMessages(warnings) = WarningMessages::from(result.warnings);
        Self {
            path: result.value.path.relative().to_string_lossy().into_owned(),
            created: result.value.created,
            warnings,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EntryKindToolResult {
    File,
    #[serde(rename = "dir")]
    Directory,
}

impl From<EntryKind> for EntryKindToolResult {
    fn from(value: EntryKind) -> Self {
        match value {
            EntryKind::File => Self::File,
            EntryKind::Directory => Self::Directory,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DirectoryEntryToolResult {
    name: String,
    kind: EntryKindToolResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
}

impl From<DirectoryEntry> for DirectoryEntryToolResult {
    fn from(value: DirectoryEntry) -> Self {
        Self {
            name: value.name,
            kind: value.kind.into(),
            size: value.size,
            modified: value.modified.map(|timestamp| timestamp.to_string()),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DirectoryListingToolResult {
    entries: Vec<DirectoryEntryToolResult>,
    rendered: String,
}

impl From<DirectoryListing> for DirectoryListingToolResult {
    fn from(value: DirectoryListing) -> Self {
        Self {
            entries: value.entries.into_iter().map(DirectoryEntryToolResult::from).collect(),
            rendered: value.rendered,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct TreeNodeToolResult {
    name: String,
    #[serde(rename = "type")]
    kind: EntryKindToolResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    children: Option<Vec<Self>>,
}

impl From<TreeNode> for TreeNodeToolResult {
    fn from(value: TreeNode) -> Self {
        Self {
            name: value.name,
            kind: value.kind.into(),
            children: value
                .children
                .map(|children| children.into_iter().map(TreeNodeToolResult::from).collect()),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct MoveToolResult {
    source: String,
    destination: String,
    warnings: Vec<String>,
}

impl From<PipelineResult<MoveOutcome>> for MoveToolResult {
    fn from(result: PipelineResult<MoveOutcome>) -> Self {
        let WarningMessages(warnings) = WarningMessages::from(result.warnings);
        Self {
            source: result.value.source.relative().to_string_lossy().into_owned(),
            destination: result.value.destination.relative().to_string_lossy().into_owned(),
            warnings,
        }
    }
}

/// One resolved `fs_delete_file` target's outcome: present in
/// every response's `results`, isolated so one target's failure never
/// fails the whole call (the same partial-success pattern
/// `fs_read_multiple_files` uses).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DeleteItemToolResult {
    pub(crate) path: String,
    pub(crate) deleted: bool,
    pub(crate) trashed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<BatchErrorToolResult>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DeleteToolResult {
    /// Mirrors `results[0]` when exactly one target was resolved (a
    /// single `path`, or `paths`/`pattern` matching exactly one target),
    /// preserved for callers using the original single-target
    /// contract; absent for a batch of zero or several targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trashed: Option<bool>,
    results: Vec<DeleteItemToolResult>,
    warnings: Vec<String>,
}

/// The original path string paired with the error that stopped resolving,
/// planning, or executing an `fs_delete_file` target, following the same
/// isolate-per-item pattern. Boxed: `ToolError` is large enough that an
/// unboxed `(String, ToolError)` `Err` variant trips
/// `clippy::result_large_err` on every `Result` that carries it.
pub(crate) type DeleteFailure = Box<(String, ToolError)>;

/// One resolved target's attempted deletion: either the pipeline outcome,
/// or the [`DeleteFailure`] that stopped it, reported as a failed
/// `results[]` entry rather than failing the whole call.
pub(crate) type DeleteAttempt = Result<PipelineResult<DeleteOutcome>, DeleteFailure>;

impl From<Vec<DeleteAttempt>> for DeleteToolResult {
    fn from(attempts: Vec<DeleteAttempt>) -> Self {
        let mut warnings = Vec::new();
        let mut results = Vec::with_capacity(attempts.len());
        for attempt in attempts {
            match attempt {
                Ok(result) => {
                    let WarningMessages(item_warnings) = WarningMessages::from(result.warnings);
                    warnings.extend(item_warnings);
                    results.push(DeleteItemToolResult {
                        path: result.value.path.relative().to_string_lossy().into_owned(),
                        deleted: result.value.deleted,
                        trashed: result.value.trashed,
                        error: None,
                    });
                }
                Err(failure) => {
                    let (path, error) = *failure;
                    let failure = crate::tool_error::ToolFailure::from(error);
                    results.push(DeleteItemToolResult {
                        path,
                        deleted: false,
                        trashed: false,
                        error: Some(BatchErrorToolResult::from(failure)),
                    });
                }
            }
        }
        let (path, deleted, trashed) = match results.as_slice() {
            [only] => (Some(only.path.clone()), Some(only.deleted), Some(only.trashed)),
            _ => (None, None, None),
        };
        Self {
            path,
            deleted,
            trashed,
            results,
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct SearchToolResult {
    paths: Vec<String>,
}

impl From<Vec<String>> for SearchToolResult {
    fn from(value: Vec<String>) -> Self {
        Self { paths: value }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FileInfoToolResult {
    path: String,
    kind: EntryKindToolResult,
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    accessed: Option<String>,
    readonly: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
}

impl From<FileInfo> for FileInfoToolResult {
    fn from(value: FileInfo) -> Self {
        let content_hash = value.content_hash.map(|hash| {
            let text: &str = (&hash).into();
            text.to_owned()
        });
        Self {
            path: value.path,
            kind: value.kind.into(),
            size: value.size,
            created: value.created.map(|timestamp| timestamp.to_string()),
            modified: value.modified.map(|timestamp| timestamp.to_string()),
            accessed: value.accessed.map(|timestamp| timestamp.to_string()),
            readonly: value.readonly,
            content_hash,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct AllowedDirectoryToolResult {
    /// This vault's configured or default-derived name, used to
    /// address it as `{name}://{relative-path}`.
    name: String,
    path: String,
    managed: bool,
}

impl From<AllowedDirectory> for AllowedDirectoryToolResult {
    fn from(value: AllowedDirectory) -> Self {
        Self {
            name: value.name,
            path: value.path,
            managed: value.managed,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct AllowedDirectoriesToolResult {
    directories: Vec<AllowedDirectoryToolResult>,
}

impl From<Vec<AllowedDirectory>> for AllowedDirectoriesToolResult {
    fn from(value: Vec<AllowedDirectory>) -> Self {
        Self {
            directories: value.into_iter().map(AllowedDirectoryToolResult::from).collect(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathsInput {
    /// Each entry is vault-relative or absolute, or `{name}://{relative-path}`
    /// to address a specific configured vault by name.
    pub(crate) paths: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadTextInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    // See the matching comment on `QueryIndexRebuildInput.budget_seconds`
    // (`tools::query`): `#[serde(default)]` is required alongside
    // `schema_with` so the field is still correctly marked optional in the
    // generated schema.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    head: Option<u64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    tail: Option<u64>,
    range: Option<RangeInput>,
}

pub(crate) struct ToolReadLimit(pub(crate) Option<ReadLimit>);

impl TryFrom<&ReadTextInput> for ToolReadLimit {
    type Error = ToolError;

    fn try_from(value: &ReadTextInput) -> Result<Self, Self::Error> {
        let count =
            usize::from(value.head.is_some()) + usize::from(value.tail.is_some()) + usize::from(value.range.is_some());
        if count > 1 {
            return Err(ToolError::Invalid("head, tail, and range are mutually exclusive"));
        }
        if let Some(head) = value.head {
            return Ok(Self(Some(ReadLimit::Head(usize::try_from(head)?))));
        }
        if let Some(tail) = value.tail {
            return Ok(Self(Some(ReadLimit::Tail(usize::try_from(tail)?))));
        }
        value
            .range
            .as_ref()
            .map(|range| {
                LineRange::try_from((usize::try_from(range.from_line)?, usize::try_from(range.to_line)?))
                    .map(ReadLimit::Range)
                    .map_err(ToolError::from)
            })
            .transpose()
            .map(Self)
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RangeInput {
    from_line: u64,
    to_line: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) expected_hash: Option<String>,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    pub(crate) edits: Vec<TextEditInput>,
    #[serde(default)]
    pub(crate) dry_run: bool,
    pub(crate) expected_hash: Option<String>,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextEditInput {
    old_text: String,
    new_text: String,
}

impl From<TextEditInput> for TextEdit {
    fn from(value: TextEditInput) -> Self {
        Self {
            old_text: value.old_text,
            new_text: value.new_text,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListDirectoryToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    /// Include each entry's size and last-modified time, and accept
    /// `sort_by`. Default `false` returns the same lightweight listing this
    /// tool always returned before it absorbed `fs_list_directory_with_sizes`.
    #[serde(default)]
    pub(crate) with_sizes: bool,
    /// Sort order, only meaningful (and only accepted) when `with_sizes` is
    /// `true`; there is nothing to sort by otherwise.
    #[serde(default)]
    pub(crate) sort_by: Option<SortByInput>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SortByInput {
    #[default]
    Name,
    Size,
}

impl From<SortByInput> for SortBy {
    fn from(value: SortByInput) -> Self {
        match value {
            SortByInput::Name => Self::Name,
            SortByInput::Size => Self::Size,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) exclude_patterns: Vec<String>,
    #[serde(default = "default_depth")]
    pub(crate) max_depth: u64,
}
const fn default_depth() -> u64 {
    10
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveInput {
    /// Vault-relative or absolute source path, or `{name}://{relative-path}`
    /// to address a specific configured vault by name.
    pub(crate) source: String,
    /// Vault-relative or absolute destination path, or
    /// `{name}://{relative-path}`; the destination must not
    /// already exist.
    pub(crate) destination: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteInput {
    /// A single target (vault-relative or absolute, or
    /// `{name}://{relative-path}` to address a specific configured vault
    /// by name; use `{name}://.` for that vault's root), or the
    /// glob base directory when `pattern` is set. Exactly one
    /// of: `path` alone; `paths`; or `path` with `pattern`.
    #[serde(default)]
    pub(crate) path: Option<String>,
    /// Multiple explicit targets, each addressed exactly as `path` above,
    /// isolated per item so one target's failure does not fail the whole
    /// call; mutually exclusive with `path` used alone and with `pattern`.
    #[serde(default)]
    pub(crate) paths: Vec<String>,
    /// Case-insensitive glob, matched the same way as
    /// `fs_search_files`'s `pattern`, against every path under `path`
    /// (required as the glob's base directory when this is set); mutually
    /// exclusive with `paths`.
    #[serde(default)]
    pub(crate) pattern: Option<String>,
    #[serde(default)]
    pub(crate) hard: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchInput {
    /// Directory to search from: vault-relative or absolute, or
    /// `{name}://{relative-path}` to address a specific configured vault
    /// by name; use `{name}://.` to search a whole named vault
    /// from its root (a bare `{name}://` with no path after it is
    /// rejected).
    pub(crate) path: String,
    /// Case-insensitive glob matched against each entry's whole path
    /// relative to `path`, not just its filename: a bare pattern with no
    /// `/` (e.g. `*.md`) only matches entries directly inside `path`
    /// itself; prefix it with `**/` (e.g. `**/*.md`, `**/anti-patterns*`)
    /// to match at any depth.
    pub(crate) pattern: String,
    #[serde(default)]
    /// Case-insensitive glob patterns to exclude from the results.
    pub(crate) exclude_patterns: Vec<String>,
    #[serde(default = "default_results")]
    pub(crate) max_results: u64,
}
const fn default_results() -> u64 {
    200
}
