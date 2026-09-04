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

#[test]
fn install_writes_the_unit_file_and_enables_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now

    SystemdUserBackend.install(&runner, &spec)?;

    let unit_path = spec
        .config_dir
        .join("systemd")
        .join("user")
        .join("contextos-web.service");
    let contents = std::fs::read_to_string(&unit_path)?;
    assert!(contents.contains(&spec.binary_path.display().to_string()));
    assert!(contents.contains(&spec.web_config_path.display().to_string()));
    assert!(contents.contains("WantedBy=default.target"));
    assert_eq!(
        runner.calls(),
        vec![
            (
                "systemctl".to_owned(),
                vec!["--user".to_owned(), "daemon-reload".to_owned()]
            ),
            (
                "systemctl".to_owned(),
                vec![
                    "--user".to_owned(),
                    "enable".to_owned(),
                    "--now".to_owned(),
                    "contextos-web.service".to_owned(),
                ]
            ),
        ]
    );
    Ok(())
}

#[test]
fn install_fails_when_enable_now_reports_a_non_zero_exit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_failure("unit not found");

    let result = SystemdUserBackend.install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })));
    Ok(())
}

#[test]
fn status_reports_not_installed_when_no_unit_file_exists() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::NotInstalled);
    assert!(
        runner.calls().is_empty(),
        "must not shell out when no unit file is present"
    );
    Ok(())
}

#[test]
fn status_reports_running_from_is_active_exit_success() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_success("active");

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed { running: true });
    Ok(())
}

#[test]
fn status_reports_not_running_from_is_active_exit_failure() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_failure("inactive");

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed { running: false });
    Ok(())
}

#[test]
fn uninstall_reports_not_installed_without_shelling_out_when_no_unit_file_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let outcome = SystemdUserBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert!(runner.calls().is_empty());
    Ok(())
}

#[test]
fn uninstall_disables_removes_the_unit_file_and_reloads() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload (install)
    runner.push_success(""); // enable --now (install)
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_success(""); // disable --now
    runner.push_success(""); // daemon-reload

    let outcome = SystemdUserBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    let unit_path = spec
        .config_dir
        .join("systemd")
        .join("user")
        .join("contextos-web.service");
    assert!(!unit_path.exists());
    Ok(())
}

#[test]
fn install_is_idempotent_over_an_existing_installation() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");
    runner.push_success("");
    SystemdUserBackend.install(&runner, &spec)?;

    runner.push_success("");
    runner.push_success("");
    let result = SystemdUserBackend.install(&runner, &spec);

    assert!(result.is_ok());
    Ok(())
}
