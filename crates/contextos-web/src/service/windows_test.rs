use tempfile::tempdir;

use super::*;
use crate::service::FakeCommandRunner;

fn spec(dir: &std::path::Path) -> ServiceSpec {
    ServiceSpec {
        binary_path: dir.join("contextos-web.exe"),
        web_config_path: dir.join("web.toml"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
    }
}

#[test]
fn install_creates_the_scheduled_task_with_a_logon_trigger() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");

    ScheduledTaskBackend.install(&runner, &spec)?;

    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (program, args) = &calls[0];
    assert_eq!(program, "schtasks");
    assert_eq!(args[0], "/Create");
    assert!(args.contains(&"ContextOS Web".to_owned()));
    assert!(args.contains(&"ONLOGON".to_owned()));
    assert!(args.contains(&"/F".to_owned()));
    let tr_index = args
        .iter()
        .position(|a| a == "/TR")
        .ok_or("expected /TR in the schtasks arguments")?
        + 1;
    assert!(args[tr_index].contains(&spec.binary_path.display().to_string()));
    assert!(args[tr_index].contains("--config"));
    Ok(())
}

#[test]
fn install_fails_when_schtasks_reports_a_non_zero_exit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_failure("access denied");

    let result = ScheduledTaskBackend.install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })));
    Ok(())
}

#[test]
fn status_reports_not_installed_when_query_fails() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_failure("ERROR: The system cannot find the file specified.");

    let status = ScheduledTaskBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::NotInstalled);
    Ok(())
}

#[test]
fn status_reports_running_when_query_output_mentions_running() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("TaskName: ContextOS Web\nStatus: Running\n");

    let status = ScheduledTaskBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed { running: true });
    Ok(())
}

#[test]
fn status_reports_not_running_when_query_output_does_not_mention_running() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("TaskName: ContextOS Web\nStatus: Ready\n");

    let status = ScheduledTaskBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed { running: false });
    Ok(())
}

#[test]
fn uninstall_reports_not_installed_without_deleting_when_query_fails() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_failure("ERROR: The system cannot find the file specified.");

    let outcome = ScheduledTaskBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert_eq!(
        runner.calls().len(),
        1,
        "must not call /Delete when the task does not exist"
    );
    Ok(())
}

#[test]
fn uninstall_deletes_an_existing_task() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("TaskName: ContextOS Web\nStatus: Ready\n");
    runner.push_success("");

    let outcome = ScheduledTaskBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    let calls = runner.calls();
    assert_eq!(calls[1].0, "schtasks");
    assert_eq!(calls[1].1[0], "/Delete");
    Ok(())
}
