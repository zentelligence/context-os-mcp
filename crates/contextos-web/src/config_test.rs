use std::fs;

use tempfile::tempdir;

use super::*;

fn write(dir: &std::path::Path, name: &str, contents: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(name);
    fs::write(&path, contents)?;
    Ok(path)
}

#[test]
fn defaults_apply_when_the_server_table_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let config = load_web_config(&path)?;

    assert_eq!(config.server.bind, "127.0.0.1:7332");
    assert_eq!(config.server.static_dir, std::path::Path::new("./static"));
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].name(), "contextos");
    Ok(())
}

#[test]
fn a_stdio_mcp_server_entry_parses_command_and_args() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
            args = ["--config", "/tmp/config.toml", "--stdio"]
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Stdio { name, command, args } = &config.mcp_servers[0] else {
        return Err("expected a stdio entry".into());
    };
    assert_eq!(name, "contextos");
    assert_eq!(command, "contextos-mcp");
    assert_eq!(args, &["--config", "/tmp/config.toml", "--stdio"]);
    Ok(())
}

#[test]
fn an_http_mcp_server_entry_parses_endpoint_and_token_env() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [[mcp_server]]
            transport = "http"
            name = "some-other-server"
            endpoint = "http://127.0.0.1:9000"
            token_env = "SOME_OTHER_SERVER_TOKEN"
        "#,
    )?;

    let config = load_web_config(&path)?;

    let McpServerConfig::Http {
        name,
        endpoint,
        token_env,
    } = &config.mcp_servers[1]
    else {
        return Err("expected an http entry".into());
    };
    assert_eq!(name, "some-other-server");
    assert_eq!(endpoint, "http://127.0.0.1:9000");
    assert_eq!(token_env.as_deref(), Some("SOME_OTHER_SERVER_TOKEN"));
    Ok(())
}

#[test]
fn an_unknown_top_level_key_is_a_hard_error() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            unknown_key = "surprise"

            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::Toml { .. })));
    Ok(())
}

#[test]
fn a_non_loopback_bind_is_rejected() {
    let result = validate_loopback_bind("0.0.0.0:7332");
    assert!(matches!(result, Err(WebConfigError::NonLoopbackBind { .. })));
}

#[test]
fn a_loopback_bind_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    validate_loopback_bind("127.0.0.1:7332")?;
    validate_loopback_bind("[::1]:7332")?;
    Ok(())
}

#[test]
fn an_unparseable_bind_is_rejected() {
    let result = validate_loopback_bind("not-a-socket-address");
    assert!(matches!(result, Err(WebConfigError::InvalidBindAddress { .. })));
}

#[test]
fn a_web_toml_with_a_non_loopback_bind_fails_to_load() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [server]
            bind = "0.0.0.0:7332"

            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::NonLoopbackBind { .. })));
    Ok(())
}

#[test]
fn duplicate_mcp_server_names_are_rejected_case_insensitively() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        r#"
            [[mcp_server]]
            transport = "stdio"
            name = "contextos"
            command = "contextos-mcp"

            [[mcp_server]]
            transport = "stdio"
            name = "Contextos"
            command = "contextos-mcp-2"
        "#,
    )?;

    let result = load_web_config(&path);

    assert!(matches!(result, Err(WebConfigError::DuplicateMcpServerName { .. })));
    Ok(())
}

#[test]
fn a_missing_web_toml_file_is_a_clear_read_error() {
    let missing = std::path::Path::new("/does/not/exist/web.toml");
    let result = load_web_config(missing);
    assert!(matches!(result, Err(WebConfigError::Read { .. })));
}

#[test]
fn load_vault_set_agrees_with_contextos_mcps_own_vault_set() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let vault_a = dir.path().join("vault-a");
    let vault_b = dir.path().join("vault-b");
    fs::create_dir_all(&vault_a)?;
    fs::create_dir_all(&vault_b)?;
    let config_path = write(
        dir.path(),
        "config.toml",
        &format!(
            r#"
                [[vault]]
                path = {vault_a:?}
                name = "mine"

                [[vault]]
                path = {vault_b:?}
                name = "family"
                managed = false
            "#
        ),
    )?;

    let web_vaults = load_vault_set(&config_path)?;

    let mcp_config = contextos_mcp::Config::try_from(fs::read_to_string(&config_path)?.as_str())?;
    let mcp_vaults = contextos_core::VaultSet::try_from(&mcp_config)?;

    assert_eq!(web_vaults.len(), mcp_vaults.len());
    for root in &mcp_vaults {
        let (_, web_root) = web_vaults
            .root_by_name(root.name())
            .ok_or_else(|| format!("contextos-web did not resolve vault {:?}", root.name()))?;
        assert_eq!(web_root.path(), root.path());
        assert_eq!(web_root.managed(), root.managed());
    }
    Ok(())
}

#[test]
fn load_vault_set_rejects_a_config_with_no_vaults() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "config.toml", "[server]\n")?;

    let result = load_vault_set(&path);

    assert!(result.is_err());
    Ok(())
}

#[test]
fn current_appearance_reads_theme_font_and_size_from_server_ui() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        "[server.ui]\ntheme = \"dark\"\nfont = \"serif\"\nsize = \"large\"\n",
    )?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.theme.as_deref(), Some("dark"));
    assert_eq!(appearance.font.as_deref(), Some("serif"));
    assert_eq!(appearance.size.as_deref(), Some("large"));
    Ok(())
}

#[test]
fn current_appearance_ignores_a_non_string_value() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\ntheme = 42\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.theme, None);
    Ok(())
}

#[test]
fn current_appearance_degrades_to_default_when_web_toml_is_unreadable() {
    let appearance = current_appearance(std::path::Path::new("/does/not/exist/web.toml"));
    assert_eq!(appearance, Appearance::default());
}

#[test]
fn current_appearance_is_empty_when_no_server_ui_table_is_present() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance, Appearance::default());
    Ok(())
}
