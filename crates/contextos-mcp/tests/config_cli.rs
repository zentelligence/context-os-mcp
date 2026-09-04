use std::fs;
use std::process::Command;

use tempfile::tempdir;

fn contextos(config: &std::path::Path, args: &[&str]) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?])
        .args(args)
        .output()?)
}

#[test]
fn config_vault_add_creates_a_missing_config_file_and_adds_a_managed_vault() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    fs::create_dir(&vault)?;
    let config = fixture.path().join("nested").join("config.toml");

    let output = contextos(
        &config,
        &[
            "config",
            "vault",
            "add",
            "mine",
            vault.to_str().ok_or("non-UTF-8 vault")?,
        ],
    )?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("Added vault \"mine\""));
    let written = fs::read_to_string(&config)?;
    assert!(written.contains("name = \"mine\""));
    assert!(!written.contains("managed = false"));
    Ok(())
}

#[test]
fn config_vault_add_with_unmanaged_writes_managed_false() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");

    let output = contextos(
        &config,
        &[
            "config",
            "vault",
            "add",
            "mine",
            vault.to_str().ok_or("non-UTF-8 vault")?,
            "--unmanaged",
        ],
    )?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let written = fs::read_to_string(&config)?;
    assert!(written.contains("managed = false"));
    Ok(())
}

#[test]
fn config_vault_add_rejects_a_nonexistent_path_and_creates_no_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let missing = fixture.path().join("does-not-exist");
    let config = fixture.path().join("config.toml");

    let output = contextos(
        &config,
        &[
            "config",
            "vault",
            "add",
            "mine",
            missing.to_str().ok_or("non-UTF-8 path")?,
        ],
    )?;

    assert!(!output.status.success());
    assert!(!config.exists());
    Ok(())
}

#[test]
fn config_vault_list_reports_no_vaults_for_a_missing_config_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let config = fixture.path().join("config.toml");

    let output = contextos(&config, &["config", "vault", "list"])?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("No vaults configured"));
    Ok(())
}

#[test]
fn config_vault_add_list_remove_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    let other_vault = fixture.path().join("other-vault");
    fs::create_dir(&vault)?;
    fs::create_dir(&other_vault)?;
    let config = fixture.path().join("config.toml");

    // Two vaults, so removing one still leaves a valid (non-empty)
    // configuration; removing the *last* vault is covered separately below.
    for (name, path) in [("mine", &vault), ("other", &other_vault)] {
        let add = contextos(
            &config,
            &["config", "vault", "add", name, path.to_str().ok_or("non-UTF-8 vault")?],
        )?;
        assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));
    }

    let list = contextos(&config, &["config", "vault", "list"])?;
    let list_stdout = String::from_utf8(list.stdout)?;
    assert!(list.status.success(), "{list_stdout}");
    assert!(list_stdout.contains("mine"));
    assert!(list_stdout.contains(vault.to_str().ok_or("non-UTF-8 vault")?));
    assert!(list_stdout.contains("other"));

    let remove = contextos(&config, &["config", "vault", "remove", "mine"])?;
    let remove_stdout = String::from_utf8(remove.stdout)?;
    assert!(remove.status.success(), "{remove_stdout}");
    assert!(remove_stdout.contains("Removed vault \"mine\""));

    let list_after = contextos(&config, &["config", "vault", "list"])?;
    let list_after_stdout = String::from_utf8(list_after.stdout)?;
    assert!(!list_after_stdout.contains("mine"));
    assert!(list_after_stdout.contains("other"));
    Ok(())
}

#[test]
fn config_vault_remove_rejects_removing_the_last_vault() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    let add = contextos(
        &config,
        &[
            "config",
            "vault",
            "add",
            "mine",
            vault.to_str().ok_or("non-UTF-8 vault")?,
        ],
    )?;
    assert!(add.status.success(), "{}", String::from_utf8_lossy(&add.stderr));

    let output = contextos(&config, &["config", "vault", "remove", "mine"])?;

    assert!(!output.status.success());
    let list = contextos(&config, &["config", "vault", "list"])?;
    assert!(String::from_utf8(list.stdout)?.contains("mine"));
    Ok(())
}

