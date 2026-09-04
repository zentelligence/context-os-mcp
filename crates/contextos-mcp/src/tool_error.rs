//! Shared tool-call error taxonomy and dispatch helpers. Every tool-router
//! module (`tools/*.rs`) maps its domain errors into [`ToolError`] and
//! propagates them through [`execute`]/[`evaluate`]/[`execute_value`], so
//! there is exactly one place translating internal error types into the
//! stable, machine-readable MCP error codes and remediation text a caller
//! sees (`ToolFailure`).

use contextos_core::PathError;
use contextos_ephemeris::EphemerisError;
use contextos_fs::FsError;
use contextos_git::{GitError, GitWriteError};
use contextos_index::IndexServiceError;
use contextos_obsidian::{
    BaseError, BaseQueryError, CanvasError, FrontmatterError, MarkdownError, NoteError,
};
use contextos_oplog::OperationLogError;
use contextos_search::SearchError;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::Json;
use rmcp::{ErrorData, schemars};
use serde::Serialize;
use thiserror::Error;

use crate::resource_support::ResourceError;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ToolFailure {
    pub(crate) code: String,
    pub(crate) message: String,
    // `#[serde(default)]` is required alongside `schema_with` here: see
    // `IndexesStatusToolResult::state_directory`'s identical comment
    // (`tools/index_status.rs`) for why, and `optional_u64_schema`'s own
    // existing callers (`tools/query.rs`, `tools/fs_types.rs`) for the
    // established precedent. Currently masked in practice by
    // `fallible_output_schema_for` stripping `required` entirely from the
    // merged schema it builds, but kept correct here regardless: nothing
    // guarantees `path` is only ever reached through that merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_path_schema")]
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) remediation: String,
}

impl IntoCallToolResult for ToolFailure {
    fn into_call_tool_result(self) -> Result<rmcp::model::CallToolResult, ErrorData> {
        Json(self).into_call_tool_result()
    }
}

