//! `git_init`, `git_commit`, `git_restore`, `git_status`, `git_log`, and
//! `git_diff`: recoverable local Git history over a vault's MCP-owned
//! writes, plus the `git_service`/`git_filter_path` helpers shared by
//! all six tools.

use std::sync::Arc;

use contextos_core::{Clock, SystemClock, VaultPath, VaultPathInput, VaultRoot};
use contextos_fs::{FilesystemService, FilesystemServiceConfig};
use contextos_git::{
    GitCommitResult, GitDiffRequest, GitDiffResult, GitInitResult, GitLogEntry, GitLogRequest, GitRestoreRequest,
    GitRestoreResult, GitStatusResult,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::schemars;
use rmcp::tool;
use serde::{Deserialize, Serialize};

use crate::resource_support::fallible_output_schema_for;
use crate::server::{ContextOsServer, ManagedGit};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};

#[rmcp::tool_router(router = git_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "git_init",
        description = "Initialise recoverable local Git history for a vault",
        output_schema = fallible_output_schema_for::<GitInitToolResult>()
    )]
    async fn initialise_git(
        &self,
        Parameters(input): Parameters<GitVaultInput>,
    ) -> Result<Json<GitInitToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let git = Arc::clone(&self.git);
        let path = evaluate(move || {
            VaultPath::try_from_vault_selector(&roots, input.vault.as_deref().unwrap_or(".")).map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = git
            .get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::GitDisabled))?;
        let writer = FilesystemService::from(FilesystemServiceConfig {
            filesystem: self.filesystem.as_ref().clone(),
            clock: SystemClock,
        });
        let guards = self.writes.lock_roots(&[path.root_id()]).await?;
        execute(move || {
            let _guards = guards;
            Ok(GitInitToolResult::from(service.initialise(&writer)?))
        })
        .await
    }

    #[tool(
        name = "git_commit",
        description = "Commit all pending MCP-owned staged paths immediately",
        output_schema = fallible_output_schema_for::<GitCommitToolResult>()
    )]
    async fn commit_git(
        &self,
        Parameters(input): Parameters<GitCommitInput>,
    ) -> Result<Json<GitCommitToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let git = Arc::clone(&self.git);
        let path = evaluate(move || {
            VaultPath::try_from_vault_selector(&roots, input.vault.as_deref().unwrap_or(".")).map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = git
            .get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::GitDisabled))?;
        let guards = self.writes.lock_roots(&[path.root_id()]).await?;
        execute(move || {
            let _guards = guards;
            Ok(GitCommitToolResult::from(service.commit(input.message.as_deref())?))
        })
        .await
    }

    #[tool(
        name = "git_restore",
        description = "Restore historical content as new forward mutations without rewriting history",
        output_schema = fallible_output_schema_for::<GitRestoreToolResult>()
    )]
    async fn restore_git(
        &self,
        Parameters(input): Parameters<GitRestoreInput>,
    ) -> Result<Json<GitRestoreToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let git = Arc::clone(&self.git);
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = git
            .get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::GitDisabled))?;
        let guards = self.writes.lock_roots(&[path.root_id()]).await?;
        let writer = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            Ok(GitRestoreToolResult::from(service.restore(
                &GitRestoreRequest {
                    path,
                    reference: input.reference,
                    dry_run: input.dry_run,
                },
                writer.as_ref(),
            )?))
        })
        .await
    }

    #[tool(
        name = "git_status",
        description = "Report branch, staged, unstaged, untracked, and pending MCP paths",
        output_schema = fallible_output_schema_for::<GitStatusToolResult>()
    )]
    async fn git_status(
        &self,
        Parameters(input): Parameters<GitVaultInput>,
    ) -> Result<Json<GitStatusToolResult>, ToolFailure> {
        let (service, index, _) = self.git_service(input.vault.as_deref()).await?;
        let debounce = self.git_debounce_seconds.get(index).copied().unwrap_or(30);
        execute(move || {
            let status = service.status()?;
            let countdown = status.staged_at_unix.map(|staged_at| {
                let elapsed = SystemClock.now().unix_timestamp().saturating_sub(staged_at);
                debounce.saturating_sub(u64::try_from(elapsed).unwrap_or(u64::MAX))
            });
            Ok(GitStatusToolResult::from((status, countdown)))
        })
        .await
    }

    #[tool(
        name = "git_log",
        description = "Read local commit history with an optional path filter",
        output_schema = fallible_output_schema_for::<GitLogToolResult>()
    )]
    async fn git_log(&self, Parameters(input): Parameters<GitLogInput>) -> Result<Json<GitLogToolResult>, ToolFailure> {
        let (service, index, root) = self.git_service(input.vault.as_deref()).await?;
        let path = match input.path {
            Some(raw) => Some(self.git_filter_path(index, root, raw).await?),
            None => None,
        };
        execute(move || {
            Ok(GitLogToolResult::from(service.log(&GitLogRequest {
                path,
                limit: usize::try_from(input.limit)?,
            })?))
        })
        .await
    }

    #[tool(
        name = "git_diff",
        description = "Return a size-capped unified diff between refs or the working tree",
        output_schema = fallible_output_schema_for::<GitDiffToolResult>()
    )]
    async fn git_diff(
        &self,
        Parameters(input): Parameters<GitDiffInput>,
    ) -> Result<Json<GitDiffToolResult>, ToolFailure> {
        let (service, index, root) = self.git_service(input.vault.as_deref()).await?;
        let path = match input.path {
            Some(raw) => Some(self.git_filter_path(index, root, raw).await?),
            None => None,
        };
        execute(move || {
            Ok(GitDiffToolResult::from(service.diff(&GitDiffRequest {
                from: input.from,
                to: input.to,
                path,
                max_bytes: 1024 * 1024,
            })?))
        })
        .await
    }
}