#[test]
fn config_mcp_register_then_status_then_deregister_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let config = fixture.path().join("config.toml");
    let host_config = fixture.path().join("claude_desktop_config.json");

    // `--force` throughout: this test proves the CLI's JSON wiring and
    // round trip, not the running-process check itself, which is already
    // covered deterministically at the unit level (`host_registration_test.rs`)
    // via an injected fake detector. Without it, this test is genuinely
    // flaky in any environment where a real process happens to match the
    // "claude" substring, including this very CLI's own host process.
    let register = contextos(
        &config,
        &[
            "config",
            "mcp",
            "register",
            "--host",
            "claude-desktop",
            "--config-path",
            host_config.to_str().ok_or("non-UTF-8 host config")?,
            "--force",
        ],
    )?;
    assert!(
        register.status.success(),
        "{}",
        String::from_utf8_lossy(&register.stderr)
    );
    let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&host_config)?)?;
    assert!(written["mcpServers"]["contextos"]["command"].is_string());
    assert_eq!(written["mcpServers"]["contextos"]["args"][0], "--config");

    let status = contextos(
        &config,
        &[
            "config",
            "mcp",
            "status",
            "--host",
            "claude-desktop",
            "--config-path",
            host_config.to_str().ok_or("non-UTF-8 host config")?,
        ],
    )?;
    let status_stdout = String::from_utf8(status.stdout)?;
    assert!(status.status.success(), "{status_stdout}");
    assert!(status_stdout.contains("is registered"));

    let deregister = contextos(
        &config,
        &[
            "config",
            "mcp",
            "deregister",
            "--host",
            "claude-desktop",
            "--config-path",
            host_config.to_str().ok_or("non-UTF-8 host config")?,
            "--force",
        ],
    )?;
    assert!(
        deregister.status.success(),
        "{}",
        String::from_utf8_lossy(&deregister.stderr)
    );

    let status_after = contextos(
        &config,
        &[
            "config",
            "mcp",
            "status",
            "--host",
            "claude-desktop",
            "--config-path",
            host_config.to_str().ok_or("non-UTF-8 host config")?,
        ],
    )?;
    let status_after_stdout = String::from_utf8(status_after.stdout)?;
    assert!(status_after.status.success(), "{status_after_stdout}");
    assert!(status_after_stdout.contains("is not registered"));
    Ok(())
}

#[test]
fn config_mcp_register_preserves_unrelated_host_config_content() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let config = fixture.path().join("config.toml");
    let host_config = fixture.path().join("claude_desktop_config.json");
    fs::write(
        &host_config,
        r#"{"someOtherKey": "keep me", "mcpServers": {"another-server": {"command": "other", "args": []}}}"#,
    )?;

    // `--force` for the same reason as the round-trip test above: this
    // test proves content preservation, not the running-process check.
    let register = contextos(
        &config,
        &[
            "config",
            "mcp",
            "register",
            "--host",
            "claude-desktop",
            "--config-path",
            host_config.to_str().ok_or("non-UTF-8 host config")?,
            "--force",
        ],
    )?;
    assert!(
        register.status.success(),
        "{}",
        String::from_utf8_lossy(&register.stderr)
    );

    let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&host_config)?)?;
    assert_eq!(written["someOtherKey"], "keep me");
    assert_eq!(written["mcpServers"]["another-server"]["command"], "other");
    assert!(written["mcpServers"]["contextos"]["command"].is_string());
    Ok(())
}

#[test]
fn config_vault_remove_rejects_an_unknown_name() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    fs::create_dir(&vault)?;
    let config = fixture.path().join("config.toml");
    contextos(
        &config,
        &[
            "config",
            "vault",
            "add",
            "mine",
            vault.to_str().ok_or("non-UTF-8 vault")?,
        ],
    )?;

    let output = contextos(&config, &["config", "vault", "remove", "nope"])?;

    assert!(!output.status.success());
    Ok(())
}
