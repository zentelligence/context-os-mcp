//! `vault_info`, `vault_index_rebuild`, `vault_log_append`: whole-vault
//! reporting and maintenance that spans indexing, Git, and the operation
//! log rather than one substrate alone.

use std::sync::Arc;

use contextos_core::{
    Clock, OperationEvent, Origin, SystemClock, VaultPath, VaultPathInput, VaultSet,
};
use contextos_fs::Filesystem;
use contextos_git::GitError;
use contextos_oplog::ManualLogInput;
use contextos_search::{SearchError, VaultSearchService};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ProtocolVersion;
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::resource_support::{ResourceError, fallible_output_schema_for};
use crate::resources::resource_eligible_file_counts;
use crate::server::{ContextOsServer, ManagedGit};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::index_status::IndexesStatusToolResult;
use crate::{Config, EmbeddingProvider, GraphBackendConfig, Transport, VaultConfig};

#[rmcp::tool_router(router = vault_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "vault_info",
        description = "Report server, transport, effective vault configuration, and substrate health without exposing secrets",
        output_schema = fallible_output_schema_for::<VaultInfoToolResult>()
    )]
    async fn vault_info(&self) -> Result<Json<VaultInfoToolResult>, ToolFailure> {
        let config = Arc::clone(&self.vault_info);
        let git = Arc::clone(&self.git);
        let search = Arc::clone(&self.search);
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let resources_list_include = Arc::clone(&self.resources_list_include);
        execute(move || {
            VaultInfoToolResult::try_from(VaultInfoSource {
                config,
                git,
                search,
                roots,
                filesystem,
                resources_list_include,
            })
            .map_err(ToolError::from)
        })
        .await
    }

    #[tool(
        name = "vault_index_rebuild",
        description = "Rebuild managed index.md files for every non-excluded folder in a subtree",
        output_schema = fallible_output_schema_for::<IndexRebuildToolResult>()
    )]
    async fn rebuild_indexes(
        &self,
        Parameters(input): Parameters<IndexRebuildInput>,
    ) -> Result<Json<IndexRebuildToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let indexes = Arc::clone(&self.indexes);
        let path = evaluate(move || {
            VaultPath::try_from_vault_selector(&roots, input.path.as_deref().unwrap_or("."))
                .map_err(ToolError::from)
        })
        .await?;
        let root_index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = indexes
            .get(root_index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::IndexDisabled))?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            Ok(IndexRebuildToolResult::from(service.rebuild_report(
                &path,
                Origin::Tool("vault_index_rebuild".to_owned()),
                input.dry_run,
            )?))
        })
        .await
    }

    #[tool(
        name = "vault_log_append",
        description = "Append one explicit manual entry to the shared daily operation log",
        output_schema = fallible_output_schema_for::<LogAppendToolResult>()
    )]
    async fn append_log(
        &self,
        Parameters(input): Parameters<LogAppendInput>,
    ) -> Result<Json<LogAppendToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let operation_logs = Arc::clone(&self.operation_logs);
        let (root_id, files) = evaluate(move || {
            let mut files = input
                .files
                .iter()
                .map(|raw| VaultPath::try_from(VaultPathInput { roots: &roots, raw }))
                .collect::<Result<Vec<_>, _>>()?;
            let root_id = if let Some(path) = files.first() {
                path.root_id()
            } else {
                let path = VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: ".",
                })?;
                path.root_id()
            };
            if files.iter().any(|path| path.root_id() != root_id) {
                return Err(ToolError::Invalid(
                    "manual log files must belong to one configured vault",
                ));
            }
            files.shrink_to_fit();
            Ok((root_id, files))
        })
        .await?;
        let root_index = usize::try_from(root_id).map_err(ToolError::from)?;
        let service = operation_logs
            .get(root_index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::OperationLogDisabled))?;
        let guards = self
            .writes
            .lock_roots(&[root_id])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            let events = service.append_manual(&ManualLogInput {
                entry: input.entry,
                files,
                at: SystemClock.now(),
            })?;
            LogAppendToolResult::try_from(events)
        })
        .await
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VaultInfoConfig {
    transports: Vec<TransportToolResult>,
    vaults: Vec<VaultInfoConfiguredVault>,
    resource_link_threshold_kb: u64,
}

