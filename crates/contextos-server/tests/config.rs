use contextos_core::VaultSet;
use contextos_server::{
    Config, ConfigEnvironment, ConfigError, ConfigLoadInput, EmbeddingProvider, GraphBackendConfig,
    LogLevel, Transport,
};
use tempfile::tempdir;

#[test]
fn configuration_applies_documented_defaults_and_builds_vault_set()
-> Result<(), Box<dyn std::error::Error>> {
    // A prefixed builder, not the bare `tempdir()` default, so the
    // directory's basename is a valid RFC 3986 scheme token: building the
    // `VaultSet` derives the vault's name from that basename when the TOML
    // omits an explicit `name`, as it does here.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    assert_eq!(config.server.transports, vec![Transport::Stdio]);
    assert_eq!(config.server.log_level, LogLevel::Info);
    assert_eq!(config.server.http.bind, "127.0.0.1:7331");
    assert_eq!(config.vaults[0].limits.max_read_mb, 5);
    assert_eq!(config.vaults[0].limits.max_batch_files, 50);
    assert!(config.vaults[0].managed);
    assert_eq!(
        config.vaults[0].git.restore_exclude,
        ["memory/log", "memory/sessions", "memory/coding"]
            .map(std::path::PathBuf::from)
            .to_vec()
    );
    assert_eq!(
        config.vaults[0].search.embedding.provider,
        EmbeddingProvider::Local
    );
    assert_eq!(
        config.vaults[0].search.graph_backend,
        GraphBackendConfig::Fjall
    );
    assert_eq!(config.vaults[0].search.embedding.model_directory, None);
    assert_eq!(config.server.resource_link_threshold_kb, 5);
    assert_eq!(config.vaults[0].search.rebuild_budget_seconds, 25);
    assert_eq!(roots.len(), 1);
    Ok(())
}

/// FR-108: `[vault.search] graph_backend` accepts exactly `"serde"`,
/// `"fjall"`, and `"sqlite"`, additively alongside the existing `graph`
/// boolean rather than nested under it, so a config that sets `graph`
/// without `graph_backend` (the default test above) keeps parsing
/// unchanged.
#[test]
fn graph_backend_accepts_every_documented_value() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (toml_value, expected) in [
        ("serde", GraphBackendConfig::Serde),
        ("fjall", GraphBackendConfig::Fjall),
        ("sqlite", GraphBackendConfig::Sqlite),
    ] {
        let source = format!(
            r#"
            [[vault]]
            path = {:?}

            [vault.search]
            graph_backend = "{toml_value}"
            "#,
            vault.path()
        );

        let config = Config::try_from(source.as_str())?;
        assert_eq!(config.vaults[0].search.graph_backend, expected);
    }

    Ok(())
}

/// A `graph_backend` value outside the three documented options is a hard
/// configuration error at parse time, not a silently ignored or defaulted
/// field (the reject-by-default disposition `configuration.md` §3 already
/// states for unknown keys, extended here to an unknown enum value).
#[test]
fn an_unrecognised_graph_backend_value_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}

        [vault.search]
        graph_backend = "leveldb"
        "#,
        vault.path()
    );

    let Err(_error) = Config::try_from(source.as_str()) else {
        return Err("expected an unrecognised graph_backend value to be rejected".into());
    };

    Ok(())
}

/// `FR-96`: an explicit `name` in TOML threads through `VaultConfig` into
/// the resolved `VaultRoot`, not just the default-from-basename case the
/// test above covers.
#[test]
fn an_explicit_vault_name_threads_through_to_the_resolved_root()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        name = "mine"
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    assert_eq!(config.vaults[0].name.as_deref(), Some("mine"));
    let (_, root) = roots.root_by_name("mine").ok_or("mine should resolve")?;
    assert_eq!(root.name(), "mine");
    Ok(())
}

#[test]
fn rebuild_budget_seconds_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [vault.search]
        rebuild_budget_seconds = 60
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].search.rebuild_budget_seconds, 60);
    Ok(())
}

#[test]
fn embedding_model_directory_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.search.embedding]
        provider = "local"
        model_directory = "/opt/contextos/models/all-MiniLM-L6-v2"
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(
        config.vaults[0].search.embedding.model_directory,
        Some(std::path::PathBuf::from(
            "/opt/contextos/models/all-MiniLM-L6-v2"
        ))
    );
    Ok(())
}

#[test]
fn search_exclusions_default_independently_of_index_md_and_are_configurable()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let default_source = format!(
        r"
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );
    let default_config = Config::try_from(default_source.as_str())?;
    assert_eq!(
        default_config.vaults[0].search.exclude,
        default_config.vaults[0].index_md.exclude
    );

    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.index_md]
        exclude = ["private-notes"]
        [vault.search]
        exclude = ["not-searchable"]
        "#,
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].index_md.exclude, ["private-notes"]);
    assert_eq!(config.vaults[0].search.exclude, ["not-searchable"]);
    Ok(())
}

#[test]
fn git_restore_exclusions_are_configurable_as_an_active_list()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.git]
        restore_exclude = ["journal/private", "memory/log"]
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(
        config.vaults[0].git.restore_exclude,
        ["journal/private", "memory/log"]
            .map(std::path::PathBuf::from)
            .to_vec()
    );
    Ok(())
}

