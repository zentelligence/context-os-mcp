//! `contextos config` with no subcommand: an interactive guided-setup
//! wizard tying vault setup, optional semantic-search enablement
//! (embedding-model download reusing `model_cli::download_default_model`),
//! index rebuild (reusing `IndexReport::try_from(&Config)`, the same path
//! `contextos index` uses), and optional MCP host registration (reusing
//! `host_registration::register`) into one guided first run, so an operator
//! never hand-edits `config.toml` or a host's own configuration file.
//!
//! Every operator interaction goes through the [`Interviewer`] trait, and
//! every other environment dependency that would otherwise need real
//! network access or a real Claude Desktop install (the embedding-model
//! download and host config-path discovery) is injected via
//! [`InterviewEnvironment`], so [`run_interview`] is exercised in tests
//! against a scripted double with no real terminal I/O or network access.
//!
//! When `config.toml` already has vault(s) configured, [`run_interview`]
//! loads it first rather than treating every run as a fresh install: a
//! single existing vault is offered back for edit with its current name,
//! path, managed flag, and semantic-search state prefilled as defaults;
//! more than one existing vault instead asks what to focus on (general
//! server settings, all vaults in turn, or one named vault to edit or
//! remove). Adding a vault is otherwise not optional on a fresh install
//! (no `[[vault]]` entries yet), so [`run_interview`] always asks for at
//! least one before any optional step in that case; once at least one vault
//! is already configured, adding another becomes an optional follow-up
//! question instead.
//! Semantic search enablement and Claude Desktop registration are both
//! genuinely optional, matched by a `confirm` gate before anything for that
//! step runs.

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use inquire::{Confirm, Select, Text};
use thiserror::Error;

use crate::{
    Config, ConfigDocument, ConfigError, ConfigIoError, ConfigWriterError, DetectsRunningProcesses,
    HostPathError, HostPathResolution, HostRegistrationError, IndexCliError, IndexReport,
    ModelCliError, RegisteredServer, VaultSummary, is_claude_desktop_running, load_config_document,
    register, write_config_document,
};

/// One operator interaction the interview wizard needs: a yes/no question
/// with a default, a free-text answer (optionally prefilled with a
/// default), or a choice from a fixed list. Abstracted so [`run_interview`]
/// is testable against a scripted double instead of a real terminal.
pub trait Interviewer {
    /// Asks a yes/no question, returning `default` when the operator enters
    /// nothing.
    ///
    /// # Errors
    ///
    /// Returns [`InterviewError::Prompt`] when the operator cancels the
    /// prompt (for example Ctrl-C) or the underlying terminal I/O fails.
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, InterviewError>;

    /// Asks a free-text question, returning the operator's trimmed answer.
    ///
    /// # Errors
    ///
    /// Returns [`InterviewError::Prompt`] when the operator cancels the
    /// prompt or the underlying terminal I/O fails.
    fn ask(&mut self, prompt: &str) -> Result<String, InterviewError>;

    /// Asks a free-text question prefilled with `default`, returning
    /// `default` unchanged when the operator accepts it as-is.
    ///
    /// # Errors
    ///
    /// Returns [`InterviewError::Prompt`] when the operator cancels the
    /// prompt or the underlying terminal I/O fails.
    fn ask_with_default(&mut self, prompt: &str, default: &str) -> Result<String, InterviewError>;

    /// Presents `options` as a fixed list and returns the index of the
    /// operator's selection.
    ///
    /// # Errors
    ///
    /// Returns [`InterviewError::Prompt`] when the operator cancels the
    /// prompt, the underlying terminal I/O fails, or (structurally
    /// unreachable in the real terminal implementation, but a typed error
    /// rather than a panic here regardless) the selection cannot be matched
    /// back to one of `options`.
    fn choose(&mut self, prompt: &str, options: &[&str]) -> Result<usize, InterviewError>;
}

/// The real [`Interviewer`], backed by `inquire`'s terminal prompts.
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalInterviewer;

impl Interviewer for TerminalInterviewer {
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool, InterviewError> {
        Ok(Confirm::new(prompt).with_default(default).prompt()?)
    }

    fn ask(&mut self, prompt: &str) -> Result<String, InterviewError> {
        Ok(Text::new(prompt).prompt()?.trim().to_owned())
    }

    fn ask_with_default(&mut self, prompt: &str, default: &str) -> Result<String, InterviewError> {
        Ok(Text::new(prompt)
            .with_default(default)
            .prompt()?
            .trim()
            .to_owned())
    }

