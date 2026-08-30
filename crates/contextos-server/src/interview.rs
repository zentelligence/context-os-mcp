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
//! Adding vaults is not optional, so [`run_interview`] always asks for at
//! least one before any optional step; declining "add another vault?"
//! simply ends that loop after the first.
//! Semantic search enablement and Claude Desktop registration are both
//! genuinely optional, matched by a `confirm` gate before anything for that
//! step runs.

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use inquire::{Confirm, Text};
use thiserror::Error;

use crate::{
    Config, ConfigError, ConfigIoError, ConfigWriterError, DetectsRunningProcesses, HostPathError,
    HostPathResolution, HostRegistrationError, IndexCliError, IndexReport, ModelCliError,
    RegisteredServer, is_claude_desktop_running, load_config_document, register,
    write_config_document,
};

/// One operator interaction the interview wizard needs: a yes/no question
/// with a default, or a free-text answer. Abstracted so [`run_interview`]
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
    pub semantic_model_directory: Option<PathBuf>,
    pub index_summary: IndexReport,
    pub host_registration: HostRegistrationOutcome,
}

impl Display for InterviewReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP guided setup")?;
        writeln!(
            formatter,
            "Added vault(s): {}",
            self.added_vaults.join(", ")
        )?;
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

/// Runs the full `contextos config` interview: adds one or more vaults,
/// optionally enables semantic search (downloading the embedding model or
/// accepting an existing model directory), writes `config.toml`, rebuilds
/// every configured vault's search index, and optionally registers this
/// server with Claude Desktop.
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
    let mut added_vaults = Vec::new();

    loop {
        let name = interviewer.ask("Vault name:")?;
        let path = interviewer.ask("Vault path (absolute, must already exist):")?;
        document.add_vault(&name, &parse_manual_path(&path), true)?;
        added_vaults.push(name);

        if !interviewer.confirm("Add another vault?", false)? {
            break;
        }
    }

    let semantic_model_directory = if interviewer.confirm(
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
        semantic_model_directory,
        index_summary,
        host_registration,
    })
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
