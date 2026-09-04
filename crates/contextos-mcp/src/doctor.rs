use std::fmt::{self, Display, Formatter};

use contextos_core::{Origin, SystemClock, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits, ReadTextRequest,
    SearchFilesRequest,
};
use contextos_git::{Git2Vault, Git2VaultConfig};
use contextos_index::{IndexService, IndexServiceConfig};
use contextos_obsidian::FrontmatterDocument;
use contextos_search::{GraphBackend, SearchError, VaultSearchConfig, VaultSearchService};
use thiserror::Error;

use crate::server::semantic_config;
use crate::{Config, ConfigError};

/// A read-only, actionable assessment of one effective server configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub(crate) checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Reports whether any check requires operator action before normal service.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|check| check.status == DoctorStatus::Fail)
    }
}

impl TryFrom<&Config> for DoctorReport {
    type Error = DoctorError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let roots = VaultSet::try_from(value)?;
        let limits = value
            .vaults
            .iter()
            .map(|vault| {
                Ok(FsLimits {
                    max_read_bytes: vault
                        .limits
                        .max_read_mb
                        .checked_mul(1024 * 1024)
                        .ok_or(DoctorError::ReadLimitOverflow)?,
                    max_batch_files: vault.limits.max_batch_files,
                })
            })
            .collect::<Result<Vec<_>, DoctorError>>()?;
        let hidden = value
            .vaults
            .iter()
            .map(|vault| vault.hidden.clone())
            .collect::<Vec<_>>();
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits,
            hidden,
            atomic_write_guard: None,
        })?;
        let writer = FilesystemService::from(FilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: SystemClock,
        });
        let mut checks = vec![DoctorCheck::pass("Configuration", "valid effective configuration")];

        for (index, (vault, root)) in value.vaults.iter().zip(roots.iter()).enumerate() {
            checks.push(
                DoctorCheck::pass(
                    format!("Vault {}", root.path().display()),
                    if vault.managed {
                        "accessible managed vault"
                    } else {
                        "accessible filesystem-only vault"
                    },
                )
                .for_vault(index),
            );
            if !vault.managed {
                checks.push(DoctorCheck::pass("Managed indexes", "disabled").for_vault(index));
                checks.push(DoctorCheck::pass("Git recovery", "disabled").for_vault(index));
                continue;
            }

            checks.push(index_check(vault, root, &roots, &filesystem, &writer)?.for_vault(index));
            checks.push(git_check(vault, root, &roots).for_vault(index));
            checks.push(semantic_check(vault, root)?.for_vault(index));
            checks.push(frontmatter_check(root, &roots, &filesystem)?.for_vault(index));
        }
        Ok(Self { checks })
    }
}

type DoctorIndexService = IndexService<Filesystem, FilesystemService<SystemClock>>;

fn index_check(
    vault: &crate::VaultConfig,
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
    writer: &FilesystemService<SystemClock>,
) -> Result<DoctorCheck, DoctorError> {
    if !vault.index_md.enabled {
        return Ok(DoctorCheck::pass("Managed indexes", "disabled"));
    }
    let path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: root
            .path()
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?,
    })?;
    let service = DoctorIndexService::try_from(IndexServiceConfig {
        root: root.clone(),
        roots: roots.clone(),
        reader: filesystem.clone(),
        writer: writer.clone(),
        excluded: vault.index_md.exclude.clone(),
    });
    let report = match service {
        Ok(service) => service.rebuild_report(&path, Origin::Tool("doctor".to_owned()), true),
        Err(error) => {
            return Ok(DoctorCheck::fail(
                "Managed indexes",
                error.to_string(),
                "Correct the index configuration, and call vault_index_rebuild again.",
            ));
        }
    };
    match report {
        Ok(report) if report.indexes_created == 0 && report.indexes_updated == 0 => Ok(DoctorCheck::pass(
            "Managed indexes",
            format!("{} directories are current", report.directories_scanned),
        )),
        Ok(report) => Ok(DoctorCheck::fail_auto_fixable(
            "Managed indexes",
            format!(
                "{} indexes are missing and {} are stale",
                report.indexes_created, report.indexes_updated
            ),
            "Call vault_index_rebuild for this vault, then run contextos doctor again.",
            "vault_index_rebuild",
        )),
        Err(error) => Ok(DoctorCheck::fail(
            "Managed indexes",
            error.to_string(),
            "Resolve the reported index conflict, call vault_index_rebuild, and rerun the doctor.",
        )),
    }
}

