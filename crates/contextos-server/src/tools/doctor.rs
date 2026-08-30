//! `doctor` and `doctor_resolve`: a schema-valid
//! MCP equivalent of `contextos doctor`, reusing
//! [`crate::doctor::DoctorReport`] unchanged so diagnostic content is
//! identical to the CLI, plus a narrowly scoped remediation tool that acts
//! only on findings `doctor` itself classifies `auto_fixable`.

use std::sync::Arc;

use contextos_core::{
    Origin, SystemClock, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultSet,
};
use contextos_fs::{Filesystem, FilesystemService, FilesystemServiceConfig};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};

use crate::Config;
use crate::doctor::{DoctorCheck, DoctorError, DoctorReport, DoctorStatus};
use crate::resource_support::fallible_output_schema_for;
use crate::server::{ContextOsServer, ManagedGit, ManagedIndexService};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};

#[rmcp::tool_router(router = doctor_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "doctor",
        description = "Report actionable, read-only health checks for the effective configuration: configuration validity, per-vault reachability, managed index staleness, Git recovery state, and semantic search health. Never writes to a vault; each check reports whether doctor_resolve may act on it automatically. Identical diagnostic content to the contextos doctor CLI command.",
        output_schema = fallible_output_schema_for::<DoctorToolResult>()
    )]
    async fn doctor(&self) -> Result<Json<DoctorToolResult>, ToolFailure> {
        let config = Arc::clone(&self.config);
        execute(move || {
            DoctorReport::try_from(config.as_ref())
                .map(DoctorToolResult::from)
                .map_err(ToolError::from)
        })
        .await
    }

    #[tool(
        name = "doctor_resolve",
        description = "Resolve every currently auto-fixable doctor finding (a stale or missing managed index, or an absent Git repository) by calling the same remediation tool doctor names for it -- vault_index_rebuild or git_init -- scoped to one configured vault via path, or every configured vault when path is omitted. Never acts on a finding doctor classifies as not auto-fixable; those always require an operator decision. Initialising Git for a vault that previously had none is a standing behavioural change, not a one-off fix: it starts automatic commits on every future mutation. Set dry_run to preview which findings would be acted on without writing anything.",
        output_schema = fallible_output_schema_for::<DoctorResolveToolResult>()
    )]
    async fn doctor_resolve(
        &self,
        Parameters(input): Parameters<DoctorResolveInput>,
    ) -> Result<Json<DoctorResolveToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let target_vault_index = if let Some(raw) = input.path {
            let roots = Arc::clone(&roots);
            let path = evaluate(move || {
                VaultPath::try_from_vault_selector(&roots, &raw).map_err(ToolError::from)
            })
            .await?;
            Some(usize::try_from(path.root_id()).map_err(ToolError::from)?)
        } else {
            None
        };

        let lock_targets = match target_vault_index {
            Some(index) => vec![index],
            None => (0..roots.len()).collect(),
        };
        let root_ids = lock_targets
            .iter()
            .copied()
            .map(|index| VaultRootId::try_from(index).map_err(ToolError::from))
            .collect::<Result<Vec<_>, _>>()?;
        let guards = self.writes.lock_roots(&root_ids).await?;

        let config = Arc::clone(&self.config);
        let indexes = Arc::clone(&self.indexes);
        let git = Arc::clone(&self.git);
        let filesystem = Arc::clone(&self.filesystem);
        let dry_run = input.dry_run;

        execute(move || {
            let _guards = guards;
            resolve_doctor_findings(&ResolveContext {
                config: config.as_ref(),
                roots: roots.as_ref(),
                indexes: indexes.as_ref(),
                git: git.as_ref(),
                filesystem: filesystem.as_ref(),
                target_vault_index,
                dry_run,
            })
            .map_err(ToolError::from)
        })
        .await
    }
}

struct ResolveContext<'a> {
    config: &'a Config,
    roots: &'a VaultSet,
    indexes: &'a [Option<ManagedIndexService>],
    git: &'a [Option<ManagedGit>],
    filesystem: &'a Filesystem,
    target_vault_index: Option<usize>,
    dry_run: bool,
}

fn resolve_doctor_findings(
    ctx: &ResolveContext<'_>,
) -> Result<DoctorResolveToolResult, DoctorError> {
    let before = DoctorReport::try_from(ctx.config)?;
    let mut outcomes = Vec::new();
    let mut wrote = false;

    for check in &before.checks {
        if !check.auto_fixable {
            continue;
        }
        let Some(vault_index) = check.vault_index else {
            continue;
        };
        if ctx
            .target_vault_index
            .is_some_and(|target| target != vault_index)
        {
            continue;
        }
        let Some(root) = ctx.roots.iter().nth(vault_index) else {
            continue;
        };

        let outcome = match check.remediation_tool {
            Some("vault_index_rebuild") => resolve_stale_index(ctx, vault_index, root)?,
            Some("git_init") => resolve_absent_git_repository(ctx, vault_index, root),
            _ => continue,
        };
        wrote |= outcome.resolved;
        outcomes.push(outcome);
    }

    let report = if wrote {
        DoctorReport::try_from(ctx.config)?
    } else {
        before
    };
    Ok(DoctorResolveToolResult {
        outcomes,
        report: DoctorToolResult::from(report),
    })
}

