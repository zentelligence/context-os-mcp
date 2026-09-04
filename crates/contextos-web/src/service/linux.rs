//! Linux backend: a `systemd --user` unit, so no elevation is needed and
//! the service starts and stops with the user's own login session.

use std::path::PathBuf;

use super::{CommandRunner, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked};
use crate::atomic_write::write_atomically;

const UNIT_NAME: &str = "contextos-web.service";

/// Installs `contextos-web` as a `systemd --user` unit.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemdUserBackend;

fn unit_path(spec: &ServiceSpec) -> PathBuf {
    spec.config_dir.join("systemd").join("user").join(UNIT_NAME)
}

fn unit_contents(spec: &ServiceSpec) -> String {
    format!(
        "[Unit]\n\
         Description=ContextOS web UI\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\" --config \"{}\"\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        spec.binary_path.display(),
        spec.web_config_path.display(),
    )
}

impl ServiceBackend for SystemdUserBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = unit_path(spec);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ServiceError::Write {
                path: path.clone(),
                source,
            })?;
        }
        write_atomically(&path, unit_contents(spec).as_bytes()).map_err(|source| ServiceError::Write {
            path: path.clone(),
            source,
        })?;

        run_checked(runner, "systemctl", &["--user", "daemon-reload"])?;
        run_checked(runner, "systemctl", &["--user", "enable", "--now", UNIT_NAME])
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        let path = unit_path(spec);
        if !path.exists() {
            return Ok(UninstallOutcome::NotInstalled);
        }

        run_checked(runner, "systemctl", &["--user", "disable", "--now", UNIT_NAME])?;
        std::fs::remove_file(&path).map_err(|source| ServiceError::Remove {
            path: path.clone(),
            source,
        })?;
        run_checked(runner, "systemctl", &["--user", "daemon-reload"])?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        let path = unit_path(spec);
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }

        let args = ["--user", "is-active", UNIT_NAME];
        let output = runner.run("systemctl", &args).map_err(|source| ServiceError::Spawn {
            program: "systemctl".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            source,
        })?;
        Ok(ServiceStatus::Installed {
            running: output.success,
        })
    }
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
