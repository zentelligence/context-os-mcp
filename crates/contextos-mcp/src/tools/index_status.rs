//! Per-index status reporting, shared by `vault_info`'s
//! `indexes` block (`tools::vault`) and `query_index_status`'s full
//! result (`tools::query`): one conversion from `contextos_search`'s
//! status types, reused by both tools rather than duplicated.

use contextos_search::{CatchUpKind, GraphIndexStatus, IndexStatusReport, SemanticIndexStatus, TextIndexStatus};
use rmcp::schemars;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
pub(crate) struct IndexesStatusToolResult {
    /// The vault's derived-state directory (omitted when search is disabled
    /// entirely for this vault), so an operator can confirm a separately
    /// invoked `contextos index` and this running server resolve to the
    /// same on-disk store rather than two silently diverged ones.
    // `#[serde(default)]` is required alongside `schema_with` here: the
    // schema-generation macro derives whether a field counts as "optional"
    // (and thus absent from the schema's `required` array) from the same
    // overridden schema type `schema_with` supplies, and a bare function
    // override's wrapper type is not itself recognised as an `Option`.
    // Without this, state_directory would be incorrectly marked required
    // despite genuinely being omitted whenever search is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    state_directory: Option<String>,
    text: TextIndexStatusToolResult,
    graph: GraphIndexStatusToolResult,
    semantic: SemanticIndexStatusToolResult,
}

impl From<IndexStatusReport> for IndexesStatusToolResult {
    fn from(value: IndexStatusReport) -> Self {
        Self {
            state_directory: Some(value.state_directory.display().to_string()),
            text: TextIndexStatusToolResult::from(value.text),
            graph: GraphIndexStatusToolResult::from(value.graph),
            semantic: SemanticIndexStatusToolResult::from(value.semantic),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct TextIndexStatusToolResult {
    enabled: bool,
    documents: usize,
    stale_estimate: usize,
    /// Omitted (never `null`) whenever this index has not yet built, most
    /// commonly because it is disabled: `#[serde(default)]` alongside
    /// `schema_with` is required for the same reason documented on
    /// `IndexesStatusToolResult::state_directory` above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
}

impl From<TextIndexStatus> for TextIndexStatusToolResult {
    fn from(value: TextIndexStatus) -> Self {
        Self {
            enabled: value.enabled,
            documents: value.documents,
            stale_estimate: value.stale_estimate,
            last_build: value.last_build.map(|when| when.to_string()),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct GraphIndexStatusToolResult {
    enabled: bool,
    nodes: usize,
    edges: usize,
    needs_rebuild: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
    /// This vault's link graph's current generation counter, omitted when
    /// the configured `graph_backend` does not track one (`fjall`, `serde`)
    /// or search is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_u64_schema")]
    generation: Option<u64>,
    /// Whether this instance's most recent cross-instance catch-up
    /// applied a partial delta (`"partial"`) or fell back to
    /// a full reload (`"full-reload"`), omitted for the same reasons as
    /// `generation`, or when this instance has not yet needed a catch-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_catch_up: Option<String>,
}

impl From<GraphIndexStatus> for GraphIndexStatusToolResult {
    fn from(value: GraphIndexStatus) -> Self {
        Self {
            enabled: value.enabled,
            nodes: value.nodes,
            edges: value.edges,
            needs_rebuild: value.needs_rebuild,
            last_build: value.last_build.map(|when| when.to_string()),
            generation: value.generation,
            last_catch_up: value.last_catch_up.map(|kind| {
                match kind {
                    CatchUpKind::Partial => "partial",
                    CatchUpKind::FullReload => "full-reload",
                }
                .to_owned()
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, schemars::JsonSchema)]
struct SemanticIndexStatusToolResult {
    enabled: bool,
    documents: usize,
    chunks: usize,
    stale_estimate: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    last_build: Option<String>,
}

impl From<SemanticIndexStatus> for SemanticIndexStatusToolResult {
    fn from(value: SemanticIndexStatus) -> Self {
        Self {
            enabled: value.enabled,
            documents: value.documents,
            chunks: value.chunks,
            stale_estimate: value.stale_estimate,
            last_build: value.last_build.map(|when| when.to_string()),
        }
    }
}
