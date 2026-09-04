//! Installs, removes, and reports on `contextos-web` running as an
//! auto-starting, per-user background service.
//!
//! One [`ServiceBackend`] per host platform (Linux via `systemd --user`,
//! macOS via a `launchd` `LaunchAgent`, Windows via a per-user Scheduled
//! Task); [`current_platform_backend`] selects the one matching the running
//! process's own OS. Every backend is always compiled in on every host,
//! deliberately: none of this module's logic calls a real platform binary
//! except through the injected [`CommandRunner`], so the two backends for
//! platforms this process is not running on stay unit-testable (exact
//! argument construction, asserted against a fake runner) without
//! `#[cfg(target_os = ...)]` gating anything here. Only [`SystemCommandRunner`]
//! itself is genuinely platform-specific, and it is exercised end to end
//! only on its own host; the other two platforms' real-host behaviour is a
//! residual acceptance gap, matching this repository's standing
//! Windows/macOS acceptance discipline (never claim a platform's gate from
//! Linux-only evidence).
//!
//! All three services run as the invoking user, none needs elevation: a
//! deliberate scope choice that keeps every platform's install symmetrical
//! and avoids a Windows-only
//! `windows-service` dependency and elevated install step a true SCM
//! service would need.

mod linux;
mod macos;
mod windows;

use std::io;
use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

pub use linux::SystemdUserBackend;
pub use macos::LaunchdBackend;
pub use windows::ScheduledTaskBackend;

/// Everything a backend needs to install `contextos-web` as a service.
/// Directories are passed in, never resolved internally, so tests never
/// touch the operator's real home or configuration directory
/// (`testing.md`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    /// Absolute path to the `contextos-web` binary to run.
    pub binary_path: PathBuf,
    /// Absolute path to the `web.toml` this instance should load, passed
    /// as `--config`.
    pub web_config_path: PathBuf,
    /// The user's home directory (the macOS backend's `LaunchAgents`
    /// directory is anchored here).
    pub home_dir: PathBuf,
    /// The user's configuration directory (the Linux backend's
    /// `systemd/user` unit directory is anchored here). Unused by the
    /// macOS and Windows backends.
    pub config_dir: PathBuf,
}

/// The service's current installation and run state, as reported by
/// [`ServiceBackend::status`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Installed { running: bool },
    NotInstalled,
}

/// The outcome of [`ServiceBackend::uninstall`]: whether a service was
/// actually present to remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallOutcome {
    Removed,
    NotInstalled,
}

/// One platform's mechanism for registering `contextos-web` as an
/// auto-starting, per-user background service.
pub trait ServiceBackend {
    /// Installs the service, overwriting any existing definition, and
    /// starts it immediately. Idempotent: installing over an
    /// already-installed, already-running service succeeds and leaves it
    /// running.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when the service definition cannot be
    /// written, or a required platform command cannot be spawned or exits
    /// non-zero.
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError>;

    /// Stops and removes the service. Reports
    /// [`UninstallOutcome::NotInstalled`], not an error, when no service was
    /// registered.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be
    /// spawned or exits non-zero, or when the service definition exists but
    /// cannot be removed.
    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError>;

    /// Reports current installation and run state without changing
    /// anything.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be
    /// spawned.
    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError>;
}

/// Selects the [`ServiceBackend`] matching `os` (an `std::env::consts::OS`
/// value), taking the value as a parameter rather than reading it directly
/// so the "unrecognised platform" branch is reachable by a test without
/// needing to run this binary on one.
///
/// # Errors
///
/// Returns [`ServiceError::UnsupportedPlatform`] when `os` names a platform
/// with no [`ServiceBackend`] implementation.
pub fn backend_for_os(os: &str) -> Result<Box<dyn ServiceBackend>, ServiceError> {
    match os {
        "linux" => Ok(Box::new(SystemdUserBackend)),
        "macos" => Ok(Box::new(LaunchdBackend)),
        "windows" => Ok(Box::new(ScheduledTaskBackend)),
        other => Err(ServiceError::UnsupportedPlatform { os: other.to_owned() }),
    }
}

