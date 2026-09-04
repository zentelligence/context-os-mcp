//! Extension module contract.
//!
//! A [`ServerModule`] contributes namespaced tools to the MCP catalogue.
//! Every call is dispatched with a [`ModuleContext`] that wraps the same
//! injected core services (`ReadsVault`/`ListsVault` for reads, the shared
//! lock-serialised write pipeline, and read-only query access) every
//! built-in tool uses, so a module using [`ModuleContext`] as intended
//! inherits path safety, write locking, logging, versioning, and indexing
//! for free, with no ergonomic reason to reach the filesystem directly.
//! This is a sanctioned, convenient path, not a security sandbox: a
//! `ServerModule` is in-process Rust with no capability isolation, so
//! nothing stops its own code from importing `std::fs`/`std::net` and
//! bypassing [`ModuleContext`] entirely, the same as any other in-process
//! Rust plugin trait. No reference module ships in this or any build; the
//! contract is exercised by test-only fixture modules that live in
//! `contextos-mcp`'s test suite and are never linked into the shipped
//! binary.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_core::{
    AppendMutation, AppendOutcome, AppendsVault, DeleteMutation, DeleteOutcome, ListsVault, MoveMutation, MoveOutcome,
    MovesVault, PathError, PipelineResult, ReadsVault, RestoreMutation, VaultEntry, VaultPath, VaultPathInput,
    VaultRootId, VaultSet, VaultText, WriteMutation, WriteOutcome, WritesVault,
};
use contextos_fs::{Filesystem, FsError};
use contextos_search::VaultSearchService;
use rmcp::ErrorData;
use rmcp::model::{CallToolResult, Tool};
use thiserror::Error;

use crate::server::{RoutedMutationService, WriteCoordinator};

/// The three reserved extension-module namespaces. `demo_*` is deliberately
/// absent: no reference module ships.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleNamespace {
    BusinessOs,
    PersonalOs,
    DeveloperOs,
}

impl ModuleNamespace {
    /// Returns the tool-name prefix every one of this module's tools must
    /// carry.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::BusinessOs => "bos_",
            Self::PersonalOs => "pos_",
            Self::DeveloperOs => "dos_",
        }
    }
}

/// Identity of one registered extension module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleManifest {
    pub namespace: ModuleNamespace,
    pub name: &'static str,
    pub version: &'static str,
}

/// One dispatched call to an extension module, after namespace routing.
#[derive(Clone, Debug)]
pub struct ModuleCall {
    pub name: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// Boxed future returned by [`ServerModule::handle`].
pub type ServerModuleFuture<'a> = Pin<Box<dyn Future<Output = Result<CallToolResult, ErrorData>> + Send + 'a>>;

/// An in-process extension module. Implementors receive core
/// capabilities through the injected [`ModuleContext`], which exposes no
/// raw filesystem path and no unlocked write; nothing in this trait forces
/// or encourages a module to reach the filesystem any other way (see the
/// module-level docs for what that guarantee does and does not cover).
pub trait ServerModule: Send + Sync {
    /// This module's identity and declared namespace.
    fn manifest(&self) -> ModuleManifest;

    /// The tools this module contributes. Every tool name must start with
    /// `self.manifest().namespace.prefix()`; the registry rejects any tool
    /// that does not at construction time.
    fn tools(&self) -> Vec<Tool>;