impl ContextOsServer {
    pub(crate) async fn git_service(&self, vault: Option<&str>) -> Result<(ManagedGit, usize, VaultRoot), ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let raw = vault.unwrap_or(".").to_owned();
        let path = evaluate(move || VaultPath::try_from_vault_selector(&roots, &raw).map_err(ToolError::from)).await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let root = self
            .roots
            .iter()
            .nth(index)
            .cloned()
            .ok_or_else(|| ToolFailure::from(ToolError::RootLockMissing { root_index: index }))?;
        self.git
            .get(index)
            .cloned()
            .flatten()
            .map(|service| (service, index, root))
            .ok_or_else(|| ToolFailure::from(ToolError::GitDisabled))
    }

    pub(crate) async fn git_filter_path(
        &self,
        root_index: usize,
        root: VaultRoot,
        raw: String,
    ) -> Result<std::path::PathBuf, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        evaluate(move || {
            let supplied = std::path::Path::new(&raw);
            let candidate = if supplied.is_absolute() {
                supplied.to_path_buf()
            } else {
                root.path().join(supplied)
            };
            let text = candidate
                .to_str()
                .ok_or(ToolError::Invalid("Git filter path must be valid UTF-8"))?;
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: text,
            })?;
            if usize::try_from(path.root_id())? != root_index {
                return Err(ToolError::Invalid("Git filter path must belong to the selected vault"));
            }
            Ok(path.relative().to_path_buf())
        })
        .await
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitVaultInput {
    /// The vault to operate on: a configured vault's bare name,
    /// or a path/`{name}://{relative-path}`; omit to use the
    /// sole configured vault.
    pub(crate) vault: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitCommitInput {
    /// The vault to operate on: a configured vault's bare name,
    /// or a path/`{name}://{relative-path}`; omit to use the
    /// sole configured vault.
    vault: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitRestoreInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    path: String,
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitLogInput {
    /// The vault to operate on: a configured vault's bare name,
    /// or a path/`{name}://{relative-path}`; omit to use the
    /// sole configured vault.
    vault: Option<String>,
    /// Restrict results to this path within the selected `vault`
    /// (relative to that vault's root, or absolute); the
    /// `{name}://{relative-path}` prefix is not accepted here
    /// because the vault is already chosen by `vault`.
    path: Option<String>,
    #[serde(default = "default_git_log_limit")]
    limit: u64,
}

const fn default_git_log_limit() -> u64 {
    20
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GitDiffInput {
    /// The vault to operate on: a configured vault's bare name,
    /// or a path/`{name}://{relative-path}`; omit to use the
    /// sole configured vault.
    vault: Option<String>,
    from: Option<String>,
    to: Option<String>,
    /// Restrict the diff to this path within the selected `vault`
    /// (relative to that vault's root, or absolute); the
    /// `{name}://{relative-path}` prefix is not accepted here
    /// because the vault is already chosen by `vault`.
    path: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitInitToolResult {
    initialised: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    commit_id: Option<String>,
}

impl From<GitInitResult> for GitInitToolResult {
    fn from(value: GitInitResult) -> Self {
        Self {
            initialised: value.initialised,
            commit_id: value.commit_id,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitCommitToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    commit_id: Option<String>,
    committed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    message: Option<String>,
}

impl From<GitCommitResult> for GitCommitToolResult {
    fn from(value: GitCommitResult) -> Self {
        Self {
            commit_id: value.commit_id,
            committed_paths: value
                .committed_paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            message: value.message,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitRestoreToolResult {
    diff: String,
    applied: bool,
    warnings: Vec<String>,
}

impl From<GitRestoreResult> for GitRestoreToolResult {
    fn from(value: GitRestoreResult) -> Self {
        let crate::server::WarningMessages(warnings) = crate::server::WarningMessages::from(value.warnings);
        Self {
            diff: value.diff,
            applied: value.applied,
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitStatusToolResult {
    branch: String,
    ahead: usize,
    behind: usize,
    staged: Vec<String>,
    unstaged: Vec<String>,
    untracked: Vec<String>,
    pending_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_u64_schema")]
    seconds_until_auto_commit: Option<u64>,
}

impl From<(GitStatusResult, Option<u64>)> for GitStatusToolResult {
    fn from(value: (GitStatusResult, Option<u64>)) -> Self {
        Self {
            branch: value.0.branch,
            ahead: value.0.ahead,
            behind: value.0.behind,
            staged: value.0.staged,
            unstaged: value.0.unstaged,
            untracked: value.0.untracked,
            pending_paths: value.0.pending_paths,
            seconds_until_auto_commit: value.1,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitLogEntryToolResult {
    id: String,
    short_id: String,
    time: i64,
    message: String,
    files_changed: Vec<String>,
}

impl From<GitLogEntry> for GitLogEntryToolResult {
    fn from(value: GitLogEntry) -> Self {
        Self {
            id: value.id,
            short_id: value.short_id,
            time: value.time,
            message: value.message,
            files_changed: value.files_changed,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitLogToolResult {
    entries: Vec<GitLogEntryToolResult>,
}

impl From<Vec<GitLogEntry>> for GitLogToolResult {
    fn from(value: Vec<GitLogEntry>) -> Self {
        Self {
            entries: value.into_iter().map(GitLogEntryToolResult::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GitDiffToolResult {
    content: String,
    truncated: bool,
}

impl From<GitDiffResult> for GitDiffToolResult {
    fn from(value: GitDiffResult) -> Self {
        Self {
            content: value.content,
            truncated: value.truncated,
        }
    }
}
