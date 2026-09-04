use std::fmt::{self, Display, Formatter};

use contextos_core::{VaultRootId, VaultSet};
use contextos_search::{RebuildTarget, SearchError, VaultSearchConfig, VaultSearchService};
use thiserror::Error;

use crate::server::semantic_config;
use crate::{Config, ConfigError};

/// A read-only-to-the-vault-config, write-to-derived-state report of one
/// `contextos index` run: rebuilds every enabled search index for every
/// configured vault and records a per-vault actionable summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexReport {
    checks: Vec<IndexCheck>,
}

impl IndexReport {
    /// Reports whether any vault requires operator action.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|check| check.status == IndexStatus::Fail)
    }
}

impl TryFrom<&Config> for IndexReport {
    type Error = IndexCliError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let roots = VaultSet::try_from(value)?;
        let mut checks = Vec::with_capacity(value.vaults.len());

        for (index, (vault, root)) in value.vaults.iter().zip(roots.iter()).enumerate() {
            let enabled = vault.search.text || vault.search.graph || vault.search.semantic;
            if !vault.managed || !enabled {
                checks.push(IndexCheck {
                    subject: format!("Vault {}", root.path().display()),
                    status: IndexStatus::Pass,
                    message: "search indexing disabled".to_owned(),
                    action: None,
                });
                continue;
            }

            let root_id = VaultRootId::try_from(index)?;
            let state_directory =
                match crate::state_dir::resolve_state_directory(vault.state_directory.as_deref(), root.path()) {
                    Ok(state_directory) => state_directory,
                    Err(error) => {
                        checks.push(IndexCheck {
                            subject: format!("Vault {}", root.path().display()),
                            status: IndexStatus::Fail,
                            message: error.to_string(),
                            action: Some(
                                "Set [[vault]] state_directory explicitly, and rerun `contextos index`.".to_owned(),
                            ),
                        });
                        continue;
                    }
                };
            let service = semantic_config(vault, &state_directory).and_then(|semantic| {
                VaultSearchService::try_from(VaultSearchConfig {
                    root_id,
                    root: root.path().to_path_buf(),
                    excludes: vault.search.exclude.clone(),
                    state_directory,
                    text_enabled: vault.search.text,
                    graph_enabled: vault.search.graph,
                    graph_backend: vault.search.graph_backend.into(),
                    semantic,
                })
            });
            let service = match service {
                Ok(service) => service,
                Err(error) => {
                    checks.push(IndexCheck {
                        subject: format!("Vault {}", root.path().display()),
                        status: IndexStatus::Fail,
                        message: error.to_string(),
                        action: Some("Correct the vault.search configuration, and rerun `contextos index`.".to_owned()),
                    });
                    continue;
                }
            };

            checks.push(rebuild_check(root, &service));
        }

        Ok(Self { checks })
    }
}

fn rebuild_check(root: &contextos_core::VaultRoot, service: &VaultSearchService) -> IndexCheck {
    match service.rebuild(RebuildTarget::All) {
        Ok(report) => {
            let text = report.text.map_or_else(
                || "text: disabled".to_owned(),
                |report| {
                    format!(
                        "text: {} scanned, {} reindexed, {} removed",
                        report.scanned, report.reindexed, report.removed
                    )
                },
            );
            let graph = report.graph.map_or_else(
                || "graph: disabled".to_owned(),
                |report| {
                    format!(
                        "graph: {} notes scanned, {} nodes, {} edges",
                        report.notes_scanned, report.nodes, report.edges
                    )
                },
            );
            let semantic = report.semantic.map_or_else(
                || "semantic: disabled".to_owned(),
                |report| {
                    format!(
                        "semantic: {} paths scanned, {} chunks embedded, {} skipped, {} failed",
                        report.paths_scanned, report.embedded, report.skipped, report.failed
                    )
                },
            );
            IndexCheck {
                subject: format!("Vault {}", root.path().display()),
                status: IndexStatus::Pass,
                message: format!("{text}; {graph}; {semantic}"),
                action: None,
            }
        }
        Err(error) => IndexCheck {
            subject: format!("Vault {}", root.path().display()),
            status: IndexStatus::Fail,
            message: error.to_string(),
            action: Some("Resolve the reported index error, and rerun `contextos index`.".to_owned()),
        },
    }
}

impl Display for IndexReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP index")?;
        for check in &self.checks {
            writeln!(formatter, "{} | {} | {}", check.subject, check.status, check.message)?;
            if let Some(action) = &check.action {
                writeln!(formatter, "  Action: {action}")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexCheck {
    subject: String,
    status: IndexStatus,
    message: String,
    action: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexStatus {
    Pass,
    Fail,
}

impl Display for IndexStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => formatter.write_str("PASS"),
            Self::Fail => formatter.write_str("FAIL"),
        }
    }
}

/// Typed failures that prevent `contextos index` from rebuilding every
/// configured vault's search indexes.
#[derive(Debug, Error)]
pub enum IndexCliError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Path(#[from] contextos_core::PathError),
    #[error(transparent)]
    Search(#[from] SearchError),
}
