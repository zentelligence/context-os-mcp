//! macOS backend: a `launchd` per-user `LaunchAgent`, so no elevation is
//! needed and the service runs and stops with the user's own login
//! session (`gui/<uid>` domain, never `system`).

use std::path::PathBuf;

use super::{CommandRunner, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked};
use crate::atomic_write::write_atomically;

const LABEL: &str = "au.zentelligence.contextos-web";

/// Installs `contextos-web` as a `launchd` `LaunchAgent`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchdBackend;

fn plist_path(spec: &ServiceSpec) -> PathBuf {
    spec.home_dir
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist_contents(spec: &ServiceSpec) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{binary}</string>\n\
         \t\t<string>--config</string>\n\
         \t\t<string>{config}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        binary = xml_escape(&spec.binary_path.display().to_string()),
        config = xml_escape(&spec.web_config_path.display().to_string()),
    )
}

/// The current user's numeric id, via `id -u` rather than an unsafe
/// `libc::getuid()` call (`#![forbid(unsafe_code)]`), so it stays
/// assertable against the injected [`CommandRunner`] in tests.
fn current_uid(runner: &dyn CommandRunner) -> Result<String, ServiceError> {
    let args: [&str; 1] = ["-u"];
    let output = runner.run("id", &args).map_err(|source| ServiceError::Spawn {
        program: "id".to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        source,
    })?;
    if !output.success {
        return Err(ServiceError::CommandFailed {
            program: "id".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: output.stderr,
        });
    }
    let uid = output.stdout.trim();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return Err(ServiceError::UnexpectedOutput {
            program: "id".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            detail: format!("expected a numeric user id, found {:?}", output.stdout),
        });
    }
    Ok(uid.to_owned())
}

impl ServiceBackend for LaunchdBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = plist_path(spec);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ServiceError::Write {
                path: path.clone(),
                source,
            })?;
        }
        write_atomically(&path, plist_contents(spec).as_bytes()).map_err(|source| ServiceError::Write {
            path: path.clone(),
            source,
        })?;

        let uid = current_uid(runner)?;
        let domain = format!("gui/{uid}");
        let target = format!("{domain}/{LABEL}");
        let path_str = path.display().to_string();
        // Best-effort: bootout a previous load so re-install is idempotent.
        // `launchctl` fails this when nothing is loaded yet, which is the
        // common case on a first install; that failure is not itself an
        // install failure.
        let _ = runner.run("launchctl", &["bootout", &target]);
        run_checked(runner, "launchctl", &["bootstrap", &domain, &path_str])
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        let path = plist_path(spec);
        if !path.exists() {
            return Ok(UninstallOutcome::NotInstalled);
        }

        let uid = current_uid(runner)?;
        let target = format!("gui/{uid}/{LABEL}");
        // Best-effort, matching install: bootout can fail if the agent is
        // not currently loaded even though its plist is still on disk (for
        // example after a manual unload), which must not block removing
        // the file below.
        let _ = runner.run("launchctl", &["bootout", &target]);

        std::fs::remove_file(&path).map_err(|source| ServiceError::Remove {
            path: path.clone(),
            source,
        })?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        let path = plist_path(spec);
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }

        let uid = current_uid(runner)?;
        let target = format!("gui/{uid}/{LABEL}");
        let output = runner
            .run("launchctl", &["print", &target])
            .map_err(|source| ServiceError::Spawn {
                program: "launchctl".to_owned(),
                args: vec!["print".to_owned(), target],
                source,
            })?;
        Ok(ServiceStatus::Installed {
            running: output.success,
        })
    }
}

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
