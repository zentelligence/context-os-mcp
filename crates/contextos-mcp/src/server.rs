use std::sync::Arc;

use contextos_core::{
    LogsOperations, MaintainsIndexes, OperationEvent, OperationRouter, OperationRouterConfig,
    OperationWarning, PathError, SystemClock, VaultPath, VaultRootId, VaultSet, VersionsVault,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits,
    RoutedFilesystemServiceConfig,
};
use contextos_git::{Git2Vault, Git2VaultConfig, GitCommitResult, GitError, GitWriteError};
use contextos_index::{IndexService, IndexServiceConfig, IndexServiceError};
use contextos_mermaid::MermanParser;
use contextos_oplog::{OperationLog, OperationLogConfig, OperationLogError};
use contextos_search::{
    EmbeddingProviderConfig, EmbedsText, SearchError, SemanticConfig, VaultSearchConfig,
    VaultSearchService,
};
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::schemars;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::module::{ModuleContext, ModuleRegistry, ModuleRegistryError};
use crate::tool_error::ToolError;
use crate::tools::vault::VaultInfoConfig;
use crate::{Config, ConfigError, EmbeddingProvider, VaultConfig};

#[derive(Clone, Debug)]
pub(crate) struct WriteCoordinator {
    locks: Arc<Vec<Arc<Mutex<()>>>>,
}

impl From<usize> for WriteCoordinator {
    fn from(root_count: usize) -> Self {
        let locks = (0..root_count).map(|_| Arc::new(Mutex::new(()))).collect();
        Self {
            locks: Arc::new(locks),
        }
    }
}

