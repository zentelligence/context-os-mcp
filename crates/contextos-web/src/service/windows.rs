//! Windows backend: a per-user Scheduled Task with a logon trigger, run
//! only while the user is logged on. No elevation is needed and, like the
//! Linux and macOS backends, nothing here runs as `LocalSystem` or any
//! other privileged account.
//!
//! Unlike the other two backends, there is no separate service-definition
//! file: `schtasks` owns the task's persisted definition itself, so
//! "installed" is answered by querying `schtasks` directly rather than by
//! checking for a file this module wrote.

use super::{CommandRunner, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked};

const TASK_NAME: &str = "ContextOS Web";

/// Installs `contextos-web` as a per-user Windows Scheduled Task.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScheduledTaskBackend;

fn task_run_value(spec: &ServiceSpec) -> String {
    format!(
        "\"{}\" --config \"{}\"",
        spec.binary_path.display(),
        spec.web_config_path.display(),
    )
}

/// Queries whether `TASK_NAME` is currently registered. `schtasks /Query`
/// exits `0` when the task exists and non-zero (commonly with a
/// locale-dependent "cannot find the file specified" message) otherwise;
/// only the exit code is relied on here.
fn query(runner: &dyn CommandRunner) -> Result<super::CommandOutput, ServiceError> {
    let args = ["/Query", "/TN", TASK_NAME];
    runner.run("schtasks", &args).map_err(|source| ServiceError::Spawn {
        program: "schtasks".to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        source,
    })
}

impl ServiceBackend for ScheduledTaskBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let run_value = task_run_value(spec);
        run_checked(
            runner,
            "schtasks",
            &[
                "/Create", "/TN", TASK_NAME, "/TR", &run_value, "/SC", "ONLOGON", "/RL", "LIMITED", "/F",
            ],
        )
    }

    fn uninstall(&self, runner: &dyn CommandRunner, _spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        if !query(runner)?.success {
            return Ok(UninstallOutcome::NotInstalled);
        }
        run_checked(runner, "schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, _spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        let output = query(runner)?;
        if !output.success {
            return Ok(ServiceStatus::NotInstalled);
        }
        // Best effort only: `schtasks`' human-readable status text is
        // localised, so this substring match is accurate on an
        // English-language Windows install and only a heuristic elsewhere.
        // Recorded as a known limitation rather than attempted further:
        // `schtasks` has no locale-independent machine-readable "is this
        // task's process currently running" flag to parse instead.
        let running = output.stdout.to_ascii_lowercase().contains("running");
        Ok(ServiceStatus::Installed { running })
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
