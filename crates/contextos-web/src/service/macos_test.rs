use tempfile::tempdir;

use super::*;
use crate::service::FakeCommandRunner;

fn spec(dir: &std::path::Path) -> ServiceSpec {
    ServiceSpec {
        binary_path: dir.join("contextos-web"),
        web_config_path: dir.join("web.toml"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
    }
}

fn plist_path(spec: &ServiceSpec) -> std::path::PathBuf {
    spec.home_dir
        .join("Library")
        .join("LaunchAgents")
        .join("au.zentelligence.contextos-web.plist")
}

#[test]
fn install_writes_the_plist_and_bootstraps_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("501"); // id -u
    runner.push_failure("not loaded"); // best-effort bootout
    runner.push_success(""); // bootstrap

    LaunchdBackend.install(&runner, &spec)?;

    let contents = std::fs::read_to_string(plist_path(&spec))?;
    assert!(contents.contains(&spec.binary_path.display().to_string()));
    assert!(contents.contains("--config"));
    assert!(contents.contains("au.zentelligence.contextos-web"));
    assert!(contents.contains("<key>RunAtLoad</key>"));

    let calls = runner.calls();
    assert_eq!(calls[0], ("id".to_owned(), vec!["-u".to_owned()]));
    assert_eq!(calls[1].0, "launchctl");
    assert_eq!(calls[1].1[0], "bootout");
    assert_eq!(calls[2].0, "launchctl");
    assert_eq!(calls[2].1[0], "bootstrap");
    assert_eq!(calls[2].1[1], "gui/501");
    Ok(())
}

#[test]
fn install_fails_when_id_u_reports_a_non_numeric_uid() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("not-a-number");

    let result = LaunchdBackend.install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::UnexpectedOutput { .. })));
    Ok(())
}

#[test]
fn install_fails_when_bootstrap_reports_a_non_zero_exit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("501");
    runner.push_failure("not loaded");
    runner.push_failure("bootstrap failed");

    let result = LaunchdBackend.install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })));
    Ok(())
}

#[test]
fn status_reports_not_installed_when_no_plist_exists() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let status = LaunchdBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::NotInstalled);
    assert!(runner.calls().is_empty());
    Ok(())
}

#[test]
fn status_reports_running_from_print_exit_success() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("501");
    runner.push_failure("");
    runner.push_success("");
    LaunchdBackend.install(&runner, &spec)?;

    runner.push_success("501");
    runner.push_success("state = running");

    let status = LaunchdBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed { running: true });
    Ok(())
}

#[test]
fn uninstall_reports_not_installed_without_shelling_out_when_no_plist_exists() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let outcome = LaunchdBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert!(runner.calls().is_empty());
    Ok(())
}

#[test]
fn uninstall_removes_the_plist_even_when_bootout_fails() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("501");
    runner.push_failure("");
    runner.push_success("");
    LaunchdBackend.install(&runner, &spec)?;

    runner.push_success("501");
    runner.push_failure("not loaded");
    let outcome = LaunchdBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    assert!(!plist_path(&spec).exists());
    Ok(())
}