impl From<&Config> for VaultInfoConfig {
    fn from(value: &Config) -> Self {
        Self {
            transports: value
                .server
                .transports
                .iter()
                .copied()
                .map(TransportToolResult::from)
                .collect(),
            vaults: value
                .vaults
                .iter()
                .map(VaultInfoConfiguredVault::from)
                .collect(),
            resource_link_threshold_kb: value.server.resource_link_threshold_kb,
        }
    }
}

#[derive(Clone, Debug)]
struct VaultInfoConfiguredVault {
    path: String,
    managed: bool,
    git_enabled: bool,
    config_summary: VaultConfigSummaryToolResult,
}

impl From<&VaultConfig> for VaultInfoConfiguredVault {
    fn from(value: &VaultConfig) -> Self {
        let service_enabled = value.managed;
        Self {
            path: value.path.to_string_lossy().into_owned(),
            managed: value.managed,
            git_enabled: service_enabled && value.git.enabled,
            config_summary: VaultConfigSummaryToolResult::from(value),
        }
    }
}

#[derive(Clone, Debug)]
struct VaultInfoSource {
    config: Arc<VaultInfoConfig>,
    git: Arc<Vec<Option<ManagedGit>>>,
    search: Arc<Vec<Option<Arc<VaultSearchService>>>>,
    roots: Arc<VaultSet>,
    filesystem: Arc<Filesystem>,
    /// Threaded through so the reported
    /// `resource_eligible_files` count matches what `resources/list`
    /// itself now enumerates, not every file `resources/read` could serve.
    resources_list_include: Arc<Vec<Vec<String>>>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultInfoToolResult {
    version: String,
    protocol_version: String,
    transports: Vec<TransportToolResult>,
    /// The configured `[server] resource_link_threshold_kb`, so an
    /// operator can confirm the effective threshold without reading config
    /// files directly.
    resource_link_threshold_kb: u64,
    vaults: Vec<VaultInfoVaultToolResult>,
}

/// Typed failure while assembling the live per-vault `vault_info` report:
/// Git, search, or resource enumeration may each fail to report
/// its own live state.
#[derive(Debug, Error)]
enum VaultInfoError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Resource(#[from] ResourceError),
}

impl From<VaultInfoError> for ToolError {
    fn from(value: VaultInfoError) -> Self {
        match value {
            VaultInfoError::Git(error) => Self::Git(error),
            VaultInfoError::Search(error) => Self::Search(error),
            VaultInfoError::Resource(error) => Self::Resource(error),
        }
    }
}

impl TryFrom<VaultInfoSource> for VaultInfoToolResult {
    type Error = VaultInfoError;