    fn choose(&mut self, prompt: &str, options: &[&str]) -> Result<usize, InterviewError> {
        let selection = Select::new(prompt, options.to_vec()).prompt()?;
        options
            .iter()
            .position(|option| *option == selection)
            .ok_or_else(|| {
                InterviewError::Prompt(inquire::InquireError::Custom(
                    "selected option not found among the offered choices".into(),
                ))
            })
    }
}

/// Every environment dependency [`run_interview`] needs beyond operator
/// interaction: the shared embedding-model cache directory and downloader,
/// Claude Desktop config-path discovery, running-process detection, and
/// this binary's own invocation details for the registered entry. Bundled
/// so production code (`main.rs`) supplies the real environment in one
/// place, and tests substitute fakes for the two dependencies that would
/// otherwise need real network access or a real Claude Desktop install.
pub struct InterviewEnvironment<'a> {
    /// The `config.toml` path this wizard edits.
    pub config_path: PathBuf,
    /// The shared local embedding-model cache directory, passed to
    /// `download_model` unchanged.
    pub model_cache_dir: PathBuf,
    /// Downloads the default embedding model into a cache directory,
    /// returning the resolved model directory. Production: reuses
    /// `model_cli::download_default_model`. Tests: a fake returning a fixed
    /// path with no network access.
    pub download_model: &'a dyn Fn(&Path) -> Result<PathBuf, ModelCliError>,
    /// Resolves Claude Desktop's own configuration file location.
    /// Production: `host_paths::default_claude_desktop_config_path`. Tests:
    /// a fake returning a fixed [`HostPathResolution`] with no real host
    /// install required.
    pub resolve_host_path: &'a dyn Fn() -> Result<HostPathResolution, HostPathError>,
    /// Detects whether Claude Desktop is currently running, re-checked by
    /// `register` itself immediately before any write.
    pub process_detector: &'a dyn DetectsRunningProcesses,
    /// Called between polls while [`run_interview`] waits for Claude
    /// Desktop to close, after the operator opts to wait rather than
    /// register manually afterwards. Production: sleeps briefly so the poll
    /// loop does not busy-spin. Tests: a no-op, so a scripted detector
    /// double can flip from running to not-running with no real wall-clock
    /// delay (`AGENTS.md`: tests never depend on wall-clock sleeps).
    pub wait_tick: &'a dyn Fn(),
    /// This binary's own absolute path, used as the registered entry's
    /// `command` (mirrors `contextos config mcp register`'s own
    /// construction in `main.rs`).
    pub server_command: String,
}

/// The outcome of the interview's optional "register with Claude Desktop"
/// step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostRegistrationOutcome {
    /// The operator declined the step, or declined to supply a path once
    /// discovery could not find one automatically.
    Skipped,
    /// Registration succeeded at this host configuration path.
    Registered { host_path: PathBuf },
    /// Claude Desktop was detected running and the operator declined to
    /// wait for it to close (`run_interview` offers to wait and retry
    /// registration automatically before reaching this outcome); the
    /// operator needs to close it and register manually afterwards.
    HostRunning { host_path: PathBuf },
}

/// A short, human-readable summary of one `contextos config` interview run,
/// written verbatim to stdout by the caller.
#[derive(Debug)]
pub struct InterviewReport {
    pub added_vaults: Vec<String>,
    /// Vaults an already-existing configuration's edit flow touched this
    /// run (a single pre-existing vault offered back for edit, or one or
    /// more vaults reached through the multi-vault focus menu's "all
    /// vaults" or "a specific vault" paths). Does not include vaults added
    /// fresh this run; see [`Self::added_vaults`] for those.
    pub edited_vaults: Vec<String>,
    /// Vaults removed this run through the multi-vault focus menu's "a
    /// specific vault" path.
    pub removed_vaults: Vec<String>,
    /// Whether the multi-vault focus menu's "general (server) settings"
    /// path was used this run.
    pub server_settings_changed: bool,
    pub semantic_model_directory: Option<PathBuf>,
    pub index_summary: IndexReport,
    pub host_registration: HostRegistrationOutcome,
}