impl WriteCoordinator {
    pub(crate) async fn lock_roots(
        &self,
        roots: &[VaultRootId],
    ) -> Result<Vec<OwnedMutexGuard<()>>, ToolError> {
        let mut indices = roots
            .iter()
            .copied()
            .map(usize::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        indices.sort_unstable();
        indices.dedup();

        let mut guards = Vec::with_capacity(indices.len());
        for root_index in indices {
            let lock = self
                .locks
                .get(root_index)
                .cloned()
                .ok_or(ToolError::RootLockMissing { root_index })?;
            guards.push(lock.lock_owned().await);
        }
        Ok(guards)
    }
}

#[derive(Clone)]
pub struct ContextOsServer {
    /// The effective configuration this server was built from, retained so
    /// `doctor`/`doctor_resolve` can reuse
    /// `DoctorReport::try_from(&Config)` unchanged, giving identical
    /// diagnostic content to `contextos doctor` on the CLI.
    pub(crate) config: Arc<Config>,
    pub(crate) roots: Arc<VaultSet>,
    pub(crate) filesystem: Arc<Filesystem>,
    pub(crate) mutations: Arc<RoutedMutationService>,
    pub(crate) writes: WriteCoordinator,
    pub(crate) destructive_delete: Arc<Vec<bool>>,
    pub(crate) indexes: Arc<Vec<Option<ManagedIndexService>>>,
    pub(crate) operation_logs: Arc<Vec<Option<ManagedOperationLog>>>,
    pub(crate) git: Arc<Vec<Option<ManagedGit>>>,
    pub(crate) git_debounce_seconds: Arc<Vec<u64>>,
    pub(crate) search: Arc<Vec<Option<Arc<VaultSearchService>>>>,
    pub(crate) rebuild_budget_seconds: Arc<Vec<u64>>,
    pub(crate) vault_info: Arc<VaultInfoConfig>,
    pub(crate) mermaid: Arc<MermanParser>,
    /// Per-vault, index-aligned with `roots`: glob patterns naming which
    /// files `resources/list` enumerates for that vault; empty means that vault reports nothing via
    /// `resources/list`. Scoped to this one surface only; every other
    /// enumeration tool keeps using `hidden` alone via `filesystem`.
    pub(crate) resources_list_include: Arc<Vec<Vec<String>>>,
    pub(crate) module_router: Arc<rmcp::handler::server::router::tool::ToolRouter<Self>>,
    /// Size in bytes at or above which a text-reading tool result attaches
    /// a `resource_link` content block; precomputed once
    /// from `[server] resource_link_threshold_kb` at construction.
    pub(crate) resource_link_threshold_bytes: u64,
}

pub(crate) type ManagedIndexService = IndexService<Filesystem, FilesystemService<SystemClock>>;
type ManagedOperationLog = OperationLog<FilesystemService<SystemClock>>;
pub(crate) type ManagedGit = Git2Vault<SystemClock>;
pub(crate) type SubstrateRouter =
    OperationRouter<IndexCollection, OperationLogCollection, VersionCollection, SearchCollection>;
pub(crate) type RoutedMutationService = FilesystemService<SystemClock, SubstrateRouter>;

#[derive(Clone, Debug)]
pub(crate) struct IndexCollection {
    services: Arc<Vec<Option<ManagedIndexService>>>,
}

impl MaintainsIndexes for IndexCollection {
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        let mut events = Vec::new();
        for service in self.services.iter().flatten() {
            events.extend(service.reconcile(event)?);
        }
        Ok(events)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperationLogCollection {
    services: Arc<Vec<Option<ManagedOperationLog>>>,
}

impl LogsOperations for OperationLogCollection {
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        let mut events = Vec::new();
        for service in self.services.iter().flatten() {
            events.extend(service.append(event)?);
        }
        Ok(events)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VersionCollection {
    services: Arc<Vec<Option<ManagedGit>>>,
    debounce_seconds: Arc<Vec<u64>>,
}

impl VersionsVault for VersionCollection {
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let mut roots = event
            .paths
            .iter()
            .map(VaultPath::root_id)
            .collect::<Vec<_>>();
        roots.sort_by_key(|root| usize::try_from(*root).unwrap_or(usize::MAX));
        roots.dedup();
        for root in roots {
            let index = usize::try_from(root).map_err(|error| OperationWarning {
                code: "git/stage".to_owned(),
                message: error.to_string(),
            })?;
            if let Some(service) = self.services.get(index).and_then(Option::as_ref)
                && service.is_repository()
            {
                let mut scoped = event.clone();
                scoped.paths.retain(|path| path.root_id() == root);
                service.stage(&scoped)?;
                let generation = service
                    .pending_generation()
                    .map_err(OperationWarning::from)?;
                let delay = self.debounce_seconds.get(index).copied().unwrap_or(30);
                let service = service.clone();
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        let committed = tokio::task::spawn_blocking(move || {
                            service.commit_if_generation(generation)
                        })
                        .await;
                        match committed {
                            Ok(Ok(_)) => {}
                            Ok(Err(error)) => tracing::warn!(code = error.code(), error = %error, "debounced Git commit failed"),
                            Err(error) => tracing::warn!(error = %error, "debounced Git task failed"),
                        }
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SearchCollection {
    services: Arc<Vec<Option<Arc<VaultSearchService>>>>,
}

impl contextos_core::UpdatesSearch for SearchCollection {
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        for service in self.services.iter().flatten() {
            service.update(event)?;
        }
        Ok(())
    }
}

impl ContextOsServer {
    /// Every tool that ships in this binary regardless of runtime `Config`,
    /// i.e. the complete build-time set, with no extension module tools
    /// merged in. Every ephemeris tool handler is always compiled
    /// in and so always appears here, whether or not `[server] astro` is
    /// set; this is the base build-complete/reserved-name set transport
    /// parity tests and module-name collision checks reason about, not
    /// what any one running instance actually advertises. See
    /// [`Self::effective_catalogue`] for that.
    #[must_use]
    pub fn catalogue() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::core_tool_router();
        router.merge(Self::ephemeris_tool_router());
        sanitise_router_schemas(&mut router);
        router
    }

    /// Every built-in tool this instance actually advertises and can
    /// dispatch: the core catalogue, the `ephemeris_*` tools only if this
    /// instance's `[server] astro` is set (runtime, not a Cargo
    /// feature, so a single build serves every operator, opted in or not),
    /// plus every registered extension module's namespaced tools. This is
    /// what dispatch actually uses.
    #[must_use]
    pub fn effective_catalogue(&self) -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::core_tool_router();
        if self.config.server.astro {
            router.merge(Self::ephemeris_tool_router());
        }
        router.merge(self.module_router.as_ref().clone());
        sanitise_router_schemas(&mut router);
        router
    }

