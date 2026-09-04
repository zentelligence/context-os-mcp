use tempfile::tempdir;

use super::*;

/// A [`DetectsRunningProcesses`] test double returning a fixed answer,
/// standing in for `SystemProcessDetector` so `HostRunning`/proceed-with-
/// `--force` behaviour is deterministic and does not depend on any real
/// process actually being named "claude".
struct FixedDetector(bool);

impl DetectsRunningProcesses for FixedDetector {
    fn is_running(&self, _name_needle: &str) -> bool {
        self.0
    }
}

fn entry() -> RegisteredServer {
    RegisteredServer {
        command: "/usr/local/bin/contextos".to_owned(),
        args: vec!["--config".to_owned(), "/home/op/config.toml".to_owned()],
    }
}

#[test]
fn status_reports_not_registered_for_a_missing_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");

    assert_eq!(status(&path)?, RegistrationStatus::NotRegistered);
    Ok(())
}

#[test]
fn register_creates_a_missing_file_with_the_expected_entry() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let detector = FixedDetector(false);

    register(&path, &entry(), &detector, false)?;

    assert_eq!(status(&path)?, RegistrationStatus::Registered(entry()));
    Ok(())
}

#[test]
fn register_preserves_unrelated_keys_and_other_servers() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    std::fs::write(
        &path,
        r#"{"unrelatedTopLevelKey": 42, "mcpServers": {"other-server": {"command": "other", "args": []}}}"#,
    )?;
    let detector = FixedDetector(false);

    register(&path, &entry(), &detector, false)?;

    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(written["unrelatedTopLevelKey"], 42);
    assert_eq!(written["mcpServers"]["other-server"]["command"], "other");
    assert_eq!(written["mcpServers"]["contextos"]["command"], entry().command);
    Ok(())
}

#[test]
fn register_refuses_to_write_while_the_host_is_running_without_force() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let detector = FixedDetector(true);

    let result = register(&path, &entry(), &detector, false);

    assert!(matches!(result, Err(HostRegistrationError::HostRunning)));
    assert!(!path.exists());
    Ok(())
}

#[test]
fn register_proceeds_while_the_host_is_running_when_forced() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let detector = FixedDetector(true);

    register(&path, &entry(), &detector, true)?;

    assert_eq!(status(&path)?, RegistrationStatus::Registered(entry()));
    Ok(())
}

#[test]
fn register_writes_a_timestamped_backup_of_an_existing_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let original = r#"{"mcpServers": {}}"#;
    std::fs::write(&path, original)?;
    let detector = FixedDetector(false);

    register(&path, &entry(), &detector, false)?;

    let backups: Vec<_> = std::fs::read_dir(fixture.path())?
        .filter_map(Result::ok)
        .filter(|dir_entry| {
            dir_entry
                .file_name()
                .to_string_lossy()
                .starts_with("claude_desktop_config.json.bak-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read_to_string(backups[0].path())?, original);
    Ok(())
}

#[test]
fn register_rejects_malformed_pre_existing_json_and_writes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    std::fs::write(&path, "{not valid json")?;
    let detector = FixedDetector(false);

    let result = register(&path, &entry(), &detector, false);

    assert!(matches!(result, Err(HostRegistrationError::InvalidJson { .. })));
    assert_eq!(std::fs::read_to_string(&path)?, "{not valid json");
    let backups: Vec<_> = std::fs::read_dir(fixture.path())?
        .filter_map(Result::ok)
        .filter(|dir_entry| dir_entry.file_name().to_string_lossy().contains(".bak-"))
        .collect();
    assert!(backups.is_empty());
    Ok(())
}

#[test]
fn deregister_removes_only_the_contextos_entry() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let detector = FixedDetector(false);
    register(&path, &entry(), &detector, false)?;

    let outcome = deregister(&path, &detector, false)?;

    assert_eq!(outcome, DeregisterOutcome::Removed);
    assert_eq!(status(&path)?, RegistrationStatus::NotRegistered);
    Ok(())
}

#[test]
fn deregister_preserves_other_servers_entries() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    std::fs::write(
        &path,
        r#"{"mcpServers": {"other-server": {"command": "other", "args": []}, "contextos": {"command": "c", "args": []}}}"#,
    )?;
    let detector = FixedDetector(false);

    deregister(&path, &detector, false)?;

    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(written["mcpServers"]["other-server"]["command"], "other");
    assert!(written["mcpServers"].get("contextos").is_none());
    Ok(())
}

#[test]
fn deregister_reports_not_registered_for_a_missing_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let detector = FixedDetector(false);

    let outcome = deregister(&path, &detector, false)?;

    assert_eq!(outcome, DeregisterOutcome::NotRegistered);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn deregister_refuses_to_write_while_the_host_is_running_without_force() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let path = fixture.path().join("claude_desktop_config.json");
    let running = FixedDetector(true);
    register(&path, &entry(), &FixedDetector(false), false)?;

    let result = deregister(&path, &running, false);

    assert!(matches!(result, Err(HostRegistrationError::HostRunning)));
    assert_eq!(status(&path)?, RegistrationStatus::Registered(entry()));
    Ok(())
}

#[test]
fn system_process_detector_finds_a_genuinely_running_process_by_name_substring()
-> Result<(), Box<dyn std::error::Error>> {
    // Genuine OS-level detection, not a fake: spawns a real second process
    // (this same test binary, re-invoked to run a no-op test) and confirms
    // `SystemProcessDetector` finds it by a substring of its own binary
    // name -- the identical mechanism `is_claude_desktop_running` uses with
    // the needle "claude", proven against a real process rather than an
    // assumption about Claude Desktop's own process name.
    let exe = std::env::current_exe()?;
    let stem = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("non-UTF-8 test binary name")?;
    let needle: String = stem.chars().take(8).collect();

    let mut child = std::process::Command::new(&exe)
        .args(["--exact", "system_process_detector_child", "--ignored", "--nocapture"])
        .spawn()?;

    let detector = SystemProcessDetector;
    let found = detector.is_running(&needle);

    child.kill()?;
    child.wait()?;
    assert!(found, "expected to find a process matching {needle:?}");
    Ok(())
}

#[test]
#[ignore = "only run as the spawned child of system_process_detector_finds_a_genuinely_running_process_by_name_substring"]
fn system_process_detector_child() {
    std::thread::sleep(std::time::Duration::from_secs(30));
}