fn resolve_stale_index(
    ctx: &ResolveContext<'_>,
    vault_index: usize,
    root: &VaultRoot,
) -> Result<DoctorResolveOutcome, DoctorError> {
    let vault_path = vault_path_for(ctx.roots, root)?;
    if ctx.dry_run {
        return Ok(DoctorResolveOutcome {
            subject: "Managed indexes".to_owned(),
            vault: root.path().display().to_string(),
            resolved: false,
            message: "would call vault_index_rebuild".to_owned(),
        });
    }
    let Some(service) = ctx.indexes.get(vault_index).cloned().flatten() else {
        return Ok(DoctorResolveOutcome {
            subject: "Managed indexes".to_owned(),
            vault: root.path().display().to_string(),
            resolved: false,
            message: "managed indexes are unavailable for this vault".to_owned(),
        });
    };
    let report = service.rebuild_report(
        &vault_path,
        Origin::Tool("doctor_resolve".to_owned()),
        false,
    );
    let message = match report {
        Ok(report) => format!(
            "vault_index_rebuild created {} and updated {} indexes",
            report.indexes_created, report.indexes_updated
        ),
        Err(error) => error.to_string(),
    };
    Ok(DoctorResolveOutcome {
        subject: "Managed indexes".to_owned(),
        vault: root.path().display().to_string(),
        resolved: true,
        message,
    })
}

fn resolve_absent_git_repository(
    ctx: &ResolveContext<'_>,
    vault_index: usize,
    root: &VaultRoot,
) -> DoctorResolveOutcome {
    if ctx.dry_run {
        return DoctorResolveOutcome {
            subject: "Git recovery".to_owned(),
            vault: root.path().display().to_string(),
            resolved: false,
            message: "would call git_init".to_owned(),
        };
    }
    let Some(service) = ctx.git.get(vault_index).cloned().flatten() else {
        return DoctorResolveOutcome {
            subject: "Git recovery".to_owned(),
            vault: root.path().display().to_string(),
            resolved: false,
            message: "Git recovery is unavailable for this vault".to_owned(),
        };
    };
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: ctx.filesystem.clone(),
        clock: SystemClock,
    });
    let message = match service.initialise(&writer) {
        Ok(_) => "git_init initialised the repository".to_owned(),
        Err(error) => error.to_string(),
    };
    DoctorResolveOutcome {
        subject: "Git recovery".to_owned(),
        vault: root.path().display().to_string(),
        resolved: true,
        message,
    }
}

/// CLI entry point for `contextos doctor --resolve`: resolves
/// every auto-fixable finding across every vault the server was built
/// from, reusing the exact same dispatch `doctor_resolve` uses over MCP,
/// so CLI and MCP stay behaviourally identical for the auto-fixable set.
/// Public (and re-exported from the crate root, alongside `DoctorReport`)
/// for the same reason `DoctorReport` is: `main.rs` is a separate binary
/// crate, not a descendant module, so only genuinely `pub` items are
/// reachable from it.
///
/// # Errors
///
/// Returns [`DoctorError`] when the effective configuration cannot be
/// assessed at all (for example, a vault root that disappeared between
/// server start and this call). A remediation failing for one finding is
/// not an error here: it is reported as an unresolved
/// [`DoctorResolveOutcome`] alongside every other finding.
pub fn resolve_for_cli(
    server: &ContextOsServer,
    dry_run: bool,
) -> Result<Vec<DoctorResolveOutcome>, DoctorError> {
    resolve_doctor_findings(&ResolveContext {
        config: server.config.as_ref(),
        roots: server.roots.as_ref(),
        indexes: server.indexes.as_ref(),
        git: server.git.as_ref(),
        filesystem: server.filesystem.as_ref(),
        target_vault_index: None,
        dry_run,
    })
    .map(|result| result.outcomes)
}

fn vault_path_for(roots: &VaultSet, root: &VaultRoot) -> Result<VaultPath, DoctorError> {
    let raw = root
        .path()
        .to_str()
        .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorResolveInput {
    /// Scope resolution to one configured vault: a configured vault's
    /// bare name, or a path/`{name}://{relative-path}`; omit to resolve
    /// every auto-fixable finding across every configured vault.
    path: Option<String>,
    /// Report what would be resolved without writing anything.
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorResolveToolResult {
    outcomes: Vec<DoctorResolveOutcome>,
    report: DoctorToolResult,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DoctorResolveOutcome {
    pub subject: String,
    pub vault: String,
    pub resolved: bool,
    pub message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorToolResult {
    checks: Vec<DoctorCheckToolResult>,
    has_failures: bool,
}

impl From<DoctorReport> for DoctorToolResult {
    fn from(value: DoctorReport) -> Self {
        let has_failures = value.has_failures();
        Self {
            checks: value
                .checks
                .iter()
                .map(DoctorCheckToolResult::from)
                .collect(),
            has_failures,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DoctorCheckToolResult {
    subject: String,
    status: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    action: Option<String>,
    /// Whether `doctor_resolve` may act on this finding without
    /// operator confirmation.
    auto_fixable: bool,
    /// The MCP tool `doctor_resolve` dispatches to when `auto_fixable` is
    /// `true`. Omitted (never `null`) when `auto_fixable` is `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    remediation_tool: Option<String>,
}

impl From<&DoctorCheck> for DoctorCheckToolResult {
    fn from(value: &DoctorCheck) -> Self {
        Self {
            subject: value.subject.clone(),
            status: match value.status {
                DoctorStatus::Pass => "pass".to_owned(),
                DoctorStatus::Fail => "fail".to_owned(),
            },
            message: value.message.clone(),
            action: value.action.clone(),
            auto_fixable: value.auto_fixable,
            remediation_tool: value.remediation_tool.map(str::to_owned),
        }
    }
}