    /// Handles one call already routed to this module by tool name.
    fn handle<'a>(&'a self, call: ModuleCall, ctx: &'a ModuleContext) -> ServerModuleFuture<'a>;
}

/// Collects extension modules and validates their namespacing and tool-name
/// uniqueness before the server starts.
#[derive(Clone, Default)]
pub struct ModuleRegistry {
    modules: Vec<Arc<dyn ServerModule>>,
}

impl ModuleRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one module. Validation happens once, in [`Self::validate`],
    /// so registration order never matters.
    #[must_use]
    pub fn register(mut self, module: Arc<dyn ServerModule>) -> Self {
        self.modules.push(module);
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<dyn ServerModule>> {
        self.modules.iter()
    }

    /// Validates every registered module's namespace and rejects any tool
    /// name collision, either between two modules or with a reserved (core)
    /// name.
    ///
    /// # Errors
    ///
    /// Returns the first namespace or collision violation found.
    pub(crate) fn validate(&self, reserved: &HashSet<String>) -> Result<(), ModuleRegistryError> {
        let mut seen: HashSet<String> = reserved.clone();
        for module in &self.modules {
            let manifest = module.manifest();
            for tool in module.tools() {
                let tool_name = tool.name.into_owned();
                if !tool_name.starts_with(manifest.namespace.prefix()) {
                    return Err(ModuleRegistryError::UnnamespacedTool {
                        module: manifest.name.to_owned(),
                        tool_name,
                        prefix: manifest.namespace.prefix(),
                    });
                }
                if !seen.insert(tool_name.clone()) {
                    return Err(ModuleRegistryError::DuplicateTool { tool_name });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModuleRegistryError {
    #[error("module {module:?} tool {tool_name:?} is not namespaced under its declared prefix {prefix:?}")]
    UnnamespacedTool {
        module: String,
        tool_name: String,
        prefix: &'static str,
    },
    #[error("tool name {tool_name:?} is registered more than once")]
    DuplicateTool { tool_name: String },
}

impl ModuleRegistryError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnnamespacedTool { .. } => "module/unnamespaced-tool",
            Self::DuplicateTool { .. } => "module/duplicate-tool",
        }
    }
}

/// Per-call capabilities injected into an extension module: path validation,
/// read access, a lock-safe write pipeline that inherits logging,
/// versioning, and indexing exactly like a core tool, and read-only query
/// access. This type never exposes the raw filesystem or the write-lock
/// coordinator itself, so going through it never bypasses the shared
/// pipeline, but it is a convenient sanctioned path, not a sandbox: a
/// module's own code is free to import `std::fs` directly instead, the same
/// as any in-process Rust plugin trait (see the module-level docs).
#[derive(Clone)]
pub struct ModuleContext {
    pub(crate) roots: Arc<VaultSet>,
    pub(crate) reads: Arc<Filesystem>,
    pub(crate) mutations: Arc<RoutedMutationService>,
    pub(crate) writes: WriteCoordinator,
    pub(crate) search: Arc<Vec<Option<Arc<VaultSearchService>>>>,
}

#[derive(Debug, Error)]
pub enum ModuleContextError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    /// `WriteCoordinator::lock_roots` failed: either a root index did not
    /// fit `usize` on this platform, or no lock exists for the given root.
    /// Rendered as a string rather than a `#[from]`-preserving variant
    /// because the specific `ToolError` cases behind it are crate-private
    /// dispatch plumbing, not part of this public error type's surface;
    /// both currently map to the same `server/internal` code below. In
    /// practice this is unreachable: every [`VaultRootId`] a module ever
    /// holds came from [`ModuleContext::resolve_path`] against the live
    /// [`VaultSet`], so it is always in range.
    #[error("module write coordination failed: {0}")]
    Internal(String),
    #[error("blocking module task failed")]
    Join(#[from] tokio::task::JoinError),
}

impl ModuleContextError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Path(error) => error.code(),
            Self::Filesystem(error) => error.code(),
            Self::Internal(_) | Self::Join(_) => "server/internal",
        }
    }

    /// Actionable remediation suitable for an MCP error response, matching
    /// every core tool's own error shape.
    #[must_use]
    pub fn remediation(&self) -> &'static str {
        match self {
            Self::Path(error) => error.remediation(),
            Self::Filesystem(error) => error.remediation(),
            Self::Internal(_) | Self::Join(_) => {
                "Retry the operation and inspect server diagnostics if it fails again."
            }
        }
    }

    /// Renders this failure as the same structured, `is_error: true` tool
    /// result every core tool produces (stable `code`, human `message`, and
    /// a `remediation` hint) via `CallToolResult::structured_error`, so a
    /// module never has to hand-roll a less complete error envelope than
    /// the core catalogue provides (`.claude/rules/mcp-contracts.md`).
    #[must_use]
    pub fn into_tool_result(self) -> CallToolResult {
        let code = self.code();
        let remediation = self.remediation();
        let message = self.to_string();
        CallToolResult::structured_error(serde_json::json!({
            "code": code,
            "message": message,
            "remediation": remediation,
        }))
    }
}

impl ModuleContext {
    /// The configured vault roots, for resolving a module's own raw path
    /// arguments the same way every core tool does.
    #[must_use]
    pub fn roots(&self) -> &VaultSet {
        &self.roots
    }