    /// Every built-in tool that carries no runtime visibility toggle:
    /// shared by [`Self::catalogue`] (which always adds the ephemeris
    /// tools on top) and [`Self::effective_catalogue`] (which adds them
    /// only when `[server] astro` is set for this instance).
    fn core_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::tool_router();
        router.merge(Self::mermaid_tool_router());
        router.merge(Self::git_tool_router());
        router.merge(Self::query_tool_router());
        router.merge(Self::vault_tool_router());
        router.merge(Self::obsidian_tool_router());
        router.merge(Self::doctor_tool_router());
        router
    }

    /// Builds the per-call capability context handed to every extension
    /// module tool. Cloning is cheap: every field is an `Arc` or an
    /// `Arc`-backed coordinator.
    fn module_context(&self) -> ModuleContext {
        ModuleContext {
            roots: Arc::clone(&self.roots),
            reads: Arc::clone(&self.filesystem),
            mutations: Arc::clone(&self.mutations),
            writes: self.writes.clone(),
            search: Arc::clone(&self.search),
        }
    }
}

/// Construction input pairing server configuration with the extension
/// modules to register. [`TryFrom<Config>`] delegates
/// here with an empty [`ModuleRegistry`], so every existing caller and test
/// that only supplies [`Config`] is unaffected: the effective catalogue
/// stays byte-identical to the core catalogue whenever no module is
/// registered.
pub struct ServerBuildConfig {
    pub config: Config,
    pub modules: ModuleRegistry,
}

impl TryFrom<Config> for ContextOsServer {
    type Error = ServerBuildError;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        Self::try_from(ServerBuildConfig {
            config: value,
            modules: ModuleRegistry::new(),
        })
    }
}

impl TryFrom<ServerBuildConfig> for ContextOsServer {
    type Error = ServerBuildError;

