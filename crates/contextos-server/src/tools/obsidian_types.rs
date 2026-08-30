//! DTOs, request/result shapes, and structured-path helpers for the
//! Obsidian domain tools in [`crate::tools::obsidian`]. Split out purely
//! to keep `obsidian.rs` under the project's file-size limit; every item
//! here is `pub(crate)` for that sibling module to use.

use contextos_core::{
    ContentHash, PipelineResult, VaultPath, VaultPathInput, VaultSet, WriteOutcome,
};
use contextos_obsidian::{
    BaseDiagnostic, BaseDocument, BaseError, BaseOperation, CanvasDiagnostic, CanvasDocument,
    CanvasError, CanvasOperation, FrontmatterDocument, LinkCollection, ObsidianLink,
};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::server::WarningMessages;
use crate::tool_error::ToolError;
use crate::tools::diagnostics::StructuredDiagnosticToolResult;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NoteValidationToolResult {
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NoteCreateToolResult {
    path: String,
    content_hash: String,
    validation: NoteValidationToolResult,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for NoteCreateToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.relative().to_string_lossy().into_owned(),
            content_hash: hash.to_owned(),
            validation: NoteValidationToolResult {
                warnings: Vec::new(),
            },
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FrontmatterReadToolResult {
    frontmatter: serde_json::Map<String, serde_json::Value>,
    body_start_line: usize,
    content_hash: String,
}

impl From<(FrontmatterDocument, ContentHash)> for FrontmatterReadToolResult {
    fn from(value: (FrontmatterDocument, ContentHash)) -> Self {
        let hash: &str = (&value.1).into();
        Self {
            frontmatter: value.0.frontmatter().clone(),
            body_start_line: value.0.body_start_line(),
            content_hash: hash.to_owned(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FrontmatterUpdateToolResult {
    path: String,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<BaseDiagnostic> for StructuredDiagnosticToolResult {
    fn from(value: BaseDiagnostic) -> Self {
        Self {
            code: value.code,
            path: value.path,
            message: value.message,
        }
    }
}

impl From<CanvasDiagnostic> for StructuredDiagnosticToolResult {
    fn from(value: CanvasDiagnostic) -> Self {
        Self {
            code: value.code,
            path: value.path,
            message: value.message,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StructuredWriteToolResult {
    path: String,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for StructuredWriteToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.relative().to_string_lossy().into_owned(),
            content_hash: hash.to_owned(),
            warnings,
        }
    }
}

pub(crate) struct BaseReadSource {
    /// `Err` when the file fails to parse at all (e.g. malformed YAML); a
    /// document that parses but fails Bases schema rules is always `Ok`,
    /// with those violations surfacing through
    /// [`BaseDocument::diagnostics`] instead. Reporting the parse-failure
    /// case here as a diagnostic entry rather than propagating it as a
    /// tool error is what let `base_validate` retire: `base_read` is now a
    /// strict superset of what it reported.
    pub(crate) document: Result<BaseDocument, BaseError>,
    pub(crate) content_hash: ContentHash,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseReadToolResult {
    definition: serde_json::Map<String, serde_json::Value>,
    content_hash: String,
    diagnostics: Vec<StructuredDiagnosticToolResult>,
}

impl From<BaseReadSource> for BaseReadToolResult {
    fn from(value: BaseReadSource) -> Self {
        let hash: &str = (&value.content_hash).into();
        let (definition, diagnostics) = match value.document {
            Ok(document) => (
                document.definition().clone(),
                document
                    .diagnostics()
                    .into_iter()
                    .map(StructuredDiagnosticToolResult::from)
                    .collect(),
            ),
            Err(error) => (
                serde_json::Map::new(),
                vec![StructuredDiagnosticToolResult {
                    code: error.code().to_owned(),
                    path: "$".to_owned(),
                    message: error.to_string(),
                }],
            ),
        };
        Self {
            definition,
            content_hash: hash.to_owned(),
            diagnostics,
        }
    }
}

pub(crate) struct CanvasReadSource {
    /// See [`BaseReadSource::document`]; the same reasoning applies to
    /// `canvas_validate`'s retirement.
    pub(crate) document: Result<CanvasDocument, CanvasError>,
    pub(crate) content_hash: ContentHash,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct CanvasReadToolResult {
    nodes: Vec<serde_json::Map<String, serde_json::Value>>,
    edges: Vec<serde_json::Map<String, serde_json::Value>>,
    content_hash: String,
    diagnostics: Vec<StructuredDiagnosticToolResult>,
}

impl From<CanvasReadSource> for CanvasReadToolResult {
    fn from(value: CanvasReadSource) -> Self {
        let hash: &str = (&value.content_hash).into();
        let as_object_vec = |values: &[serde_json::Value]| {
            values
                .iter()
                .map(|value| value.as_object().cloned().unwrap_or_default())
                .collect()
        };
        let (nodes, edges, diagnostics) = match value.document {
            Ok(document) => (
                as_object_vec(document.nodes()),
                as_object_vec(document.edges()),
                document
                    .diagnostics()
                    .into_iter()
                    .map(StructuredDiagnosticToolResult::from)
                    .collect(),
            ),
            Err(error) => (
                Vec::new(),
                Vec::new(),
                vec![StructuredDiagnosticToolResult {
                    code: error.code().to_owned(),
                    path: "$".to_owned(),
                    message: error.to_string(),
                }],
            ),
        };
        Self {
            nodes,
            edges,
            content_hash: hash.to_owned(),
            diagnostics,
        }
    }
}

impl From<PipelineResult<WriteOutcome>> for FrontmatterUpdateToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.relative().to_string_lossy().into_owned(),
            content_hash: hash.to_owned(),
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ObsidianLinkToolResult {
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    block: Option<String>,
    embed: bool,
}

impl From<ObsidianLink> for ObsidianLinkToolResult {
    fn from(value: ObsidianLink) -> Self {
        Self {
            target: value.target,
            display: value.display,
            heading: value.heading,
            block: value.block,
            embed: value.embed,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct LinksReadToolResult {
    outgoing: Vec<ObsidianLinkToolResult>,
    unresolved: Vec<String>,
}

impl From<(LinkCollection, Vec<String>)> for LinksReadToolResult {
    fn from(value: (LinkCollection, Vec<String>)) -> Self {
        Self {
            outgoing: value
                .0
                .outgoing()
                .iter()
                .cloned()
                .map(ObsidianLinkToolResult::from)
                .collect(),
            unresolved: value.1,
        }
    }
}

pub(crate) struct StructuredPathInput<'a> {
    pub(crate) roots: &'a VaultSet,
    pub(crate) raw: &'a str,
}

pub(crate) struct BaseVaultPath(pub(crate) VaultPath);

impl TryFrom<StructuredPathInput<'_>> for BaseVaultPath {
    type Error = ToolError;

    fn try_from(value: StructuredPathInput<'_>) -> Result<Self, Self::Error> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: value.roots,
            raw: value.raw,
        })?;
        if path
            .relative()
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            != Some("base")
        {
            return Err(ToolError::Invalid("Base path must end in .base"));
        }
        Ok(Self(path))
    }
}

pub(crate) struct CanvasVaultPath(pub(crate) VaultPath);

impl TryFrom<StructuredPathInput<'_>> for CanvasVaultPath {
    type Error = ToolError;

    fn try_from(value: StructuredPathInput<'_>) -> Result<Self, Self::Error> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: value.roots,
            raw: value.raw,
        })?;
        if path
            .relative()
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            != Some("canvas")
        {
            return Err(ToolError::Invalid("Canvas path must end in .canvas"));
        }
        Ok(Self(path))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteReferenceInput {
    pub(crate) target: String,
    pub(crate) summary: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) frontmatter: serde_json::Map<String, serde_json::Value>,
    pub(crate) content: String,
    #[serde(default)]
    pub(crate) references: Vec<NoteReferenceInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontmatterUpdateInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    pub(crate) patch: serde_json::Map<String, serde_json::Value>,
    pub(crate) expected_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    pub(crate) definition: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseApplyToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[schemars(schema_with = "base_operations_schema")]
    pub(crate) operations: Vec<BaseOperationToolInput>,
    pub(crate) expected_hash: Option<String>,
}

/// Renders as an empty object schema (`{}`) rather than schemars' default bare
/// `true`, since some MCP clients reject boolean sub-schemas in tool schemas.
/// Both forms mean "any JSON value is valid" per the JSON Schema spec.
fn any_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::from(serde_json::Map::new())
}

/// Converts a hand-written `serde_json::json!` object literal into a
/// `schemars::Schema`: `Schema` only implements `From` for
/// `serde_json::Map`, not the `Value` the macro produces, and every
/// caller here always passes an object literal, never any other JSON
/// value shape. Falls back to the empty (any-value) schema in the
/// unreachable non-object case rather than panicking.
fn object_schema(value: serde_json::Value) -> schemars::Schema {
    match value {
        serde_json::Value::Object(map) => schemars::Schema::from(map),
        _ => schemars::Schema::from(serde_json::Map::new()),
    }
}

/// Replaces `BaseOperationToolInput`'s default derived schema, a `oneOf`
/// of eight exact per-variant shapes, with one flat object schema: an
/// `op` discriminator plus every variant's fields declared optional.
/// Confirmed live (0.7.1): a root-level `oneOf` alone, with no null
/// branch involved, broke Cowork's whole per-task tool registry; every
/// other tool in this catalogue's advertised schema is already free of
/// `oneOf`/`anyOf`/`allOf` (`sanitise_nullable_unions`, `server.rs`), and
/// this was the one deliberate holdout because it is a genuine
/// discriminated choice, not a nullability shape that transform can
/// collapse. `BaseOperationToolInput` itself, and its `deny_unknown_fields`
/// serde tag matching, are unchanged: a caller supplying a field that does
/// not belong to the given `op`, or omitting one that operation actually
/// requires, is still rejected at deserialization; only the advertised
/// schema's own precision is traded for a shape every MCP client is
/// confirmed to tolerate.
fn base_operations_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    object_schema(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": [
                        "set_filters", "add_formula", "remove_formula", "set_property",
                        "remove_property", "add_view", "remove_view", "update_view",
                        "set_summary", "remove_summary"
                    ],
                    "description": "Which Base operation this entry performs; determines \
                        which of the other fields apply and are required."
                },
                "filters": {
                    "description": "set_filters only: the Base's replacement filter \
                        definition, any JSON value."
                },
                "name": {
                    "type": "string",
                    "description": "add_formula, remove_formula, set_property, \
                        remove_property, add_view, remove_view, update_view, set_summary, \
                        remove_summary: the target formula, property, or view's name."
                },
                "expression": {
                    "type": "string",
                    "description": "add_formula, set_summary only: the formula expression."
                },
                "property": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "set_property only: the property definition."
                },
                "view": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_view only: the new view's definition."
                },
                "patch": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "update_view only: fields to merge into the existing view."
                }
            },
            "required": ["op"],
            "additionalProperties": false
        }
    }))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BaseOperationToolInput {
    SetFilters {
        #[schemars(schema_with = "any_json_schema")]
        filters: serde_json::Value,
    },
    AddFormula {
        name: String,
        expression: String,
    },
    RemoveFormula {
        name: String,
    },
    SetProperty {
        name: String,
        property: serde_json::Map<String, serde_json::Value>,
    },
    RemoveProperty {
        name: String,
    },
    AddView {
        name: String,
        view: serde_json::Map<String, serde_json::Value>,
    },
    RemoveView {
        name: String,
    },
    UpdateView {
        name: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    SetSummary {
        name: String,
        expression: String,
    },
    RemoveSummary {
        name: String,
    },
}

impl From<BaseOperationToolInput> for BaseOperation {
    fn from(value: BaseOperationToolInput) -> Self {
        match value {
            BaseOperationToolInput::SetFilters { filters } => Self::SetFilters { filters },
            BaseOperationToolInput::AddFormula { name, expression } => {
                Self::AddFormula { name, expression }
            }
            BaseOperationToolInput::RemoveFormula { name } => Self::RemoveFormula { name },
            BaseOperationToolInput::SetProperty { name, property } => Self::SetProperty {
                name,
                definition: property,
            },
            BaseOperationToolInput::RemoveProperty { name } => Self::RemoveProperty { name },
            BaseOperationToolInput::AddView { name, view } => Self::AddView {
                name,
                definition: view,
            },
            BaseOperationToolInput::RemoveView { name } => Self::RemoveView { name },
            BaseOperationToolInput::UpdateView { name, patch } => Self::UpdateView { name, patch },
            BaseOperationToolInput::SetSummary { name, expression } => {
                Self::SetSummary { name, expression }
            }
            BaseOperationToolInput::RemoveSummary { name } => Self::RemoveSummary { name },
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanvasCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) nodes: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub(crate) edges: Vec<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanvasApplyToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[schemars(schema_with = "canvas_operations_schema")]
    pub(crate) operations: Vec<CanvasOperationToolInput>,
    pub(crate) expected_hash: Option<String>,
}

/// [`base_operations_schema`]'s counterpart for `CanvasOperationToolInput`:
/// same rationale, same `oneOf`-avoidance, same unchanged runtime
/// validation via `CanvasOperationToolInput`'s own `deny_unknown_fields`
/// serde tag matching.
fn canvas_operations_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    object_schema(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": [
                        "add_node", "update_node", "remove_node",
                        "add_edge", "update_edge", "remove_edge", "group"
                    ],
                    "description": "Which Canvas operation this entry performs; determines \
                        which of the other fields apply and are required."
                },
                "node": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_node only: the new node's definition."
                },
                "edge": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_edge only: the new edge's definition."
                },
                "id": {
                    "type": "string",
                    "description": "update_node, remove_node, update_edge, remove_edge: the \
                        target node or edge's id."
                },
                "patch": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "update_node, update_edge: fields to merge into the \
                        existing node or edge."
                },
                "group": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "group only: the new group node's definition."
                },
                "members": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "group only: ids of the nodes this group contains."
                }
            },
            "required": ["op"],
            "additionalProperties": false
        }
    }))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CanvasOperationToolInput {
    AddNode {
        node: serde_json::Map<String, serde_json::Value>,
    },
    UpdateNode {
        id: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    RemoveNode {
        id: String,
    },
    AddEdge {
        edge: serde_json::Map<String, serde_json::Value>,
    },
    UpdateEdge {
        id: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    RemoveEdge {
        id: String,
    },
    Group {
        group: serde_json::Map<String, serde_json::Value>,
        members: Vec<String>,
    },
}

impl From<CanvasOperationToolInput> for CanvasOperation {
    fn from(value: CanvasOperationToolInput) -> Self {
        match value {
            CanvasOperationToolInput::AddNode { node } => Self::AddNode { node },
            CanvasOperationToolInput::UpdateNode { id, patch } => Self::UpdateNode { id, patch },
            CanvasOperationToolInput::RemoveNode { id } => Self::RemoveNode { id },
            CanvasOperationToolInput::AddEdge { edge } => Self::AddEdge { edge },
            CanvasOperationToolInput::UpdateEdge { id, patch } => Self::UpdateEdge { id, patch },
            CanvasOperationToolInput::RemoveEdge { id } => Self::RemoveEdge { id },
            CanvasOperationToolInput::Group { group, members } => Self::Group { group, members },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LinkDirectionInput {
    #[default]
    Out,
    In,
    Both,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinksReadInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) direction: LinkDirectionInput,
}

/// `base_query` accepts exactly one of `path` (an existing `.base` file) or
/// `definition` (an inline ad hoc definition); `vault` selects which
/// configured vault to scan and is only consulted for `definition`, since
/// `path` already unambiguously names a vault.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseQueryToolInput {
    /// Existing `.base` file to query: vault-relative or absolute, or
    /// `{name}://{relative-path}`. Exactly one of `path` or
    /// `definition` is required.
    pub(crate) path: Option<String>,
    pub(crate) definition: Option<serde_json::Map<String, serde_json::Value>>,
    pub(crate) view: Option<String>,
    /// The vault to scan when using `definition` (ignored for `path`,
    /// which already names a vault): `{name}://.` to select a specific
    /// configured vault by name (a bare vault name alone is not
    /// accepted for this parameter); omit to use the sole configured
    /// vault.
    pub(crate) vault: Option<String>,
    #[serde(default)]
    pub(crate) format: BaseQueryFormatInput,
    pub(crate) limit: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BaseQueryFormatInput {
    #[default]
    Table,
    Json,
    Csv,
}

impl From<BaseQueryFormatInput> for contextos_obsidian::QueryFormat {
    fn from(value: BaseQueryFormatInput) -> Self {
        match value {
            BaseQueryFormatInput::Table => Self::Table,
            BaseQueryFormatInput::Json => Self::Json,
            BaseQueryFormatInput::Csv => Self::Csv,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseQueryToolResult {
    pub(crate) content: String,
    pub(crate) columns: Vec<String>,
    pub(crate) matched: usize,
    pub(crate) truncated: bool,
    pub(crate) diagnostics: Vec<StructuredDiagnosticToolResult>,
}
