//! `query_text`, `query_semantic`, `query_graph`, `query_index_status`,
//! and `query_index_rebuild`: the read-only search layer over a vault's
//! text, semantic, and link-graph indexes, plus the `search_service`
//! helper and streamed-progress plumbing shared by these tools.

use std::sync::Arc;

use contextos_core::{VaultPath, VaultPathInput};
use contextos_search::{
    GraphDirection, GraphEdge, GraphEdgeKind, GraphNode, GraphRebuildReport, GraphView, RebuildProgress, RebuildReport,
    RebuildTarget, SemanticHit, SemanticQuery, SemanticRebuildReport, TextHit, TextQuery, VaultSearchService,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, schemars, tool};
use serde::{Deserialize, Serialize};

use crate::resource_support::{fallible_output_schema_for, optional_u64_schema};
use crate::server::ContextOsServer;
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::index_status::IndexesStatusToolResult;

#[rmcp::tool_router(router = query_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "query_text",
        description = "Ranked full-text search across vault markdown with path, tag, and frontmatter filters; exclude_paths omits given path prefixes (e.g. a superseded version) from results",
        output_schema = fallible_output_schema_for::<QueryTextToolResult>()
    )]
    async fn query_text(
        &self,
        Parameters(input): Parameters<QueryTextInput>,
    ) -> Result<Json<QueryTextToolResult>, ToolFailure> {
        let (service, _index) = self.search_service(input.vault.as_deref()).await?;
        execute(move || {
            let limit = usize::try_from(input.limit)?;
            let request = TextQuery {
                query: &input.query,
                path_prefix: input.path_prefix.as_deref(),
                exclude_paths: &input.exclude_paths,
                tags: &input.tags,
                fields: &input.fields,
                limit,
            };
            let (hits, freshness) = service.query_text(&request)?;
            Ok(QueryTextToolResult {
                hits: hits.into_iter().map(TextHitToolResult::from).collect(),
                index_freshness: IndexFreshnessToolResult::from(freshness),
            })
        })
        .await
    }

    #[tool(
        name = "query_semantic",
        description = "Vector similarity search over chunked note content; requires [vault.search] semantic = true; exclude_paths omits given path prefixes (e.g. a superseded version) from results",
        output_schema = fallible_output_schema_for::<QuerySemanticToolResult>()
    )]
    async fn query_semantic(
        &self,
        Parameters(input): Parameters<QuerySemanticInput>,
    ) -> Result<Json<QuerySemanticToolResult>, ToolFailure> {
        let (service, _index) = self.search_service(input.vault.as_deref()).await?;
        execute(move || {
            let limit = usize::try_from(input.limit)?;
            let request = SemanticQuery {
                query: &input.query,
                limit,
                path_prefix: input.path_prefix.as_deref(),
                exclude_paths: &input.exclude_paths,
            };
            let hits = service.query_semantic(&request)?;
            Ok(QuerySemanticToolResult {
                hits: hits.into_iter().map(SemanticHitToolResult::from).collect(),
            })
        })
        .await
    }

    #[tool(
        name = "query_graph",
        description = "Traverse the wikilink graph: neighbours, backlinks, shortest path, or orphaned notes",
        output_schema = fallible_output_schema_for::<GraphViewToolResult>()
    )]
    async fn query_graph(
        &self,
        Parameters(input): Parameters<QueryGraphInput>,
    ) -> Result<Json<GraphViewToolResult>, ToolFailure> {
        let (service, _index) = self.search_service(input.vault.as_deref()).await?;
        execute(move || {
            let direction = GraphDirection::from(input.direction);
            let view = match input.operation {
                GraphOperationInput::Neighbours => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'neighbours' requires 'from'"))?;
                    service.graph_neighbours(&from, input.depth, direction)?
                }
                GraphOperationInput::Backlinks => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'backlinks' requires 'from'"))?;
                    service.graph_backlinks(&from)?
                }
                GraphOperationInput::Path => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'path' requires 'from'"))?;
                    let to = input.to.ok_or(ToolError::Invalid("query_graph 'path' requires 'to'"))?;
                    service.graph_path(&from, &to, direction)?
                }
                GraphOperationInput::Orphans => service.graph_orphans()?,
            };
            Ok(GraphViewToolResult::from(view))
        })
        .await
    }

    #[tool(
        name = "query_index_status",
        description = "Report per-index document counts, staleness estimate, and last build time",
        output_schema = fallible_output_schema_for::<IndexesStatusToolResult>()
    )]
    async fn query_index_status(
        &self,
        Parameters(input): Parameters<QueryVaultInput>,
    ) -> Result<Json<IndexesStatusToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let search = Arc::clone(&self.search);
        let raw = input.vault.unwrap_or_else(|| ".".to_owned());
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = search.get(index).cloned().flatten();
        execute(move || match service {
            Some(service) => Ok(IndexesStatusToolResult::from(service.status()?)),
            None => Ok(IndexesStatusToolResult::default()),
        })
        .await
    }

    /// Rebuilds the requested indexes. Text and graph are each a single
    /// fast pass and always run to completion; the semantic phase (local
    /// CPU embedding of a vault, which can run far longer than a typical
    /// request timeout) is budgeted (`budget_seconds`, defaulting to this
    /// vault's configured `[vault.search] rebuild_budget_seconds`) and
    /// returns early with partial progress once the budget elapses, leaving
    /// the rest queued. The
    /// result's `semantic.remaining` is the continuation signal: nonzero
    /// means call this tool again (same `index`) to keep going, zero means
    /// the semantic index is fully caught up. This is the primary mechanism
    /// for a long semantic rebuild, since it works even for a client (such
    /// as Claude Cowork) that never sends `_meta.progressToken` and so
    /// cannot receive the MCP progress notifications also streamed here for
    /// each phase boundary and, during the semantic phase, each completed
    /// document.
    #[tool(
        name = "query_index_rebuild",
        description = "Rebuild the text index, link graph, and/or semantic index from a full vault scan. The semantic phase returns early after `budget_seconds` (default from [vault.search] rebuild_budget_seconds, 25 if unset) with partial progress rather than blocking until finished: call again with the same `index` while the result's `semantic.remaining` is nonzero.",
        output_schema = fallible_output_schema_for::<QueryIndexRebuildToolResult>()
    )]
    async fn query_index_rebuild(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(input): Parameters<QueryIndexRebuildInput>,
    ) -> Result<Json<QueryIndexRebuildToolResult>, ToolFailure> {
        let (service, index) = self.search_service(input.vault.as_deref()).await?;
        let progress_token = context.meta.get_progress_token();
        let peer = context.peer;
        let configured_default = self
            .rebuild_budget_seconds
            .get(index)
            .copied()
            .unwrap_or(FALLBACK_REBUILD_BUDGET_SECONDS);
        let budget = resolve_rebuild_budget(input.budget_seconds, configured_default);

        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<RebuildProgress>();
        let forwarder = tokio::spawn(async move {
            let mut sequence = 0.0_f64;
            while let Some(update) = progress_rx.recv().await {
                sequence += 1.0;
                if let Some(token) = &progress_token {
                    let _ = peer
                        .notify_progress(rebuild_progress_notification(token.clone(), sequence, update))
                        .await;
                }
            }
        });

        let result = execute(move || {
            let report =
                service.rebuild_with_budget(RebuildTarget::from(input.index), Some(budget), &mut |update| {
                    let _ = progress_tx.send(update);
                })?;
            Ok(QueryIndexRebuildToolResult::from(report))
        })
        .await;

        let _ = forwarder.await;
        result
    }
}