    #[expect(
        clippy::too_many_lines,
        reason = "the composition root keeps all per-vault service wiring visible in one auditable boundary"
    )]
    fn try_from(build: ServerBuildConfig) -> Result<Self, Self::Error> {
        let ServerBuildConfig {
            config: value,
            modules,
        } = build;
        let vault_info = Arc::new(VaultInfoConfig::from(&value));
        let destructive_delete = value
            .vaults
            .iter()
            .map(|vault| vault.git.destructive_delete)
            .collect::<Vec<_>>();
        let roots = VaultSet::try_from(&value)?;
        let limits = value
            .vaults
            .iter()
            .map(|vault| {
                Ok(FsLimits {
                    max_read_bytes: vault
                        .limits
                        .max_read_mb
                        .checked_mul(1024 * 1024)
                        .ok_or(ServerBuildError::ReadLimitOverflow)?,
                    max_batch_files: vault.limits.max_batch_files,
                })
            })
            .collect::<Result<Vec<_>, ServerBuildError>>()?;
        let hidden = value
            .vaults
            .iter()
            .map(|vault| vault.hidden.clone())
            .collect::<Vec<_>>();
        let resources_list_include = value
            .vaults
            .iter()
            .map(|vault| vault.resources_list_include.clone())
            .collect::<Vec<_>>();
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits,
            hidden,
            atomic_write_guard: None,
        })?;
        let plain_mutations = FilesystemService::from(FilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: SystemClock,
        });
        let indexes = value
            .vaults
            .iter()
            .zip(roots.iter())
            .map(|(config, root)| {
                if !config.managed || !config.index_md.enabled {
                    return Ok(None);
                }
                IndexService::try_from(IndexServiceConfig {
                    root: root.clone(),
                    roots: roots.clone(),
                    reader: filesystem.clone(),
                    writer: plain_mutations.clone(),
                    excluded: config.index_md.exclude.clone(),
                })
                .map(Some)
                .map_err(ServerBuildError::Index)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let indexes = Arc::new(indexes);
        let operation_logs = value
            .vaults
            .iter()
            .zip(roots.iter())
            .map(|(config, root)| {
                if !config.managed || !config.oplog.enabled {
                    return Ok(None);
                }
                let relative_directory = config
                    .oplog
                    .path
                    .to_str()
                    .ok_or_else(|| ServerBuildError::NonUtf8OperationLogPath {
                        path: config.oplog.path.clone(),
                    })?
                    .to_owned();
                OperationLog::try_from(OperationLogConfig {
                    root: root.clone(),
                    roots: roots.clone(),
                    relative_directory,
                    appender: plain_mutations.clone(),
                })
                .map(Some)
                .map_err(ServerBuildError::OperationLog)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let operation_logs = Arc::new(operation_logs);
        let git = value
            .vaults
            .iter()
            .zip(roots.iter())
            .map(|(config, root)| {
                if !config.managed || !config.git.enabled {
                    return Ok(None);
                }
                Git2Vault::try_from(Git2VaultConfig {
                    root: root.clone(),
                    roots: roots.clone(),
                    clock: SystemClock,
                    author_name: config.git.author_name.clone(),
                    author_email: config.git.author_email.clone(),
                    allow_destructive_restore: config.git.destructive_delete,
                    protected_restore_paths: {
                        let mut paths = config.git.restore_exclude.clone();
                        if config.oplog.enabled && !paths.contains(&config.oplog.path) {
                            paths.push(config.oplog.path.clone());
                        }
                        paths
                    },
                })
                .map(Some)
                .map_err(ServerBuildError::Git)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let git = Arc::new(git);
        for service in git.iter().flatten() {
            if service.is_repository() {
                service
                    .initialise(&plain_mutations)
                    .map_err(ServerBuildError::GitWrite)?;
            }
            service.recover_staged().map_err(ServerBuildError::Git)?;
        }
        let debounce_seconds = Arc::new(
            value
                .vaults
                .iter()
                .map(|config| config.git.commit_debounce_s)
                .collect::<Vec<_>>(),
        );
        let search = value
            .vaults
            .iter()
            .zip(roots.iter())
            .enumerate()
            .map(|(index, (config, root))| {
                let enabled = config.search.text || config.search.graph || config.search.semantic;
                if !config.managed || !enabled {
                    return Ok(None);
                }
                let root_id = VaultRootId::try_from(index)?;
                let state_directory = crate::state_dir::resolve_state_directory(
                    config.state_directory.as_deref(),
                    root.path(),
                )?;
                let semantic = semantic_config(config, &state_directory)?;
                tracing::info!(
                    vault = %root.path().display(),
                    state_directory = %state_directory.display(),
                    text_enabled = config.search.text,
                    graph_enabled = config.search.graph,
                    graph_backend = ?config.search.graph_backend,
                    semantic_enabled = semantic.is_some(),
                    "opening vault search service"
                );
                VaultSearchService::try_from(VaultSearchConfig {
                    root_id,
                    root: root.path().to_path_buf(),
                    excludes: config.search.exclude.clone(),
                    state_directory: state_directory.clone(),
                    text_enabled: config.search.text,
                    graph_enabled: config.search.graph,
                    graph_backend: config.search.graph_backend.into(),
                    semantic,
                })
                .inspect_err(|error| {
                    // The last line an operator sees before the whole
                    // server aborts construction over this one vault
                    // (`ServerBuildError`'s `?` below has no further
                    // context to add): pairs the vault and its resolved
                    // state directory with the stable error code, so a
                    // hard-to-reproduce startup failure is diagnosable
                    // from a log line alone.
                    tracing::error!(
                        vault = %root.path().display(),
                        state_directory = %state_directory.display(),
                        code = error.code(),
                        error = %error,
                        "vault search service failed to open; aborting server construction"
                    );
                })
                .map(|service| Some(Arc::new(service)))
                .map_err(ServerBuildError::from)
            })
            .collect::<Result<Vec<_>, ServerBuildError>>()?;
        let search = Arc::new(search);
        let rebuild_budget_seconds = Arc::new(
            value
                .vaults
                .iter()
                .map(|config| config.search.rebuild_budget_seconds)
                .collect::<Vec<_>>(),
        );
        let mutations = FilesystemService::from(RoutedFilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: SystemClock,
            services: OperationRouter::from(OperationRouterConfig {
                indexes: IndexCollection {
                    services: Arc::clone(&indexes),
                },
                operation_log: OperationLogCollection {
                    services: Arc::clone(&operation_logs),
                },
                versions: VersionCollection {
                    services: Arc::clone(&git),
                    debounce_seconds: Arc::clone(&debounce_seconds),
                },
                search: SearchCollection {
                    services: Arc::clone(&search),
                },
            }),
        });
        let writes = WriteCoordinator::from(roots.len());
        let reserved_tool_names: std::collections::HashSet<String> = Self::catalogue()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        modules.validate(&reserved_tool_names)?;
        let module_router = build_module_router(&modules);
        let resource_link_threshold_bytes = value
            .server
            .resource_link_threshold_kb
            .checked_mul(1024)
            .ok_or(ServerBuildError::ResourceLinkThresholdOverflow)?;
        Ok(Self {
            config: Arc::new(value),
            roots: Arc::new(roots),
            filesystem: Arc::new(filesystem),
            mutations: Arc::new(mutations),
            writes,
            destructive_delete: Arc::new(destructive_delete),
            indexes,
            operation_logs,
            git,
            git_debounce_seconds: debounce_seconds,
            search,
            rebuild_budget_seconds,
            vault_info,
            mermaid: Arc::new(MermanParser::new()),
            resources_list_include: Arc::new(resources_list_include),
            resource_link_threshold_bytes,
            module_router: Arc::new(module_router),
        })
    }
}

/// Applies [`crate::resource_support::sanitise_nullable_unions`] to every
/// route's advertised `input_schema` and `output_schema` in place, without
/// touching its dispatch handler: the single choke point [`Self::catalogue`]
/// and [`Self::effective_catalogue`] both call after every `merge` (core,
/// ephemeris, and registered modules alike), so no tool this server can
/// ever advertise, built in or extension, carries a nullable-union shape
/// confirmed to take down Cowork's tool registry.
fn sanitise_router_schemas(
    router: &mut rmcp::handler::server::router::tool::ToolRouter<ContextOsServer>,
) {
    for route in router.map.values_mut() {
        let mut input = serde_json::Value::Object((*route.attr.input_schema).clone());
        crate::resource_support::sanitise_nullable_unions(&mut input);
        crate::resource_support::inline_local_refs(&mut input);
        if let serde_json::Value::Object(map) = input {
            route.attr.input_schema = Arc::new(map);
        }

        if let Some(output) = route.attr.output_schema.take() {
            let mut value = serde_json::Value::Object((*output).clone());
            crate::resource_support::sanitise_nullable_unions(&mut value);
            crate::resource_support::inline_local_refs(&mut value);
            if let serde_json::Value::Object(map) = value {
                route.attr.output_schema = Some(Arc::new(map));
            }
        }
    }
}

/// Builds the dispatch route for every tool every registered module
/// contributes. Each route resolves its [`ModuleContext`] fresh from the
/// live server instance at call time (`tcc.service`), so a module never
/// holds server state directly.
fn build_module_router(
    modules: &ModuleRegistry,
) -> rmcp::handler::server::router::tool::ToolRouter<ContextOsServer> {
    let mut router = rmcp::handler::server::router::tool::ToolRouter::<ContextOsServer>::new();
    for module in modules.iter() {
        for tool in module.tools() {
            let module = Arc::clone(module);
            router.add_route(ToolRoute::new_dyn(
                tool,
                move |tcc: ToolCallContext<'_, ContextOsServer>| {
                    let module = Arc::clone(&module);
                    let ctx = tcc.service.module_context();
                    let call = crate::module::ModuleCall {
                        name: tcc.name.to_string(),
                        arguments: tcc.arguments.clone().unwrap_or_default(),
                    };
                    Box::pin(async move { module.handle(call, &ctx).await })
                },
            ));
        }
    }
    router
}

/// Builds this vault's semantic search capability from configuration
/// (`[vault.search] semantic`, `[vault.search.embedding]`), or `None` when
/// semantic search is off for this vault. The vector store lives at
/// `<state_directory>/vectors.db`, matching the vault's other derived
/// state.
pub(crate) fn semantic_config(
    config: &VaultConfig,
    state_directory: &std::path::Path,
) -> Result<Option<SemanticConfig>, SearchError> {
    if !config.search.semantic {
        return Ok(None);
    }
    let provider_config = embedding_provider_config(&config.search.embedding)?;
    let embedder: Box<dyn EmbedsText> = provider_config.try_into()?;
    Ok(Some(SemanticConfig {
        embedder,
        vector_store_path: state_directory.join("vectors.db"),
    }))
}

/// Converts the server's TOML-facing embedding configuration into
/// `contextos-search`'s vault-agnostic provider selection, so provider
/// construction stays purely configuration-driven (swapping `provider`
/// changes which variant is built, with no code change).
///
/// # Errors
///
/// Returns [`SearchError::EmbeddingConfig`] when `provider = "local"` and no
/// `model_directory` is configured; the directory to look in must always
/// be explicit. That directory need not be the exact flat directory
/// `FastembedLocal` reads from: `crate::model_cli::resolve_model_directory`
/// resolves a shared `contextos model download` cache root to the actual
/// model snapshot directory first.
fn embedding_provider_config(
    embedding: &crate::EmbeddingConfig,
) -> Result<EmbeddingProviderConfig, SearchError> {
    match embedding.provider {
        EmbeddingProvider::Local => {
            let model_directory =
                embedding
                    .model_directory
                    .clone()
                    .ok_or_else(|| SearchError::EmbeddingConfig {
                        reason: "provider = \"local\" requires [vault.search.embedding] \
                             model_directory to be configured"
                            .to_owned(),
                    })?;
            let model_directory = crate::model_cli::resolve_model_directory(&model_directory);
            Ok(EmbeddingProviderConfig::Local { model_directory })
        }
        EmbeddingProvider::OpenaiCompatible => Ok(EmbeddingProviderConfig::OpenAiCompatible {
            endpoint: embedding.endpoint.clone(),
            model: embedding.model.clone(),
            api_key_env: embedding.api_key_env.clone(),
        }),
    }
}

impl ContextOsServer {
    /// Flushes every pending MCP-owned Git batch, including during graceful shutdown.
    ///
    /// # Errors
    ///
    /// Returns the first typed Git failure while committing a configured vault.
    pub fn flush_git(&self) -> Result<Vec<GitCommitResult>, GitError> {
        let mut results = Vec::new();
        let mut first_error = None;
        for service in self.git.iter().flatten() {
            match service.commit(None) {
                Ok(result) => results.push(result),
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(error) => tracing::warn!(
                    code = error.code(),
                    error = %error,
                    "additional Git vault failed during graceful flush"
                ),
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(results),
        }
    }

    /// Retries buffered operation-log records for every configured vault.
    ///
    /// All vaults are attempted before the first typed failure is returned.
    ///
    /// # Errors
    ///
    /// Returns the first operation-log failure after attempting every vault.
    pub fn flush_operation_logs(&self) -> Result<Vec<OperationEvent>, OperationLogError<FsError>> {
        let mut events = Vec::new();
        let mut first_error = None;
        for service in self.operation_logs.iter().flatten() {
            match service.flush() {
                Ok(flushed) => events.extend(flushed),
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(error) => tracing::warn!(
                    code = error.code(),
                    error = %error,
                    "additional operation-log vault failed during graceful flush"
                ),
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(events),
        }
    }

    /// Flushes retryable operation-log state and pending Git batches at shutdown.
    ///
    /// Both substrates are attempted even when one reports a failure.
    ///
    /// # Errors
    ///
    /// Returns the first typed substrate failure after both flushes are attempted.
    pub fn flush_substrates(&self) -> Result<(), ShutdownFlushError> {
        let operation_log_error = self.flush_operation_logs().err();
        let git_error = self.flush_git().err();
        match (operation_log_error, git_error) {
            (Some(error), Some(git_error)) => {
                tracing::warn!(
                    code = git_error.code(),
                    error = %git_error,
                    "Git also failed during graceful substrate flush"
                );
                Err(ShutdownFlushError::OperationLog(error))
            }
            (Some(error), None) => Err(ShutdownFlushError::OperationLog(error)),
            (None, Some(error)) => Err(ShutdownFlushError::Git(error)),
            (None, None) => Ok(()),
        }
    }

    /// Spawns one background task per vault with semantic search enabled,
    /// each repeatedly draining that vault's embedding queue on its own
    /// (`crate::semantic_drain`) so the semantic index keeps pace with
    /// live edits without an operator ever having to call
    /// `query_index_rebuild` or `contextos index` manually. `shutdown`
    /// stops every spawned task promptly; the caller should await each
    /// returned handle as part of its own graceful-shutdown sequence.
    #[must_use]
    pub fn spawn_semantic_drain(
        &self,
        shutdown: &CancellationToken,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        crate::semantic_drain::spawn(&self.search, &self.rebuild_budget_seconds, shutdown)
    }
}

pub(crate) struct WarningMessages(pub(crate) Vec<String>);

impl From<Vec<OperationWarning>> for WarningMessages {
    fn from(value: Vec<OperationWarning>) -> Self {
        Self(
            value
                .into_iter()
                .map(|warning| format!("{}: {}", warning.code, warning.message))
                .collect(),
        )
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
}

#[derive(Debug, Error)]
pub enum ServerBuildError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Index(#[from] IndexServiceError<FsError, FsError>),
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error("operation-log path is not valid UTF-8: {path}")]
    NonUtf8OperationLogPath { path: std::path::PathBuf },
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GitWrite(#[from] GitWriteError<FsError>),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Module(#[from] ModuleRegistryError),
    #[error(transparent)]
    StateDir(#[from] crate::state_dir::StateDirError),
    #[error("at least one vault is required")]
    NoVaults,
    #[error("configured maximum read size overflows bytes")]
    ReadLimitOverflow,
    #[error("configured resource_link_threshold_kb overflows a byte count")]
    ResourceLinkThresholdOverflow,
}

/// Typed failure from graceful substrate persistence during server shutdown.
#[derive(Debug, Error)]
pub enum ShutdownFlushError {
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error(transparent)]
    Git(#[from] GitError),
}

#[cfg(test)]
mod tests {
    use super::{WriteCoordinator, embedding_provider_config};
    use crate::{EmbeddingConfig, EmbeddingProvider};
    use contextos_search::{EmbeddingProviderConfig, REQUIRED_MODEL_FILES};

    #[tokio::test]
    async fn concurrent_mutations_for_one_root_are_serialised()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let coordinator = WriteCoordinator::from(1_usize);
        let root = contextos_core::VaultRootId::try_from(0_usize)?;
        let first_guards = coordinator.lock_roots(&[root]).await?;
        let competitor = coordinator.clone();
        let waiting = tokio::spawn(async move { competitor.lock_roots(&[root]).await });

        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        drop(first_guards);
        let second_guards = waiting.await??;
        assert_eq!(second_guards.len(), 1);
        Ok(())
    }

    #[test]
    fn local_provider_config_resolves_a_shared_cache_root_model_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let repo_dir = cache.path().join("models--Qdrant--all-MiniLM-L6-v2-onnx");
        let snapshot_dir = repo_dir.join("snapshots").join("test-revision");
        std::fs::create_dir_all(&snapshot_dir)?;
        let refs_dir = repo_dir.join("refs");
        std::fs::create_dir_all(&refs_dir)?;
        std::fs::write(refs_dir.join("main"), "test-revision")?;
        for file in REQUIRED_MODEL_FILES {
            std::fs::write(snapshot_dir.join(file), b"fixture")?;
        }

        let embedding = EmbeddingConfig {
            provider: EmbeddingProvider::Local,
            model: String::new(),
            endpoint: String::new(),
            api_key_env: String::new(),
            model_directory: Some(cache.path().to_path_buf()),
        };

        let provider_config = embedding_provider_config(&embedding)?;

        let EmbeddingProviderConfig::Local { model_directory } = provider_config else {
            return Err("expected a Local provider config".into());
        };
        assert_eq!(model_directory, snapshot_dir);
        Ok(())
    }
}