fn git_check(vault: &crate::VaultConfig, root: &contextos_core::VaultRoot, roots: &VaultSet) -> DoctorCheck {
    if !vault.git.enabled {
        return DoctorCheck::pass("Git recovery", "disabled");
    }
    let mut protected_restore_paths = vault.git.restore_exclude.clone();
    if vault.oplog.enabled && !protected_restore_paths.contains(&vault.oplog.path) {
        protected_restore_paths.push(vault.oplog.path.clone());
    }
    let service = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: roots.clone(),
        clock: SystemClock,
        author_name: vault.git.author_name.clone(),
        author_email: vault.git.author_email.clone(),
        allow_destructive_restore: vault.git.destructive_delete,
        protected_restore_paths,
    });
    let service = match service {
        Ok(service) => service,
        Err(error) => {
            return DoctorCheck::fail(
                "Git recovery",
                error.to_string(),
                "Correct the vault.git configuration, and rerun the doctor.",
            );
        }
    };
    if !service.is_repository() {
        return DoctorCheck::fail_auto_fixable(
            "Git recovery",
            "enabled, but the vault is not a Git repository",
            "Call git_init for this vault, then rerun the doctor. This starts \
             automatic commits on every future mutation to this vault, a \
             standing behavioural change, not just a one-off fix.",
            "git_init",
        );
    }
    match service.status() {
        Ok(status) => DoctorCheck::pass(
            "Git recovery",
            format!(
                "repository is readable on {} with {} pending paths",
                status.branch,
                status.pending_paths.len()
            ),
        ),
        Err(error) => DoctorCheck::fail(
            "Git recovery",
            error.to_string(),
            "Inspect the local Git repository, repair it without rewriting shared history, and rerun the doctor.",
        ),
    }
}

/// Reports the semantic index's provider configuration, model presence,
/// and vector store health, by
/// constructing the same `VaultSearchService` the real server would for
/// this vault (text and graph left disabled, since those are already
/// covered by [`index_check`]) and reading its status. Model acquisition
/// itself (the pre-fetch tool) is separate; this check only reports whether
/// the configured directory currently holds a usable model.
fn semantic_check(vault: &crate::VaultConfig, root: &contextos_core::VaultRoot) -> Result<DoctorCheck, DoctorError> {
    if !vault.search.semantic {
        return Ok(DoctorCheck::pass("Semantic search", "disabled"));
    }

    let root_id = contextos_core::VaultRootId::try_from(0_usize)?;
    let state_directory = crate::state_dir::resolve_state_directory(vault.state_directory.as_deref(), root.path())?;
    let outcome: Result<_, SearchError> = semantic_config(vault, &state_directory).and_then(|semantic| {
        let service = VaultSearchService::try_from(VaultSearchConfig {
            root_id,
            root: root.path().to_path_buf(),
            excludes: vault.search.exclude.clone(),
            state_directory,
            text_enabled: false,
            graph_enabled: false,
            graph_backend: GraphBackend::default(),
            semantic,
        })?;
        service.status()
    });

    Ok(match outcome {
        Ok(status) => DoctorCheck::pass(
            "Semantic search",
            format!(
                "provider and model are available; the vector store holds {} documents across {} chunks",
                status.semantic.documents, status.semantic.chunks
            ),
        ),
        Err(error) => DoctorCheck::fail(
            "Semantic search",
            error.to_string(),
            "Correct [vault.search.embedding] (provider, model_directory, endpoint, or \
             api_key_env), or check this vault's vectors.db permissions under its derived \
             state directory, and rerun the doctor.",
        ),
    })
}

