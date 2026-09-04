use std::process::Command;

use tempfile::tempdir;

#[test]
fn phase_2_doctor_validates_configuration_and_reports_a_healthy_vault() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Configuration | PASS"));
    // The doctor reports the resolved vault path, not the raw one
    // constructed here: they can differ, for example under Windows 8.3
    // short-name generation.
    let resolved_vault = dunce::canonicalize(&vault)?;
    assert!(stdout.contains(resolved_vault.to_str().ok_or("non-UTF-8 vault")?));
    assert!(stdout.contains("Managed indexes | PASS | disabled"));
    assert!(stdout.contains("Git recovery | PASS | disabled"));
    Ok(())
}

#[test]
fn stage_4_doctor_reports_semantic_search_disabled_by_default() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Semantic search | PASS | disabled"));
    Ok(())
}

#[test]
fn stage_4_doctor_fails_with_an_action_when_semantic_is_enabled_without_a_model_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.index_md]
            enabled = false
            [vault.git]
            enabled = false
            [vault.search]
            semantic = true
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "doctor"])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("Semantic search | FAIL"));
    assert!(stdout.contains("model_directory"));
    assert!(stdout.contains("Action: Correct [vault.search.embedding]"));
    Ok(())
}

#[test]
fn phase_2_doctor_returns_failure_with_an_action_for_missing_indexes() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["doctor", "--config", config.to_str().ok_or("non-UTF-8 config")?])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("Managed indexes | FAIL"));
    assert!(stdout.contains("Action: Call vault_index_rebuild"));
    Ok(())
}

#[test]
fn doctor_resolve_flag_rebuilds_a_stale_index_and_reports_it_healthy() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(vault.join("index.md").exists());
    assert!(stdout.contains("Resolved: Managed indexes"));
    assert!(stdout.contains("Managed indexes | PASS"));
    Ok(())
}

#[test]
fn doctor_resolve_dry_run_previews_without_writing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let data_home = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r"
            [[vault]]
            path = {vault:?}
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args([
            "--config",
            config.to_str().ok_or("non-UTF-8 config")?,
            "doctor",
            "--resolve",
            "--dry-run",
        ])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(!output.status.success(), "{stdout}");
    assert!(!vault.join("index.md").exists());
    assert!(stdout.contains("Would resolve: Managed indexes"));
    assert!(stdout.contains("Managed indexes | FAIL"));
    Ok(())
}