/// Selects the [`ServiceBackend`] for the platform this binary is actually
/// running on.
///
/// # Errors
///
/// Returns [`ServiceError::UnsupportedPlatform`] when the running platform
/// (`std::env::consts::OS`) has no [`ServiceBackend`] implementation.
pub fn current_platform_backend() -> Result<Box<dyn ServiceBackend>, ServiceError> {
    backend_for_os(std::env::consts::OS)
}

/// The result of running one external command: exit success and captured,
/// lossily-decoded output. Used for diagnostics and, in one backend's
/// `status` implementation, a best-effort running/idle read; a backend's
/// own success/failure test for an install or uninstall step is always the
/// command's exit status, never a match on this output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl From<std::process::Output> for CommandOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// Runs one external command, abstracted so every backend's exact
/// invocation is assertable against a fake without a real
/// `systemctl`/`launchctl`/`schtasks` binary on the test host
/// (`testing.md`: "inject ... external providers").
pub trait CommandRunner {
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when `program` cannot be
    /// spawned at all; a non-zero exit is reported through
    /// [`CommandOutput::success`], not this `Result`.
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error>;
}

/// The real [`CommandRunner`], backed by [`std::process::Command`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error> {
        Command::new(program).args(args).output().map(CommandOutput::from)
    }
}

/// Runs `program args` and maps a spawn failure or a non-zero exit onto
/// [`ServiceError`]. Shared by every backend's own "this step must
/// succeed" commands (`daemon-reload`, `enable --now`, `bootstrap`, and so
/// on); a backend step that treats a non-zero exit as informative rather
/// than fatal (systemd's `is-active`, launchd's best-effort `bootout`) does
/// not use this helper.
pub(super) fn run_checked(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<(), ServiceError> {
    let output = runner.run(program, args).map_err(|source| ServiceError::Spawn {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        source,
    })?;
    if output.success {
        Ok(())
    } else {
        Err(ServiceError::CommandFailed {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: output.stderr,
        })
    }
}

/// Typed failures installing, removing, or reporting on the
/// `contextos-web` background service.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("contextos-web service management is not implemented for platform {os:?}")]
    UnsupportedPlatform { os: String },
    #[error("could not write the service definition at {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not remove the service definition at {}: {source}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not run `{program} {}`: {source}", args.join(" "))]
    Spawn {
        program: String,
        args: Vec<String>,
        #[source]
        source: io::Error,
    },
    #[error("`{program} {}` failed: {stderr}", args.join(" "))]
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    #[error("unexpected output from `{program} {}`: {detail}", args.join(" "))]
    UnexpectedOutput {
        program: String,
        args: Vec<String>,
        detail: String,
    },
}

/// Test-only [`CommandRunner`] that records every invocation and replays a
/// scripted queue of responses, so each backend's exact command sequence is
/// assertable without a real `systemctl`/`launchctl`/`schtasks` binary.
/// Shared by `linux_test`, `macos_test`, and `windows_test` rather than each
/// keeping its own copy.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeCommandRunner {
    calls: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    responses: std::cell::RefCell<std::collections::VecDeque<Result<CommandOutput, io::Error>>>,
}

#[cfg(test)]
impl FakeCommandRunner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Queues a successful response for the next call.
    pub(crate) fn push_success(&self, stdout: &str) {
        self.responses.borrow_mut().push_back(Ok(CommandOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }));
    }

    /// Queues a non-zero-exit response for the next call.
    pub(crate) fn push_failure(&self, stderr: &str) {
        self.responses.borrow_mut().push_back(Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }));
    }

    /// Queues a spawn failure (the program itself could not be started) for
    /// the next call.
    pub(crate) fn push_spawn_error(&self) {
        self.responses
            .borrow_mut()
            .push_back(Err(io::Error::other("simulated spawn failure")));
    }

    /// Every call made so far, in order, as `(program, args)`.
    pub(crate) fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.borrow().clone()
    }
}

#[cfg(test)]
impl CommandRunner for FakeCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error> {
        self.calls
            .borrow_mut()
            .push((program.to_owned(), args.iter().map(|arg| (*arg).to_owned()).collect()));
        self.responses.borrow_mut().pop_front().unwrap_or(Ok(CommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