/// Vault-wide YAML frontmatter validity scan. Reuses the same
/// strict parser `frontmatter_read`/`frontmatter_update`
/// (`contextos_obsidian::FrontmatterDocument`) already enforce, so a file
/// this check passes is guaranteed parseable by those tools too. Always
/// `auto_fixable: false`: reformatting or repairing a caller's YAML is out
/// of scope, per the reject-never-repair non-negotiable.
///
/// Reuses `Filesystem::search_files` (the same traversal `fs_search_files`
/// uses, already honouring the vault's `hidden` configuration) rather than
/// a bespoke walk, for consistent behaviour across every enumeration
/// surface. A file that cannot be read as UTF-8 text
/// at all (binary, oversized, or a genuine I/O race) is skipped rather
/// than reported: that is a different problem outside this check's scope,
/// and every other doctor check already leaves file-level read failures to
/// the purpose-built reading tools.
fn frontmatter_check(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    const MAX_REPORTED: usize = 10;

    let root_path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: root
            .path()
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(root.path().to_path_buf()))?,
    })?;
    let matches = filesystem.search_files(&SearchFilesRequest {
        path: root_path,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: Vec::new(),
        max_results: usize::MAX,
    })?;

    let mut invalid = Vec::new();
    for relative in &matches {
        let absolute = root.path().join(relative);
        let Some(raw) = absolute.to_str() else {
            continue;
        };
        let Ok(file_path) = VaultPath::try_from(VaultPathInput { roots, raw }) else {
            continue;
        };
        let Ok(read) = filesystem.read_text(&ReadTextRequest {
            path: file_path,
            limit: None,
        }) else {
            continue;
        };
        if let Err(error) = FrontmatterDocument::try_from(read.content.as_str())
            && !is_valid_once_placeholders_are_substituted(&read.content)
        {
            invalid.push(format!("{relative}: {error}"));
        }
    }

    if invalid.is_empty() {
        return Ok(DoctorCheck::pass(
            "Frontmatter validity",
            format!("{} markdown files scanned", matches.len()),
        ));
    }
    let mut listed = invalid
        .iter()
        .take(MAX_REPORTED)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if invalid.len() > MAX_REPORTED {
        use std::fmt::Write as _;
        let _ = write!(listed, ", and {} more", invalid.len() - MAX_REPORTED);
    }
    Ok(DoctorCheck::fail(
        "Frontmatter validity",
        format!(
            "{} of {} markdown files have invalid YAML frontmatter: {listed}",
            invalid.len(),
            matches.len()
        ),
        "Correct the YAML frontmatter in the listed file(s) without changing the note body, \
         then rerun the doctor.",
    ))
}

/// True when `content`'s frontmatter parses once every `{{...}}` token is
/// replaced by a YAML-safe placeholder scalar. `{{...}}` is the vault's
/// operator-facing "unresolved placeholder" convention (the same one
/// `context-os-plugin`'s `vault_lint.py` `UNRESOLVED_PLACEHOLDER_RE`
/// recognises), used generally, not only under a `templates/` directory.
/// An unquoted `{` opens a YAML flow mapping, so a plain scalar value
/// containing this convention fails strict parsing on syntax alone. This
/// re-check exists to tell that apart from a genuine structural error: the
/// substitution only ever changes placeholder spans, so an unrelated
/// mistake elsewhere in the same frontmatter block still fails both
/// attempts and is still reported.
fn is_valid_once_placeholders_are_substituted(content: &str) -> bool {
    if !content.contains("{{") {
        return false;
    }
    FrontmatterDocument::try_from(substitute_placeholders(content).as_str()).is_ok()
}