#[derive(Debug, Error)]
pub(crate) enum ToolError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Hash(#[from] contextos_core::ContentHashError),
    #[error("numeric parameter is too large for this platform")]
    Number(#[from] std::num::TryFromIntError),
    #[error("blocking filesystem task failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("invalid tool arguments: {0}")]
    Invalid(&'static str),
    #[error("batch result assembly did not preserve input cardinality")]
    BatchAssembly,
    #[error("configured root {root_index} has no write coordinator")]
    RootLockMissing { root_index: usize },
    #[error("hard delete is disabled for this vault")]
    DestructiveDeleteDisabled,
    #[error("managed indexes are disabled for this vault")]
    IndexDisabled,
    #[error(transparent)]
    Index(#[from] IndexServiceError<FsError, FsError>),
    #[error("operation logging is disabled for this vault")]
    OperationLogDisabled,
    #[error("manual operation-log append emitted no persistence event")]
    ManualLogEventMissing,
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error(transparent)]
    Note(#[from] NoteError),
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
    #[error(transparent)]
    Base(#[from] BaseError),
    #[error(transparent)]
    BaseQuery(#[from] BaseQueryError),
    #[error(transparent)]
    Canvas(#[from] CanvasError),
    #[error("Git is disabled for this vault")]
    GitDisabled,
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GitWrite(#[from] GitWriteError<FsError>),
    #[error("search is disabled for this vault")]
    SearchDisabled,
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error("no fenced ```mermaid code block was found in the note")]
    MermaidFenceMissing,
    /// Defensive only: `MermanParser::render` always produces UTF-8 bytes
    /// (`String::into_bytes`), so this is unreachable with the current
    /// `RendersMermaid` implementation. It guards the trait's wider
    /// `Vec<u8>` contract against a future implementation (e.g. the
    /// documented raster-output follow-up) that might not uphold that
    /// invariant.
    #[error("rendered Mermaid SVG was not valid UTF-8")]
    MermaidRenderNotUtf8,
    /// Defensive only: serialising `StructuredValidationToolResult` (plain
    /// `String`/`bool`/`Vec` fields) cannot fail; this guards against a
    /// future field type that could.
    #[error("failed to serialise Mermaid diagnostics: {0}")]
    MermaidDiagnosticSerialisation(serde_json::Error),
    /// Defensive only: a `VaultPath`'s `root_id` always refers to a root in
    /// the same `VaultSet` it was resolved against (`VaultPath::try_new`),
    /// so `fs_attach_file`'s `{name}://{relative-path}` URI construction
    /// should never actually fail to find that root and hit this.
    #[error("attachment path could not be represented as a resource URI")]
    AttachmentUriInvalid,
    /// Defensive only: `ReadTextToolResult`/`ReadManyToolResult`/
    /// `TreeNodeToolResult` are plain `String`/`usize`/`bool`/`Vec`/`Self`
    /// fields, which cannot fail to serialise; this guards against a
    /// future field type that could.
    #[error("failed to serialise the tool result: {0}")]
    CallToolResultSerialisation(serde_json::Error),
    #[error(transparent)]
    Resource(#[from] ResourceError),
    /// `doctor`/`doctor_resolve` could not assess the
    /// effective configuration at all (for example, a vault root that
    /// disappeared between server start and the call). Distinct from a
    /// `DoctorCheck` reporting `Fail`, which is a successful assessment
    /// that found a problem, not a tool-level error.
    #[error(transparent)]
    Doctor(#[from] crate::doctor::DoctorError),
    #[error(transparent)]
    Ephemeris(#[from] EphemerisError),
    /// Defensive only: every instant the ephemeris tools
    /// build comes from a real calendar date well within RFC
    /// 3339's representable range, so this should never actually fail.
    #[error("failed to format an ephemeris instant as RFC 3339: {0}")]
    EphemerisInstantFormatting(time::error::Format),
}

impl From<ToolError> for ToolFailure {
    #[expect(
        clippy::too_many_lines,
        reason = "one explicit code/remediation match keeps the complete MCP error taxonomy auditable in one place"
    )]
    fn from(value: ToolError) -> Self {
        let (code, path, remediation) = match &value {
            ToolError::Path(error) => (
                error.code(),
                error.path().map(std::path::Path::to_path_buf),
                error.remediation(),
            ),
            ToolError::Filesystem(error) => (
                error.code(),
                error.path().map(std::path::Path::to_path_buf),
                error.remediation(),
            ),
            ToolError::Hash(_) => (
                "io/invalid-hash",
                None,
                "Pass a complete 64-character hexadecimal SHA-256 hash.",
            ),
            ToolError::Number(_) | ToolError::Invalid(_) => (
                "io/invalid-argument",
                None,
                "Correct the tool arguments using the advertised input schema.",
            ),
            ToolError::DestructiveDeleteDisabled => (
                "io/destructive-delete-disabled",
                None,
                "Enable vault.git.destructive_delete or omit hard to use platform trash.",
            ),
            ToolError::IndexDisabled => (
                "index/disabled",
                None,
                "Enable index_md for this managed vault before rebuilding indexes.",
            ),
            ToolError::Index(error) => (
                error.code(),
                None,
                "Resolve the reported index conflict, and retry the subtree rebuild.",
            ),
            ToolError::OperationLogDisabled => (
                "log/disabled",
                None,
                "Enable oplog for this managed vault before appending entries.",
            ),
            ToolError::OperationLog(error) => (
                error.code(),
                None,
                "Correct the log entry or vault log configuration, and retry.",
            ),
            ToolError::ManualLogEventMissing => (
                "log/append",
                None,
                "Retry the append and inspect server diagnostics if it fails again.",
            ),
            ToolError::Note(error) => (
                error.code(),
                None,
                "Correct the note title or Obsidian Markdown syntax, and retry.",
            ),
            ToolError::Frontmatter(error) => (error.code(), None, error.remediation()),
            ToolError::Markdown(error) => (error.code(), None, error.remediation()),
            ToolError::Base(error) => (error.code(), None, error.remediation()),
            ToolError::BaseQuery(error) => (error.code(), None, error.remediation()),
            ToolError::Canvas(error) => (error.code(), None, error.remediation()),
            ToolError::GitDisabled => (
                "git/disabled",
                None,
                "Enable Git for this managed vault, and retry.",
            ),
            ToolError::Git(error) => (
                error.code(),
                None,
                "Initialise the repository or correct the Git request, and retry.",
            ),
            ToolError::GitWrite(error) => (
                error.code(),
                None,
                "Resolve the Git or vault write error, and retry.",
            ),
            ToolError::SearchDisabled => (
                "index/disabled",
                None,
                "Enable [vault.search] text or graph for this managed vault, and retry.",
            ),
            ToolError::Search(error) => (error.code(), None, error.remediation()),
            ToolError::MermaidFenceMissing => (
                "format/mermaid-schema",
                None,
                "Add a fenced ```mermaid code block to the note, or pass 'source' directly.",
            ),
            ToolError::Resource(error) => {
                let (code, remediation) = error.code_and_remediation();
                (code, None, remediation)
            }
            ToolError::Doctor(_) => (
                "doctor/unavailable",
                None,
                "Correct the effective configuration and retry.",
            ),
            ToolError::Ephemeris(error) => (error.code(), None, error.remediation()),
            ToolError::Join(_)
            | ToolError::BatchAssembly
            | ToolError::RootLockMissing { .. }
            | ToolError::MermaidRenderNotUtf8
            | ToolError::MermaidDiagnosticSerialisation(_)
            | ToolError::AttachmentUriInvalid
            | ToolError::CallToolResultSerialisation(_)
            | ToolError::EphemerisInstantFormatting(_) => (
                "server/internal",
                None,
                "Retry the operation and inspect server diagnostics if it fails again.",
            ),
        };
        Self {
            code: code.to_owned(),
            message: value.to_string(),
            path,
            remediation: remediation.to_owned(),
        }
    }
}

pub(crate) async fn execute<T, F>(operation: F) -> Result<Json<T>, ToolFailure>
where
    T: Serialize + schemars::JsonSchema + Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    Ok(Json(evaluate(operation).await?))
}

pub(crate) async fn evaluate<T, F>(operation: F) -> Result<T, ToolFailure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(operation)
        .await
        .map_err(ToolError::from)?;
    result.map_err(ToolFailure::from)
}

pub(crate) async fn execute_value<T, F>(operation: F) -> Result<Json<T>, ToolFailure>
where
    T: Serialize + schemars::JsonSchema + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let value = tokio::task::spawn_blocking(operation)
        .await
        .map_err(ToolError::from)?;
    Ok(Json(value))
}