    /// Validates a raw path argument against the configured roots.
    ///
    /// Runs on a blocking thread, like every core tool's own path
    /// validation: symlink resolution is real filesystem I/O and must never
    /// run on an async executor thread (`AGENTS.md`).
    ///
    /// # Errors
    ///
    /// Returns the same [`PathError`] a core tool would for an invalid,
    /// escaping, or ambiguous path, wrapped as a [`ModuleContextError`].
    pub async fn resolve_path(&self, raw: &str) -> Result<VaultPath, ModuleContextError> {
        let roots = Arc::clone(&self.roots);
        let raw = raw.to_owned();
        Ok(tokio::task::spawn_blocking(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw,
            })
        })
        .await??)
    }

    /// Reads text, returning `None` only when the path does not exist. Runs
    /// on a blocking thread; never call this from anywhere except an
    /// `async` context that can `.await` it.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed error for every failure other than
    /// absence.
    pub async fn read_optional_text(&self, path: &VaultPath) -> Result<Option<VaultText>, ModuleContextError> {
        let reads = Arc::clone(&self.reads);
        let path = path.clone();
        Ok(tokio::task::spawn_blocking(move || reads.read_optional_text(&path)).await??)
    }

    /// Lists direct children in deterministic name order. Runs on a
    /// blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the adapter's typed discovery error.
    pub async fn list(&self, path: &VaultPath) -> Result<Vec<VaultEntry>, ModuleContextError> {
        let reads = Arc::clone(&self.reads);
        let path = path.clone();
        Ok(tokio::task::spawn_blocking(move || reads.list(&path)).await??)
    }

    /// The read-only search service for one configured root, when search is
    /// enabled for that vault. Returning the reference itself is not
    /// blocking (it is a plain in-memory lookup), but every method on
    /// [`VaultSearchService`] does real `SQLite` or index I/O: call it only
    /// from inside `tokio::task::spawn_blocking`, exactly as every core
    /// `query_*` tool does (`server.rs`'s `execute`/`evaluate` helpers).
    #[must_use]
    pub fn search(&self, root: VaultRootId) -> Option<&VaultSearchService> {
        let index = usize::try_from(root).ok()?;
        self.search.get(index)?.as_ref().map(Arc::as_ref)
    }

    /// Persists one validated mutation through the shared, lock-serialised
    /// write pipeline: identical validation, logging, versioning, and
    /// indexing to every core write tool.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in
    /// the successful result's warnings.
    pub async fn write(&self, request: WriteMutation) -> Result<PipelineResult<WriteOutcome>, ModuleContextError> {
        let root = request.path.root_id();
        let mutations = Arc::clone(&self.mutations);
        Ok(self.locked(&[root], move || mutations.persist(&request)).await??)
    }

    /// Materialises historical content as a new forward mutation.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in
    /// the successful result's warnings.
    pub async fn restore(&self, request: RestoreMutation) -> Result<PipelineResult<WriteOutcome>, ModuleContextError> {
        let root = request.path.root_id();
        let mutations = Arc::clone(&self.mutations);
        Ok(self.locked(&[root], move || mutations.restore(&request)).await??)
    }

    /// Deletes a validated file or empty directory through the shared
    /// pipeline.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in
    /// the successful result's warnings.
    pub async fn delete(&self, request: DeleteMutation) -> Result<PipelineResult<DeleteOutcome>, ModuleContextError> {
        let root = request.path.root_id();
        let mutations = Arc::clone(&self.mutations);
        Ok(self.locked(&[root], move || mutations.delete(&request)).await??)
    }

    /// Appends one complete record through the shared pipeline, initialising
    /// an empty file if needed.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in
    /// the successful result's warnings.
    pub async fn append(&self, request: AppendMutation) -> Result<PipelineResult<AppendOutcome>, ModuleContextError> {
        let root = request.path.root_id();
        let mutations = Arc::clone(&self.mutations);
        Ok(self.locked(&[root], move || mutations.append(&request)).await??)
    }

    /// Moves one path through the shared pipeline without replacing an
    /// existing destination.
    ///
    /// # Errors
    ///
    /// Returns the primary persistence error. Secondary failures remain in
    /// the successful result's warnings.
    pub async fn move_path(&self, request: MoveMutation) -> Result<PipelineResult<MoveOutcome>, ModuleContextError> {
        let roots = [request.source.root_id(), request.destination.root_id()];
        let mutations = Arc::clone(&self.mutations);
        Ok(self.locked(&roots, move || mutations.move_path(&request)).await??)
    }

    /// Acquires the write lock for every affected root, then runs `task` on
    /// a blocking thread while holding the guards, exactly like every core
    /// mutating tool. This is the only path by which a module reaches the
    /// mutation service; nothing in this type exposes the coordinator or the
    /// service itself.
    async fn locked<T, F>(&self, roots: &[VaultRootId], task: F) -> Result<T, ModuleContextError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let guards = self
            .writes
            .lock_roots(roots)
            .await
            .map_err(|error| ModuleContextError::Internal(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _guards = guards;
            task()
        })
        .await
        .map_err(ModuleContextError::Join)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ModuleCall, ModuleContext, ModuleManifest, ModuleNamespace, ModuleRegistry, ModuleRegistryError, ServerModule,
        ServerModuleFuture, Tool,
    };

    struct FixedModule {
        manifest: ModuleManifest,
        tool_names: Vec<&'static str>,
    }

    impl ServerModule for FixedModule {
        fn manifest(&self) -> ModuleManifest {
            self.manifest.clone()
        }

        fn tools(&self) -> Vec<Tool> {
            self.tool_names
                .iter()
                .map(|name| Tool::new((*name).to_owned(), "fixture tool", serde_json::Map::new()))
                .collect()
        }

        fn handle<'a>(&'a self, _call: ModuleCall, _ctx: &'a ModuleContext) -> ServerModuleFuture<'a> {
            Box::pin(async { unreachable!("fixture module handle is not exercised by these tests") })
        }
    }

    fn developer_os(tool_names: &[&'static str]) -> std::sync::Arc<dyn ServerModule> {
        std::sync::Arc::new(FixedModule {
            manifest: ModuleManifest {
                namespace: ModuleNamespace::DeveloperOs,
                name: "fixture",
                version: "0.0.0",
            },
            tool_names: tool_names.to_vec(),
        })
    }

    #[test]
    fn a_correctly_namespaced_module_validates_cleanly() {
        let registry = ModuleRegistry::new().register(developer_os(&["dos_one", "dos_two"]));
        assert!(registry.validate(&std::collections::HashSet::new()).is_ok());
    }

    #[test]
    fn a_tool_name_outside_its_modules_namespace_is_rejected() {
        let registry = ModuleRegistry::new().register(developer_os(&["pos_intruder"]));
        assert_eq!(
            registry.validate(&std::collections::HashSet::new()),
            Err(ModuleRegistryError::UnnamespacedTool {
                module: "fixture".to_owned(),
                tool_name: "pos_intruder".to_owned(),
                prefix: "dos_",
            })
        );
    }

    #[test]
    fn unnamespaced_tool_error_carries_the_stable_code() {
        let error = ModuleRegistryError::UnnamespacedTool {
            module: "fixture".to_owned(),
            tool_name: "pos_intruder".to_owned(),
            prefix: "dos_",
        };
        assert_eq!(error.code(), "module/unnamespaced-tool");
    }

    #[test]
    fn two_modules_registering_the_same_tool_name_collide() {
        let registry = ModuleRegistry::new()
            .register(developer_os(&["dos_shared"]))
            .register(developer_os(&["dos_shared"]));
        assert_eq!(
            registry.validate(&std::collections::HashSet::new()),
            Err(ModuleRegistryError::DuplicateTool {
                tool_name: "dos_shared".to_owned(),
            })
        );
    }

    #[test]
    fn duplicate_tool_error_carries_the_stable_code() {
        let error = ModuleRegistryError::DuplicateTool {
            tool_name: "dos_shared".to_owned(),
        };
        assert_eq!(error.code(), "module/duplicate-tool");
    }

    #[test]
    fn a_module_tool_colliding_with_a_reserved_core_name_is_rejected() {
        let registry = ModuleRegistry::new().register(developer_os(&["dos_reserved"]));
        let reserved = std::collections::HashSet::from(["dos_reserved".to_owned()]);
        assert_eq!(
            registry.validate(&reserved),
            Err(ModuleRegistryError::DuplicateTool {
                tool_name: "dos_reserved".to_owned(),
            })
        );
    }

    #[test]
    fn an_empty_registry_validates_against_any_reserved_set() {
        let registry = ModuleRegistry::new();
        let reserved = std::collections::HashSet::from(["fs_write_file".to_owned()]);
        assert!(registry.validate(&reserved).is_ok());
    }
}