/// Replaces every `{{...}}` span with a fixed, unquoted placeholder word.
/// Non-nesting: the first `}}` after a `{{` closes it. A `{{` with no
/// closing `}}` is left untouched (not a well-formed placeholder).
///
/// Deliberately unquoted rather than a quoted scalar (`"placeholder"`):
/// real placeholders are not always the entire value. A quoted
/// replacement breaks in two cases found live against an operator vault:
/// trailing literal text after the placeholder on the same line (`{{Hat
/// Name}} Hat` becomes `"placeholder" Hat`, invalid: text after a closed
/// quoted scalar), and a placeholder already nested inside a quoted
/// string (`"[[{{slug}}]]"` becomes `"[["placeholder"]]"`, injecting `"`
/// characters mid-string). A single unquoted word is a valid YAML plain
/// scalar fragment in both positions.
fn substitute_placeholders(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("}}") {
            result.push_str("PLACEHOLDER");
            rest = &after_open[end + 2..];
        } else {
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

impl Display for DoctorReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP doctor")?;
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
pub(crate) struct DoctorCheck {
    pub(crate) subject: String,
    pub(crate) status: DoctorStatus,
    pub(crate) message: String,
    pub(crate) action: Option<String>,
    /// Whether `doctor_resolve` may act on this finding without
    /// operator confirmation. A type-level classification set explicitly
    /// per check, not derived from `action`'s free text.
    pub(crate) auto_fixable: bool,
    /// The existing MCP tool `doctor_resolve` dispatches to when
    /// `auto_fixable` is `true`. Always `None` when `auto_fixable` is
    /// `false`.
    pub(crate) remediation_tool: Option<&'static str>,
    /// The configured vault this check applies to, as an index into
    /// `Config::vaults` (and every other per-vault `Vec` `ContextOsServer`
    /// holds in lockstep, per `server.rs`). `None` for the one global
    /// `Configuration` check. Required by `doctor_resolve` to
    /// dispatch a remediation to the correct vault in a multi-vault
    /// configuration: `subject` alone is ambiguous, since every vault's
    /// "Managed indexes"/"Git recovery" checks share the same text.
    pub(crate) vault_index: Option<usize>,
}

impl DoctorCheck {
    fn pass(subject: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            status: DoctorStatus::Pass,
            message: message.into(),
            action: None,
            auto_fixable: false,
            remediation_tool: None,
            vault_index: None,
        }
    }

    /// A failure requiring operator judgement: never auto-fixable.
    fn fail(subject: impl Into<String>, message: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            status: DoctorStatus::Fail,
            message: message.into(),
            action: Some(action.into()),
            auto_fixable: false,
            remediation_tool: None,
            vault_index: None,
        }
    }

    /// A failure `doctor_resolve` may act on without operator confirmation
    /// by calling `remediation_tool`.
    fn fail_auto_fixable(
        subject: impl Into<String>,
        message: impl Into<String>,
        action: impl Into<String>,
        remediation_tool: &'static str,
    ) -> Self {
        Self {
            subject: subject.into(),
            status: DoctorStatus::Fail,
            message: message.into(),
            action: Some(action.into()),
            auto_fixable: true,
            remediation_tool: Some(remediation_tool),
            vault_index: None,
        }
    }

    /// Tags this check with the configured-vault index it applies to.
    /// Applied by the per-vault loop in `TryFrom<&Config>`; every
    /// constructor above defaults to `None` (global), since a check
    /// function has no reason to know its own position in `Config::vaults`.
    fn for_vault(mut self, index: usize) -> Self {
        self.vault_index = Some(index);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DoctorStatus {
    Pass,
    Fail,
}

impl Display for DoctorStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => formatter.write_str("PASS"),
            Self::Fail => formatter.write_str("FAIL"),
        }
    }
}