impl ContextOsServer {
    /// Resolves the vault search service for `vault` (or the sole
    /// configured vault when `None`), matching the Git tools' `vault`
    /// parameter convention: default to the sole vault, else required.
    /// Also returns the resolved vault's index, for callers (currently only
    /// `query_index_rebuild`) that need it to look up other per-vault
    /// configuration alongside the service itself.
    pub(crate) async fn search_service(
        &self,
        vault: Option<&str>,
    ) -> Result<(Arc<VaultSearchService>, usize), ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let raw = vault.unwrap_or(".").to_owned();
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = self
            .search
            .get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::SearchDisabled))?;
        Ok((service, index))
    }
}

/// Renders one [`RebuildProgress`] update as an MCP progress notification.
/// `progress` is a monotonically increasing sequence number (every update
/// counts, per the MCP spec's "this should increase every time progress is
/// made"); `total`/`message` carry the actually meaningful detail, which is
/// only precise for the semantic phase's per-document count.
fn rebuild_progress_notification(
    token: ProgressToken,
    sequence: f64,
    update: RebuildProgress,
) -> ProgressNotificationParam {
    let message = match update {
        RebuildProgress::TextStarted => "rebuilding text index".to_owned(),
        RebuildProgress::TextComplete => "text index rebuilt".to_owned(),
        RebuildProgress::GraphStarted => "rebuilding link graph".to_owned(),
        RebuildProgress::GraphComplete => "link graph rebuilt".to_owned(),
        RebuildProgress::SemanticProgress { completed, total } => {
            format!("embedded {completed} of {total} documents")
        }
    };
    let param = ProgressNotificationParam::new(token, sequence).with_message(message);
    match update {
        RebuildProgress::SemanticProgress { total, .. } => {
            #[allow(clippy::cast_precision_loss)]
            let total = total as f64;
            param.with_total(total)
        }
        _ => param,
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryVaultInput {
    /// The vault to report on: `{name}://.` to select a specific
    /// configured vault by name (a bare vault name alone is not
    /// accepted for this parameter); omit to use the sole configured
    /// vault.
    vault: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryTextInput {
    query: String,
    path_prefix: Option<String>,
    /// Forward-slash relative path prefixes to exclude from results,
    /// matching whole path segments like `path_prefix`: an entry `"old"`
    /// excludes `"old"` and everything under `"old/"`, never
    /// `"oldstuff.md"`. Useful for excluding a superseded version of
    /// something (the previous draft, an archived copy) so it cannot bias
    /// or be mistaken for the current one while rewriting or researching.
    #[serde(default)]
    exclude_paths: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default = "default_query_text_limit")]
    limit: u64,
    /// The vault to search: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_query_text_limit() -> u64 {
    20
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QueryTextToolResult {
    hits: Vec<TextHitToolResult>,
    index_freshness: IndexFreshnessToolResult,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TextHitToolResult {
    path: String,
    score: f32,
    title: String,
    snippet: String,
    modified: String,
}

impl From<TextHit> for TextHitToolResult {
    fn from(value: TextHit) -> Self {
        Self {
            path: value.path,
            score: value.score,
            title: value.title,
            snippet: value.snippet,
            modified: value.modified.to_string(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexFreshnessToolResult {
    scanned: usize,
    reindexed: usize,
    removed: usize,
}

impl From<contextos_search::FreshnessReport> for IndexFreshnessToolResult {
    fn from(value: contextos_search::FreshnessReport) -> Self {
        Self {
            scanned: value.scanned,
            reindexed: value.reindexed,
            removed: value.removed,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QuerySemanticInput {
    query: String,
    #[serde(default = "default_query_semantic_limit")]
    limit: u64,
    path_prefix: Option<String>,
    /// Forward-slash relative path prefixes to exclude from results,
    /// matching whole path segments like `path_prefix`: an entry `"old"`
    /// excludes `"old"` and everything under `"old/"`, never
    /// `"oldstuff.md"`. Useful for excluding a superseded version of
    /// something (the previous draft, an archived copy) so it cannot bias
    /// or be mistaken for the current one while rewriting or researching.
    #[serde(default)]
    exclude_paths: Vec<String>,
    /// The vault to search: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_query_semantic_limit() -> u64 {
    10
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QuerySemanticToolResult {
    hits: Vec<SemanticHitToolResult>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SemanticHitToolResult {
    path: String,
    chunk: String,
    score: f32,
    heading_context: Vec<String>,
}

impl From<SemanticHit> for SemanticHitToolResult {
    fn from(value: SemanticHit) -> Self {
        Self {
            path: value.path,
            chunk: value.chunk,
            score: value.score,
            heading_context: value.heading_context,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryGraphInput {
    operation: GraphOperationInput,
    from: Option<String>,
    to: Option<String>,
    #[serde(default = "default_graph_depth")]
    depth: u32,
    #[serde(default)]
    direction: GraphDirectionInput,
    /// The vault to query: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_graph_depth() -> u32 {
    1
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GraphOperationInput {
    Neighbours,
    Backlinks,
    Path,
    Orphans,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum GraphDirectionInput {
    Out,
    In,
    #[default]
    Both,
}

impl From<GraphDirectionInput> for GraphDirection {
    fn from(value: GraphDirectionInput) -> Self {
        match value {
            GraphDirectionInput::Out => Self::Out,
            GraphDirectionInput::In => Self::In,
            GraphDirectionInput::Both => Self::Both,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphViewToolResult {
    nodes: Vec<GraphNodeToolResult>,
    edges: Vec<GraphEdgeToolResult>,
}

impl From<GraphView> for GraphViewToolResult {
    fn from(value: GraphView) -> Self {
        Self {
            nodes: value.nodes.into_iter().map(GraphNodeToolResult::from).collect(),
            edges: value.edges.into_iter().map(GraphEdgeToolResult::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphNodeToolResult {
    path: String,
    title: String,
    phantom: bool,
}

impl From<GraphNode> for GraphNodeToolResult {
    fn from(value: GraphNode) -> Self {
        Self {
            path: value.path,
            title: value.title,
            phantom: value.phantom,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphEdgeToolResult {
    from: String,
    to: String,
    kind: GraphEdgeKindToolResult,
}

impl From<GraphEdge> for GraphEdgeToolResult {
    fn from(value: GraphEdge) -> Self {
        Self {
            from: value.from,
            to: value.to,
            kind: GraphEdgeKindToolResult::from(value.kind),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum GraphEdgeKindToolResult {
    Link,
    Embed,
}

impl From<GraphEdgeKind> for GraphEdgeKindToolResult {
    fn from(value: GraphEdgeKind) -> Self {
        match value {
            GraphEdgeKind::Link => Self::Link,
            GraphEdgeKind::Embed => Self::Embed,
        }
    }
}

/// Safety-net fallback for [`ContextOsServer::rebuild_budget_seconds`] if a
/// vault index is ever out of range against that lookup table (should not
/// happen in practice: it is built with exactly one entry per configured
/// vault). Same value as `[vault.search] rebuild_budget_seconds`'s own
/// config default, so this is never a user-visible behaviour change.
const FALLBACK_REBUILD_BUDGET_SECONDS: u64 = 25;

/// Resolves the effective semantic-rebuild budget: an explicit per-call
/// `requested` value always wins; otherwise falls back to
/// `configured_default` (this vault's `[vault.search]
/// rebuild_budget_seconds`). A `requested`/`configured_default` value too
/// large to represent as `i64` seconds falls back to `i64::MAX` seconds
/// (effectively unbounded) rather than panicking.
fn resolve_rebuild_budget(requested: Option<u64>, configured_default: u64) -> time::Duration {
    let seconds = requested.unwrap_or(configured_default);
    time::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryIndexRebuildInput {
    #[serde(default)]
    index: RebuildIndexInput,
    /// The vault to rebuild: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
    /// Maximum seconds the semantic phase may spend embedding documents
    /// before returning early with partial progress; ignored by the text
    /// and graph phases, which always run to completion. Defaults to this
    /// vault's configured `[vault.search] rebuild_budget_seconds` when
    /// omitted.
    // `#[serde(default)]` is required alongside `schema_with` here: the
    // schema-generation macro derives whether a field counts as "optional"
    // (and thus absent from the schema's `required` array) from the same
    // overridden schema type `schema_with` supplies, and a bare function
    // override's wrapper type is not itself recognised as an `Option`.
    // Without this, budget_seconds would be incorrectly marked required.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    budget_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RebuildIndexInput {
    Text,
    Graph,
    Semantic,
    #[default]
    All,
}

impl From<RebuildIndexInput> for RebuildTarget {
    fn from(value: RebuildIndexInput) -> Self {
        match value {
            RebuildIndexInput::Text => Self::Text,
            RebuildIndexInput::Graph => Self::Graph,
            RebuildIndexInput::Semantic => Self::Semantic,
            RebuildIndexInput::All => Self::All,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QueryIndexRebuildToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<IndexFreshnessToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphRebuildToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<SemanticRebuildToolResult>,
}

impl From<RebuildReport> for QueryIndexRebuildToolResult {
    fn from(value: RebuildReport) -> Self {
        Self {
            text: value.text.map(IndexFreshnessToolResult::from),
            graph: value.graph.map(GraphRebuildToolResult::from),
            semantic: value.semantic.map(SemanticRebuildToolResult::from),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphRebuildToolResult {
    notes_scanned: usize,
    nodes: usize,
    edges: usize,
}

impl From<GraphRebuildReport> for GraphRebuildToolResult {
    fn from(value: GraphRebuildReport) -> Self {
        Self {
            notes_scanned: value.notes_scanned,
            nodes: value.nodes,
            edges: value.edges,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SemanticRebuildToolResult {
    paths_scanned: usize,
    embedded: usize,
    skipped: usize,
    failed: usize,
    /// Documents still queued for embedding once this call returned.
    /// Nonzero means the rebuild budget was reached before the queue was
    /// empty: call `query_index_rebuild` again with the same `index` to
    /// continue. Zero means the semantic index is fully caught up.
    remaining: usize,
    /// Human-readable summary of `remaining`, meant for a tool-calling
    /// agent deciding whether to call this tool again.
    message: String,
}

impl From<SemanticRebuildReport> for SemanticRebuildToolResult {
    fn from(value: SemanticRebuildReport) -> Self {
        let message = if value.remaining > 0 {
            format!(
                "Embedded {} document(s) this pass; {} still pending. Call query_index_rebuild again with the same index target to continue.",
                value.paths_scanned, value.remaining
            )
        } else {
            "Semantic index is fully up to date.".to_owned()
        };
        Self {
            paths_scanned: value.paths_scanned,
            embedded: value.embedded,
            remaining: value.remaining,
            message,
            skipped: value.skipped,
            failed: value.failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_rebuild_budget;

    #[test]
    fn resolve_rebuild_budget_falls_back_to_the_configured_vault_default_when_omitted() {
        assert_eq!(resolve_rebuild_budget(None, 60), time::Duration::seconds(60));
    }

    #[test]
    fn resolve_rebuild_budget_prefers_an_explicit_per_call_override() {
        assert_eq!(resolve_rebuild_budget(Some(5), 60), time::Duration::seconds(5));
    }

    #[test]
    fn resolve_rebuild_budget_never_panics_on_an_unrepresentable_second_count() {
        assert_eq!(
            resolve_rebuild_budget(None, u64::MAX),
            time::Duration::seconds(i64::MAX)
        );
    }
}