#[test]
fn resource_link_threshold_kb_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [server]
        resource_link_threshold_kb = 64
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.server.resource_link_threshold_kb, 64);
    Ok(())
}

#[test]
fn configuration_rejects_git_restore_exclusions_that_are_not_portable_relative_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.git]
        restore_exclude = ["../outside"]
        "#,
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(
        result,
        Err(ConfigError::InvalidRelativePath {
            field: "vault.git.restore_exclude",
            ..
        })
    ));
    Ok(())
}

/// `FR-96`: name validity and uniqueness are checked when the resolved
/// `VaultSet` is built, not by `Config::try_from`'s own `.validate()` (which
/// only checks limits and portable-relative-path fields); this is the
/// error path's own end-to-end proof, complementing the explicit-name
/// threading test above.
#[test]
fn duplicate_explicit_vault_names_are_rejected_when_the_vault_set_is_built()
-> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        name = "mine"
        [[vault]]
        path = {:?}
        name = "Mine"
        "#,
        first.path(),
        second.path()
    );

    let config = Config::try_from(source.as_str())?;
    let result = VaultSet::try_from(&config);

    assert!(matches!(result, Err(ConfigError::VaultSet { .. })));
    Ok(())
}

#[test]
fn configuration_rejects_unknown_keys() {
    let result = Config::try_from(
        r#"
        unexpected = true
        [[vault]]
        path = "/tmp"
        "#,
    );

    assert!(matches!(result, Err(ConfigError::Toml { .. })));
}

#[test]
fn configuration_requires_at_least_one_vault() {
    let result = Config::try_from("[server]\nlog_level = \"info\"");

    assert!(matches!(result, Err(ConfigError::NoVaults)));
}

#[test]
fn configuration_rejects_zero_limits() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [vault.limits]
        max_read_mb = 0
        max_batch_files = 0
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::InvalidLimit { .. })));
    Ok(())
}

#[test]
fn unmanaged_vault_retains_flag_in_domain_root() -> Result<(), Box<dyn std::error::Error>> {
    // See the same rationale in `configuration_applies_documented_defaults_and_builds_vault_set`.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        managed = false
        ",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    let managed = roots.iter().next().map(contextos_core::VaultRoot::managed);
    assert_eq!(managed, Some(false));
    Ok(())
}

#[test]
fn environment_values_override_file_values() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config_path = fixture.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[server]\nlog_level = \"warn\"\n[server.http]\ntoken = \"file-token\"\n[[vault]]\npath = {vault:?}\n"
        ),
    )?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        environment: ConfigEnvironment {
            token: Some("environment-token".to_owned()),
            log_level: Some("debug".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.log_level, LogLevel::Debug);
    assert_eq!(config.server.http.token, "environment-token");
    Ok(())
}

#[test]
fn cli_log_level_overrides_environment_value() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let config = Config::try_from(ConfigLoadInput {
        cli_vaults: vec![vault],
        cli_log_level: Some(LogLevel::Trace),
        environment: ConfigEnvironment {
            log_level: Some("debug".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.log_level, LogLevel::Trace);
    Ok(())
}

#[test]
fn missing_config_file_is_ignored_when_cli_supplies_a_vault()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(fixture.path().join("missing.toml")),
        cli_vaults: vec![vault.clone()],
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].path, vault);
    Ok(())
}

#[test]
fn missing_config_file_without_another_vault_source_is_actionable()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;

    let result = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(fixture.path().join("missing.toml")),
        ..ConfigLoadInput::default()
    });

    assert!(matches!(result, Err(ConfigError::NoVaults)));
    Ok(())
}

#[test]
fn cli_config_path_precedes_environment_and_platform_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let cli_vault = fixture.path().join("cli-vault");
    let environment_vault = fixture.path().join("environment-vault");
    let platform_vault = fixture.path().join("platform-vault");
    for vault in [&cli_vault, &environment_vault, &platform_vault] {
        std::fs::create_dir(vault)?;
    }
    let cli_path = fixture.path().join("cli.toml");
    let environment_path = fixture.path().join("environment.toml");
    let platform_path = fixture.path().join("platform.toml");
    std::fs::write(&cli_path, format!("[[vault]]\npath = {cli_vault:?}\n"))?;
    std::fs::write(
        &environment_path,
        format!("[[vault]]\npath = {environment_vault:?}\n"),
    )?;
    std::fs::write(
        &platform_path,
        format!("[[vault]]\npath = {platform_vault:?}\n"),
    )?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(cli_path),
        default_config_path: Some(platform_path),
        environment: ConfigEnvironment {
            config_path: Some(environment_path),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.vaults[0].path, cli_vault);
    Ok(())
}

#[test]
fn invalid_environment_log_level_is_a_typed_startup_error() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let result = Config::try_from(ConfigLoadInput {
        cli_vaults: vec![vault],
        environment: ConfigEnvironment {
            log_level: Some("verbose".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    });

    assert!(matches!(
        result,
        Err(ConfigError::InvalidEnvironmentValue {
            variable: "CONTEXTOS_MCP_LOG_LEVEL",
            ..
        })
    ));
    Ok(())
}

#[test]
fn documented_example_configuration_parses_under_deny_unknown_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let example_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/config.example.toml"
    );
    let source = std::fs::read_to_string(example_path)?;

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults.len(), 2);
    Ok(())
}