    fn try_from(value: VaultInfoSource) -> Result<Self, Self::Error> {
        let resource_eligible_files = resource_eligible_file_counts(
            &value.roots,
            &value.filesystem,
            &value.resources_list_include,
        )?;
        let vaults = value
            .config
            .vaults
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, configured)| {
                // The effective name (explicit or defaulted from
                // the root directory's basename) lives on the resolved
                // `VaultRoot`, not the raw `VaultConfig` `configured` is
                // built from, so it is looked up here by the same index
                // rather than threaded through `VaultInfoConfiguredVault`.
                // Defensive only: `value.roots` is always built index-
                // aligned, one root per `value.config.vaults` entry, in the
                // same order (`TryFrom<&Config> for VaultSet`), so this
                // index should never actually miss.
                let name = value
                    .roots
                    .iter()
                    .nth(index)
                    .map_or_else(String::new, |root| root.name().to_owned());
                VaultInfoVaultToolResult::try_from((
                    configured,
                    value.git.get(index).and_then(Option::as_ref),
                    value.search.get(index).and_then(Option::as_ref),
                    resource_eligible_files.get(index).copied().unwrap_or(0),
                    name,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: ProtocolVersion::LATEST.to_string(),
            transports: value.config.transports.clone(),
            resource_link_threshold_kb: value.config.resource_link_threshold_kb,
            vaults,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TransportToolResult {
    Stdio,
    Http,
}

impl From<Transport> for TransportToolResult {
    fn from(value: Transport) -> Self {
        match value {
            Transport::Stdio => Self::Stdio,
            Transport::Http => Self::Http,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultInfoVaultToolResult {
    /// This vault's configured or default-derived name, used to
    /// address it as `{name}://{relative-path}`.
    name: String,
    path: String,
    managed: bool,
    git: VaultGitStatusToolResult,
    indexes: IndexesStatusToolResult,
    config_summary: VaultConfigSummaryToolResult,
    /// Count of files this root currently exposes through
    /// `resources/list` (`hidden`-eligible, any format), reusing the same
    /// enumeration `resources/list` itself uses rather than a second walk.
    resource_eligible_files: usize,
}

impl
    TryFrom<(
        VaultInfoConfiguredVault,
        Option<&ManagedGit>,
        Option<&Arc<VaultSearchService>>,
        usize,
        String,
    )> for VaultInfoVaultToolResult
{
    type Error = VaultInfoError;

    fn try_from(
        value: (
            VaultInfoConfiguredVault,
            Option<&ManagedGit>,
            Option<&Arc<VaultSearchService>>,
            usize,
            String,
        ),
    ) -> Result<Self, Self::Error> {
        let pending_commits = value
            .1
            .map(ManagedGit::pending_commit_count)
            .transpose()?
            .unwrap_or_default();
        let indexes = value
            .2
            .map(|service| service.status())
            .transpose()?
            .map_or_else(
                IndexesStatusToolResult::default,
                IndexesStatusToolResult::from,
            );
        Ok(Self {
            name: value.4,
            path: value.0.path,
            managed: value.0.managed,
            git: VaultGitStatusToolResult {
                enabled: value.0.git_enabled,
                repo: value.1.is_some_and(ManagedGit::is_repository),
                pending_commits,
            },
            indexes,
            config_summary: value.0.config_summary,
            resource_eligible_files: value.3,
        })
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultGitStatusToolResult {
    enabled: bool,
    repo: bool,
    pending_commits: usize,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct VaultConfigSummaryToolResult {
    limits: LimitsConfigSummaryToolResult,
    index_md: IndexMdConfigSummaryToolResult,
    oplog: OplogConfigSummaryToolResult,
    git: GitConfigSummaryToolResult,
    search: SearchConfigSummaryToolResult,
}

impl From<&VaultConfig> for VaultConfigSummaryToolResult {
    fn from(value: &VaultConfig) -> Self {
        let managed = value.managed;
        Self {
            limits: LimitsConfigSummaryToolResult {
                max_read_mb: value.limits.max_read_mb,
                max_batch_files: value.limits.max_batch_files,
            },
            index_md: IndexMdConfigSummaryToolResult {
                enabled: managed && value.index_md.enabled,
                exclude: value.index_md.exclude.clone(),
            },
            oplog: OplogConfigSummaryToolResult {
                enabled: managed && value.oplog.enabled,
                path: value.oplog.path.to_string_lossy().into_owned(),
            },
            git: GitConfigSummaryToolResult {
                enabled: managed && value.git.enabled,
                commit_debounce_s: value.git.commit_debounce_s,
                author_name: value.git.author_name.clone(),
                author_email: value.git.author_email.clone(),
                destructive_delete: value.git.destructive_delete,
                restore_exclude: value
                    .git
                    .restore_exclude
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            },
            search: SearchConfigSummaryToolResult {
                text: managed && value.search.text,
                graph: managed && value.search.graph,
                graph_backend: GraphBackendToolResult::from(value.search.graph_backend),
                semantic: managed && value.search.semantic,
                exclude: value.search.exclude.clone(),
                rebuild_budget_seconds: value.search.rebuild_budget_seconds,
                embedding: EmbeddingConfigSummaryToolResult::from(&value.search.embedding),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
struct LimitsConfigSummaryToolResult {
    max_read_mb: u64,
    max_batch_files: usize,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct IndexMdConfigSummaryToolResult {
    enabled: bool,
    exclude: Vec<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct OplogConfigSummaryToolResult {
    enabled: bool,
    path: String,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct GitConfigSummaryToolResult {
    enabled: bool,
    commit_debounce_s: u64,
    author_name: String,
    author_email: String,
    destructive_delete: bool,
    restore_exclude: Vec<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct SearchConfigSummaryToolResult {
    text: bool,
    graph: bool,
    /// The link graph's configured persistence backend, reported
    /// regardless of `graph`'s own `managed` gating:
    /// an operator diagnosing a graph-availability problem (for example
    /// `fjall`'s single-process lock under Cowork's multi-instance
    /// pattern) needs to see which backend is configured without reading
    /// the TOML file blind.
    graph_backend: GraphBackendToolResult,
    semantic: bool,
    exclude: Vec<String>,
    rebuild_budget_seconds: u64,
    embedding: EmbeddingConfigSummaryToolResult,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct EmbeddingConfigSummaryToolResult {
    provider: EmbeddingProviderToolResult,
    model: String,
    endpoint: String,
    api_key_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    model_directory: Option<String>,
}

impl From<&crate::EmbeddingConfig> for EmbeddingConfigSummaryToolResult {
    fn from(value: &crate::EmbeddingConfig) -> Self {
        Self {
            provider: EmbeddingProviderToolResult::from(value.provider),
            model: value.model.clone(),
            endpoint: value.endpoint.clone(),
            api_key_env: value.api_key_env.clone(),
            model_directory: value
                .model_directory
                .as_deref()
                .map(|path| path.display().to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum EmbeddingProviderToolResult {
    Local,
    OpenaiCompatible,
}

impl From<EmbeddingProvider> for EmbeddingProviderToolResult {
    fn from(value: EmbeddingProvider) -> Self {
        match value {
            EmbeddingProvider::Local => Self::Local,
            EmbeddingProvider::OpenaiCompatible => Self::OpenaiCompatible,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum GraphBackendToolResult {
    Serde,
    Fjall,
    Sqlite,
}

impl From<GraphBackendConfig> for GraphBackendToolResult {
    fn from(value: GraphBackendConfig) -> Self {
        match value {
            GraphBackendConfig::Serde => Self::Serde,
            GraphBackendConfig::Fjall => Self::Fjall,
            GraphBackendConfig::Sqlite => Self::Sqlite,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexRebuildToolResult {
    directories_scanned: usize,
    indexes_updated: usize,
    indexes_created: usize,
    skipped: usize,
}

impl From<contextos_index::IndexRebuildResult> for IndexRebuildToolResult {
    fn from(value: contextos_index::IndexRebuildResult) -> Self {
        Self {
            directories_scanned: value.directories_scanned,
            indexes_updated: value.indexes_updated,
            indexes_created: value.indexes_created,
            skipped: value.skipped,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LogAppendToolResult {
    path: String,
    appended: bool,
    warnings: Vec<String>,
}

impl TryFrom<Vec<OperationEvent>> for LogAppendToolResult {
    type Error = ToolError;

    fn try_from(value: Vec<OperationEvent>) -> Result<Self, Self::Error> {
        let path = value
            .last()
            .and_then(|event| event.paths.first())
            .ok_or(ToolError::ManualLogEventMissing)?
            .relative()
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            path,
            appended: true,
            warnings: Vec::new(),
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IndexRebuildInput {
    /// Scope the rebuild to this vault or subtree: a configured vault's
    /// bare name to rebuild that whole vault, or a path/
    /// `{name}://{relative-path}` to scope to a specific
    /// subtree; omit to rebuild the sole configured vault.
    path: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogAppendInput {
    entry: String,
    /// Each entry is vault-relative or absolute, or
    /// `{name}://{relative-path}`; every file must resolve to
    /// the same configured vault.
    #[serde(default)]
    files: Vec<String>,
}