/// Typed failures that prevent the read-only doctor from assessing all vaults.
#[derive(Debug, Error)]
pub enum DoctorError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Path(#[from] contextos_core::PathError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    StateDir(#[from] crate::state_dir::StateDirError),
    #[error("configured maximum read size overflows bytes")]
    ReadLimitOverflow,
    #[error("vault path is not valid UTF-8: {0}")]
    NonUtf8VaultPath(std::path::PathBuf),
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::Config;

    use super::DoctorReport;

    #[test]
    fn frontmatter_validity_reports_files_that_fail_strict_yaml_parsing() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        std::fs::write(vault.join("valid.md"), "---\ntitle: Fine\n---\nBody\n")?;
        std::fs::write(vault.join("no-frontmatter.md"), "# Just a heading\n")?;
        // The operator's original real-world case: an unquoted colon inside
        // a text value, which strict YAML treats as an unexpected nested
        // mapping rather than a plain scalar.
        std::fs::write(
            vault.join("broken.md"),
            "---\ntitle: Notes: something happened\n---\nBody\n",
        )?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Fail);
        assert!(!check.auto_fixable);
        assert_eq!(check.remediation_tool, None);
        assert!(check.message.contains("broken.md"), "{}", check.message);
        assert!(!check.message.contains("valid.md"), "{}", check.message);
        assert!(!check.message.contains("no-frontmatter.md"), "{}", check.message);
        Ok(())
    }

    #[test]
    fn frontmatter_validity_passes_a_vault_with_only_valid_or_absent_frontmatter()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        std::fs::write(vault.join("valid.md"), "---\ntitle: Fine\n---\nBody\n")?;
        std::fs::write(vault.join("no-frontmatter.md"), "# Just a heading\n")?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Pass);
        Ok(())
    }

    #[test]
    fn a_double_moustache_placeholder_is_not_reported_as_invalid_frontmatter() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        // The real case found scanning an operator vault: an unquoted `{{`
        // is otherwise indistinguishable from opening a YAML flow mapping,
        // so a template using the `{{...}}` placeholder convention (the
        // same one `context-os-plugin`'s `vault_lint.py`
        // `UNRESOLVED_PLACEHOLDER_RE` recognises) fails strict parsing on
        // syntax alone, not on any real structural problem.
        std::fs::write(
            vault.join("template.md"),
            "---\ntitle: {{Human-readable claim title, stated as an assertion}}\nstatus: proposed\n---\nBody\n",
        )?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
        Ok(())
    }

    #[test]
    fn a_genuine_yaml_error_beside_a_placeholder_is_still_reported() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        // The placeholder substitution must not blanket-forgive a file just
        // because it contains `{{...}}` somewhere: a genuinely broken,
        // unrelated line must still be caught.
        std::fs::write(
            vault.join("template.md"),
            "---\ntitle: {{Placeholder}}\nsummary: Notes: something happened\n---\nBody\n",
        )?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Fail);
        assert!(check.message.contains("template.md"), "{}", check.message);
        Ok(())
    }

    #[test]
    fn a_placeholder_followed_by_trailing_literal_text_is_not_reported() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        // Real case found live against the operator's vault
        // (templates/registry-hat.md): the placeholder is not the whole
        // value, just part of it. A quoted replacement ("placeholder")
        // would leave trailing unquoted text after the closing quote,
        // which is itself invalid YAML.
        std::fs::write(vault.join("template.md"), "---\nname: {{Hat Name}} Hat\n---\nBody\n")?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
        Ok(())
    }

    #[test]
    fn a_placeholder_nested_inside_an_already_quoted_string_is_not_reported() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        // Real case found live against the operator's vault
        // (templates/insight-note.md): the placeholder sits inside an
        // already-quoted wikilink string. A quoted replacement would
        // inject `"` characters mid-string, breaking the quoting.
        std::fs::write(
            vault.join("template.md"),
            "---\nrelated:\n  - \"[[{{related-insight-slug}}]]\"\n---\nBody\n",
        )?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
        Ok(())
    }

    #[test]
    fn the_exact_real_vault_templates_that_first_exposed_this_are_now_valid() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        // Verbatim content of the two files that were still failing after
        // the first placeholder-substitution fix, fetched live from the
        // operator's vault (templates/insight-note.md,
        // templates/registry-hat.md), not a simplified reproduction.
        std::fs::write(
            vault.join("insight-note.md"),
            "---\ntitle: {{Human-readable claim title, stated as an assertion}}\ntype: insight\nstatus: proposed\nid: {{slug}}\ncreated: {{YYYY-MM-DD}}\nupdated: {{YYYY-MM-DD}}\ndomain:\n  - {{lowercase-hyphenated-domain}}\nkind: {{principle | pattern | heuristic | reframe}}\nconfidence: {{low | medium | high}}\nsources:\n  - {{memory/raw/YYYY/MM/YYYY-MM-DD-slug.md}}\nrelated:\n  - \"[[{{related-insight-slug}}]]\"\ntags:\n  - insight/{{domain}}\n---\n\nBody.\n",
        )?;
        std::fs::write(
            vault.join("registry-hat.md"),
            "---\nname: {{Hat Name}} Hat\ndescription: {{One sentence: what task phase this hat covers and what it produces.}}\n---\n\nBody.\n",
        )?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Frontmatter validity")
            .ok_or("no Frontmatter validity check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Pass, "{}", check.message);
        Ok(())
    }

    #[test]
    fn stale_or_missing_managed_index_is_classified_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].git.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Managed indexes")
            .ok_or("no Managed indexes check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Fail);
        assert!(check.auto_fixable);
        assert_eq!(check.remediation_tool, Some("vault_index_rebuild"));
        Ok(())
    }

    #[test]
    fn absent_git_repository_is_classified_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Git recovery")
            .ok_or("no Git recovery check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Fail);
        assert!(check.auto_fixable);
        assert_eq!(check.remediation_tool, Some("git_init"));
        let action = check
            .action
            .as_deref()
            .ok_or("no action reported for the missing Git repository")?;
        assert!(
            action.contains("automatic commits"),
            "action must disclose that git_init starts auto-commits going forward, was: {action}"
        );
        Ok(())
    }

    #[test]
    fn each_per_vault_check_is_tagged_with_its_configured_vault_index() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let healthy = fixture.path().join("healthy");
        let stale = fixture.path().join("stale");
        std::fs::create_dir(&healthy)?;
        std::fs::create_dir(&stale)?;
        let mut config = Config::try_from(vec![healthy, stale])?;
        for vault in &mut config.vaults {
            vault.git.enabled = false;
        }
        config.vaults[0].index_md.enabled = false;

        let report = DoctorReport::try_from(&config)?;

        let healthy_managed = report
            .checks
            .iter()
            .find(|check| check.subject == "Managed indexes" && check.vault_index == Some(0))
            .ok_or("no vault-0 Managed indexes check reported")?;
        assert_eq!(healthy_managed.status, super::DoctorStatus::Pass);

        let stale_managed = report
            .checks
            .iter()
            .find(|check| check.subject == "Managed indexes" && check.vault_index == Some(1))
            .ok_or("no vault-1 Managed indexes check reported")?;
        assert_eq!(stale_managed.status, super::DoctorStatus::Fail);
        assert!(stale_managed.auto_fixable);

        let configuration = report
            .checks
            .iter()
            .find(|check| check.subject == "Configuration")
            .ok_or("no Configuration check reported")?;
        assert_eq!(configuration.vault_index, None);
        Ok(())
    }

    #[test]
    fn semantic_search_misconfiguration_is_not_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        std::fs::create_dir(&vault)?;
        let mut config = Config::try_from(vec![vault])?;
        config.vaults[0].index_md.enabled = false;
        config.vaults[0].git.enabled = false;
        config.vaults[0].search.semantic = true;

        let report = DoctorReport::try_from(&config)?;

        let check = report
            .checks
            .iter()
            .find(|check| check.subject == "Semantic search")
            .ok_or("no Semantic search check reported")?;
        assert_eq!(check.status, super::DoctorStatus::Fail);
        assert!(!check.auto_fixable);
        assert_eq!(check.remediation_tool, None);
        Ok(())
    }
}