impl Display for InterviewReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP guided setup")?;
        if !self.added_vaults.is_empty() {
            writeln!(
                formatter,
                "Added vault(s): {}",
                self.added_vaults.join(", ")
            )?;
        }
        if !self.edited_vaults.is_empty() {
            writeln!(
                formatter,
                "Edited vault(s): {}",
                self.edited_vaults.join(", ")
            )?;
        }
        if !self.removed_vaults.is_empty() {
            writeln!(
                formatter,
                "Removed vault(s): {}",
                self.removed_vaults.join(", ")
            )?;
        }
        if self.server_settings_changed {
            writeln!(formatter, "Server settings updated")?;
        }
        match &self.semantic_model_directory {
            Some(directory) => writeln!(
                formatter,
                "Semantic search enabled; embedding model at {}",
                directory.display()
            )?,
            None => writeln!(formatter, "Semantic search not enabled")?,
        }
        write!(formatter, "{}", self.index_summary)?;
        match &self.host_registration {
            HostRegistrationOutcome::Skipped => {
                writeln!(formatter, "Claude Desktop registration skipped")?;
            }
            HostRegistrationOutcome::Registered { host_path } => {
                writeln!(
                    formatter,
                    "Registered with Claude Desktop at {}",
                    host_path.display()
                )?;
            }
            HostRegistrationOutcome::HostRunning { host_path } => {
                writeln!(
                    formatter,
                    "Claude Desktop is currently running; close it, then run `contextos config \
                     mcp register --host claude-desktop --config-path {}`",
                    host_path.display()
                )?;
            }
        }
        Ok(())
    }
}

/// Runs the full `contextos config` interview. On a fresh install (no
/// `[[vault]]` entries yet in the loaded `config.toml`), adds one or more
/// vaults, mandatory as before. Against an already-configured file, offers
/// the existing vault(s) back for edit first (see the module documentation)
/// before optionally adding more. Either way, optionally enables semantic
/// search for any vault(s) newly added this run (downloading the embedding
/// model or accepting an existing model directory), writes `config.toml`,
/// rebuilds every configured vault's search index, and optionally registers
/// this server with Claude Desktop.
///
/// # Errors
///
/// Returns [`InterviewError`] on operator cancellation, an invalid vault or
/// configuration edit, a configuration write failure, an index-rebuild
/// failure that is not itself per-vault-recoverable, or a host-registration
/// failure other than the host being detected running (which is reported in
/// [`InterviewReport::host_registration`] instead of erroring the whole
/// interview).
pub fn run_interview(
    interviewer: &mut dyn Interviewer,
    environment: &InterviewEnvironment<'_>,
) -> Result<InterviewReport, InterviewError> {
    let mut document = load_config_document(&environment.config_path)?;
    let existing = document.vaults();

    let mut edited_vaults = Vec::new();
    let mut removed_vaults = Vec::new();
    let mut server_settings_changed = false;

    match existing.len() {
        0 => {}
        1 => {
            let updated_name =
                edit_existing_vault(interviewer, &mut document, environment, &existing[0])?;
            edited_vaults.push(updated_name);
        }
        _ => {
            focus_on_existing_configuration(
                interviewer,
                &mut document,
                environment,
                &mut edited_vaults,
                &mut removed_vaults,
                &mut server_settings_changed,
            )?;
        }
    }

    let mut added_vaults = Vec::new();
    if existing.is_empty() {
        loop {
            added_vaults.push(add_one_vault(interviewer, &mut document)?);
            if !interviewer.confirm("Add another vault?", false)? {
                break;
            }
        }
    } else {
        while interviewer.confirm("Add a new vault?", false)? {
            added_vaults.push(add_one_vault(interviewer, &mut document)?);
        }
    }

    let semantic_model_directory = if !added_vaults.is_empty()
        && interviewer.confirm(
            "Enable semantic search for the vault(s) you just added?",
            false,
        )? {
        let model_directory =
            if interviewer.confirm("Download the local embedding model now?", true)? {
                (environment.download_model)(&environment.model_cache_dir)?
            } else {
                parse_manual_path(
                    &interviewer.ask("Path to an existing local embedding model directory:")?,
                )
            };
        for name in &added_vaults {
            document.enable_semantic_search(name, &model_directory)?;
        }
        Some(model_directory)
    } else {
        None
    };

    write_config_document(&environment.config_path, &document)?;

    let config = Config::try_from(document.render().as_str())?;
    let index_summary = IndexReport::try_from(&config)?;

    let host_registration =
        if interviewer.confirm("Register this server with Claude Desktop now?", true)? {
            register_with_host(interviewer, environment)?
        } else {
            HostRegistrationOutcome::Skipped
        };

    Ok(InterviewReport {
        added_vaults,
        edited_vaults,
        removed_vaults,
        server_settings_changed,
        semantic_model_directory,
        index_summary,
        host_registration,
    })
}

/// Asks for one vault's name and path and appends it, the shared body of
/// both the fresh-install mandatory loop and the already-configured
/// optional "add a new vault?" loop.
fn add_one_vault(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
) -> Result<String, InterviewError> {
    let name = interviewer.ask("Vault name:")?;
    let path = interviewer.ask("Vault path (absolute, must already exist):")?;
    document.add_vault(&name, &parse_manual_path(&path), true)?;
    Ok(name)
}

/// Offers the existing `current` vault back for edit: its name, path,
/// managed flag, and semantic-search state are prefilled as defaults, which
/// the operator accepts unchanged or overrides. Returns the vault's name
/// after any rename, for the caller's edited-vault report.
fn edit_existing_vault(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
    environment: &InterviewEnvironment<'_>,
    current: &VaultSummary,
) -> Result<String, InterviewError> {
    let name = interviewer.ask_with_default("Vault name:", &current.name)?;
    let path = interviewer.ask_with_default(
        "Vault path (absolute, must already exist):",
        &current.path.display().to_string(),
    )?;
    let managed = interviewer.confirm(
        &format!("Keep {name} managed (indexing, oplog, Git)?"),
        current.managed,
    )?;
    document.update_vault(&current.name, &name, &parse_manual_path(&path), managed)?;

    let semantic = interviewer.confirm(
        &format!("Enable semantic search for {name}?"),
        current.semantic,
    )?;
    if semantic {
        let keep_existing = current.model_directory.is_some()
            && interviewer.confirm(
                &format!(
                    "Keep the current embedding model directory ({})?",
                    current
                        .model_directory
                        .as_ref()
                        .map_or_else(|| "none".to_owned(), |path| path.display().to_string())
                ),
                true,
            )?;
        let model_directory =
            if let (true, Some(existing)) = (keep_existing, &current.model_directory) {
                existing.clone()
            } else if interviewer.confirm("Download the local embedding model now?", true)? {
                (environment.download_model)(&environment.model_cache_dir)?
            } else {
                parse_manual_path(
                    &interviewer.ask("Path to an existing local embedding model directory:")?,
                )
            };
        document.enable_semantic_search(&name, &model_directory)?;
    } else if current.semantic {
        document.disable_semantic_search(&name)?;
    }

    Ok(name)
}

/// The multi-vault case: repeatedly asks what to focus on (general server
/// settings, all vaults in turn, or one named vault to edit or remove)
/// until the operator declines "focus on something else?".
fn focus_on_existing_configuration(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
    environment: &InterviewEnvironment<'_>,
    edited_vaults: &mut Vec<String>,
    removed_vaults: &mut Vec<String>,
    server_settings_changed: &mut bool,
) -> Result<(), InterviewError> {
    let focus_options = [
        "General (server) settings",
        "All vaults",
        "A specific vault",
    ];
    loop {
        let focus = interviewer.choose("What would you like to focus on?", &focus_options)?;
        match focus {
            0 => {
                edit_server_settings(interviewer, document)?;
                *server_settings_changed = true;
            }
            1 => {
                for vault in document.vaults() {
                    let updated_name =
                        edit_existing_vault(interviewer, document, environment, &vault)?;
                    edited_vaults.push(updated_name);
                }
            }
            2 => {
                focus_on_one_vault(
                    interviewer,
                    document,
                    environment,
                    edited_vaults,
                    removed_vaults,
                )?;
            }
            _ => {
                return Err(InterviewError::Prompt(inquire::InquireError::Custom(
                    "focus selection out of range".into(),
                )));
            }
        }

        if !interviewer.confirm("Focus on something else?", false)? {
            break;
        }
    }
    Ok(())
}

/// The "a specific vault" focus: pick one configured vault by name, then
/// edit it (prefilled defaults, as [`edit_existing_vault`]) or remove it.
fn focus_on_one_vault(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
    environment: &InterviewEnvironment<'_>,
    edited_vaults: &mut Vec<String>,
    removed_vaults: &mut Vec<String>,
) -> Result<(), InterviewError> {
    let vaults = document.vaults();
    let names: Vec<&str> = vaults.iter().map(|vault| vault.name.as_str()).collect();
    let index = interviewer.choose("Which vault?", &names)?;
    let vault = vaults.get(index).cloned().ok_or_else(|| {
        InterviewError::Prompt(inquire::InquireError::Custom(
            "selected vault not found among the offered choices".into(),
        ))
    })?;

    let action = interviewer.choose(
        &format!("What would you like to do with {}?", vault.name),
        &["Edit", "Remove"],
    )?;
    if action == 1 {
        document.remove_vault(&vault.name)?;
        removed_vaults.push(vault.name.clone());
    } else {
        let updated_name = edit_existing_vault(interviewer, document, environment, &vault)?;
        edited_vaults.push(updated_name);
    }
    Ok(())
}

/// The "general (server) settings" focus: prefills the current
/// `transports`/`log_level`/`log_file` values as defaults.
fn edit_server_settings(
    interviewer: &mut dyn Interviewer,
    document: &mut ConfigDocument,
) -> Result<(), InterviewError> {
    let current = document.server_settings();
    let transports_raw = interviewer.ask_with_default(
        "Transports (comma-separated: stdio, http):",
        &current.transports.join(", "),
    )?;
    let transports: Vec<String> = transports_raw
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    let log_level = interviewer
        .ask_with_default(
            "Log level (error, warn, info, debug, trace):",
            &current.log_level,
        )?
        .to_ascii_lowercase();
    let log_file =
        interviewer.ask_with_default("Log file path (blank for stderr):", &current.log_file)?;
    document.set_server_settings(&transports, &log_level, &log_file)?;
    Ok(())
}

/// Resolves Claude Desktop's config path (auto-discovery, falling back to a
/// manual prompt on `NotFound`/`Ambiguous`) and, unless the operator leaves
/// a manual prompt blank, registers this server there.
fn register_with_host(
    interviewer: &mut dyn Interviewer,
    environment: &InterviewEnvironment<'_>,
) -> Result<HostRegistrationOutcome, InterviewError> {
    let host_path = match (environment.resolve_host_path)()? {
        HostPathResolution::Found(path) => path,
        HostPathResolution::NotFound { reason } => {
            let manual = interviewer.ask(&format!(
                "Could not automatically find Claude Desktop's configuration file ({reason}). \
                 Enter its path manually, or leave blank to skip:"
            ))?;
            if manual.is_empty() {
                return Ok(HostRegistrationOutcome::Skipped);
            }
            parse_manual_path(&manual)
        }
        HostPathResolution::Ambiguous { candidates } => {
            let list = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let manual = interviewer.ask(&format!(
                "More than one Claude Desktop configuration file candidate was found ({list}). \
                 Enter the correct path manually, or leave blank to skip:"
            ))?;
            if manual.is_empty() {
                return Ok(HostRegistrationOutcome::Skipped);
            }
            parse_manual_path(&manual)
        }
    };

    let entry = RegisteredServer {
        command: environment.server_command.clone(),
        args: vec![
            "--config".to_owned(),
            environment.config_path.to_string_lossy().into_owned(),
        ],
    };

    loop {
        match register(&host_path, &entry, environment.process_detector, false) {
            Ok(()) => return Ok(HostRegistrationOutcome::Registered { host_path }),
            Err(HostRegistrationError::HostRunning) => {
                let wait = interviewer.confirm(
                    "Claude Desktop is currently running. Wait for it to close and register \
                     automatically? (Declining prints the command to run manually afterwards.)",
                    true,
                )?;
                if !wait {
                    return Ok(HostRegistrationOutcome::HostRunning { host_path });
                }
                while is_claude_desktop_running(environment.process_detector) {
                    (environment.wait_tick)();
                }
            }
            Err(source) => return Err(InterviewError::HostRegistration(source)),
        }
    }
}

/// Strips one layer of matching surrounding quotes (`"..."` or `'...'`)
/// from a manually-entered path before it is used. A path pasted from a
/// file manager's "Copy as path" (Windows Explorer wraps it in double
/// quotes) otherwise lands here with the quote characters still part of
/// the string, which then resolves as a nonexistent path (operator-tested:
/// `contextos config` reported the quoted path not found).
fn parse_manual_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    PathBuf::from(unquoted)
}

/// Typed failures running the `contextos config` interview.
#[derive(Debug, Error)]
pub enum InterviewError {
    #[error("the operator cancelled the interview: {0}")]
    Prompt(#[from] inquire::InquireError),
    #[error(transparent)]
    ConfigIo(#[from] ConfigIoError),
    #[error(transparent)]
    ConfigWriter(#[from] ConfigWriterError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Index(#[from] IndexCliError),
    #[error(transparent)]
    Model(#[from] ModelCliError),
    #[error(transparent)]
    HostPath(#[from] HostPathError),
    #[error(transparent)]
    HostRegistration(#[from] HostRegistrationError),
}

#[cfg(test)]
#[path = "interview_test.rs"]
mod tests;
