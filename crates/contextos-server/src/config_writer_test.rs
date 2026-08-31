use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn add_vault_to_an_empty_document_produces_a_valid_config() -> Result<(), Box<dyn std::error::Error>>
{
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();

    document.add_vault("mine", vault_dir.path(), true)?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].path, vault_dir.path());
    assert_eq!(config.vaults[0].name.as_deref(), Some("mine"));
    Ok(())
}

#[test]
fn add_vault_preserves_hand_written_comments_in_an_existing_document()
-> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let source = format!(
        "# a hand-written operator comment\n[[vault]]\npath = {:?}\nname = \"first\"\n",
        first_dir.path()
    );
    let mut document = ConfigDocument::parse(&source)?;

    document.add_vault("second", second_dir.path(), true)?;

    let rendered = document.render();
    assert!(rendered.contains("# a hand-written operator comment"));
    let config = Config::try_from(rendered.as_str())?;
    assert_eq!(config.vaults.len(), 2);
    Ok(())
}

#[test]
fn add_vault_omits_managed_when_true_but_writes_it_when_false()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();

    document.add_vault("mine", vault_dir.path(), false)?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(!config.vaults[0].managed);
    Ok(())
}

#[test]
fn add_vault_rejects_a_duplicate_name_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", first_dir.path(), true)?;
    let before = document.render();

    let result = document.add_vault("mine", second_dir.path(), true);

    assert!(result.is_err());
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn add_vault_rejects_a_nonexistent_path_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let missing = vault_dir.path().join("does-not-exist");
    let mut document = ConfigDocument::new();
    let before = document.render();

    let result = document.add_vault("mine", &missing, true);

    assert!(result.is_err());
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn remove_vault_removes_the_named_vault_and_keeps_the_rest()
-> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("first", first_dir.path(), true)?;
    document.add_vault("second", second_dir.path(), true)?;

    document.remove_vault("first")?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].name.as_deref(), Some("second"));
    Ok(())
}

#[test]
fn remove_vault_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("Mine", first_dir.path(), true)?;
    document.add_vault("other", second_dir.path(), true)?;

    document.remove_vault("mine")?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.vaults.len(), 1);
    Ok(())
}

#[test]
fn remove_vault_errors_on_an_unknown_name_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.remove_vault("nope");

    assert!(matches!(
        result,
        Err(ConfigWriterError::UnknownVaultName { .. })
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn remove_vault_rejects_removing_the_last_vault_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.remove_vault("mine");

    assert!(result.is_err());
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn vaults_reports_nothing_for_an_empty_document() {
    let document = ConfigDocument::new();

    assert_eq!(document.vaults(), Vec::new());
}

#[test]
fn vaults_reports_every_configured_vault_in_file_order() -> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("first", first_dir.path(), true)?;
    document.add_vault("second", second_dir.path(), false)?;

    let vaults = document.vaults();

    assert_eq!(vaults.len(), 2);
    assert_eq!(vaults[0].name, "first");
    assert_eq!(vaults[0].path, first_dir.path());
    assert!(vaults[0].managed);
    assert_eq!(vaults[1].name, "second");
    assert!(!vaults[1].managed);
    Ok(())
}

#[test]
fn enable_semantic_search_sets_semantic_and_model_directory_on_the_named_vault()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    document.enable_semantic_search("mine", model_dir.path())?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(config.vaults[0].search.semantic);
    assert_eq!(
        config.vaults[0].search.embedding.model_directory.as_deref(),
        Some(model_dir.path())
    );
    Ok(())
}

#[test]
fn enable_semantic_search_is_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("Mine", vault_dir.path(), true)?;

    document.enable_semantic_search("mine", model_dir.path())?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(config.vaults[0].search.semantic);
    Ok(())
}

#[test]
fn enable_semantic_search_leaves_other_vaults_untouched() -> Result<(), Box<dyn std::error::Error>>
{
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("first", first_dir.path(), true)?;
    document.add_vault("second", second_dir.path(), true)?;

    document.enable_semantic_search("first", model_dir.path())?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(config.vaults[0].search.semantic);
    assert!(!config.vaults[1].search.semantic);
    Ok(())
}

#[test]
fn enable_semantic_search_errors_on_an_unknown_name_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.enable_semantic_search("nope", model_dir.path());

    assert!(matches!(
        result,
        Err(ConfigWriterError::UnknownVaultName { .. })
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn enable_semantic_search_errors_on_an_empty_document_and_leaves_it_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    let before = document.render();

    let result = document.enable_semantic_search("mine", model_dir.path());

    assert!(matches!(
        result,
        Err(ConfigWriterError::UnknownVaultName { .. })
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn update_vault_renames_re_roots_and_changes_managed() -> Result<(), Box<dyn std::error::Error>> {
    let old_dir = tempdir()?;
    let new_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", old_dir.path(), true)?;

    document.update_vault("mine", "renamed", new_dir.path(), false)?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].name.as_deref(), Some("renamed"));
    assert_eq!(config.vaults[0].path, new_dir.path());
    assert!(!config.vaults[0].managed);
    Ok(())
}

#[test]
fn update_vault_reapplying_the_same_values_is_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    document.update_vault("mine", "mine", vault_dir.path(), true)?;

    let vaults = document.vaults();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].name, "mine");
    assert_eq!(vaults[0].path, vault_dir.path());
    assert!(vaults[0].managed);
    Ok(())
}

#[test]
fn update_vault_leaves_other_vaults_untouched() -> Result<(), Box<dyn std::error::Error>> {
    let first_dir = tempdir()?;
    let second_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("first", first_dir.path(), true)?;
    document.add_vault("second", second_dir.path(), true)?;

    document.update_vault("first", "renamed", first_dir.path(), true)?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.vaults.len(), 2);
    assert_eq!(config.vaults[1].name.as_deref(), Some("second"));
    Ok(())
}

#[test]
fn update_vault_errors_on_an_unknown_name_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.update_vault("nope", "nope", vault_dir.path(), true);

    assert!(matches!(
        result,
        Err(ConfigWriterError::UnknownVaultName { .. })
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn disable_semantic_search_turns_off_semantic_and_keeps_the_model_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    document.enable_semantic_search("mine", model_dir.path())?;

    document.disable_semantic_search("mine")?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(!config.vaults[0].search.semantic);
    let vaults = document.vaults();
    assert_eq!(vaults[0].model_directory.as_deref(), Some(model_dir.path()));
    Ok(())
}

#[test]
fn disable_semantic_search_on_a_vault_with_no_search_table_is_a_no_op()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    document.disable_semantic_search("mine")?;

    let config = Config::try_from(document.render().as_str())?;
    assert!(!config.vaults[0].search.semantic);
    Ok(())
}

#[test]
fn disable_semantic_search_errors_on_an_unknown_name_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.disable_semantic_search("nope");

    assert!(matches!(
        result,
        Err(ConfigWriterError::UnknownVaultName { .. })
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn vaults_reports_semantic_state_and_model_directory() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let model_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    document.enable_semantic_search("mine", model_dir.path())?;

    let vaults = document.vaults();

    assert!(vaults[0].semantic);
    assert_eq!(vaults[0].model_directory.as_deref(), Some(model_dir.path()));
    Ok(())
}

#[test]
fn vaults_reports_semantic_false_and_no_model_directory_by_default()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    let vaults = document.vaults();

    assert!(!vaults[0].semantic);
    assert_eq!(vaults[0].model_directory, None);
    Ok(())
}

#[test]
fn server_settings_reports_schema_defaults_for_an_empty_document() {
    let document = ConfigDocument::new();

    let settings = document.server_settings();

    assert_eq!(settings.transports, vec!["stdio".to_owned()]);
    assert_eq!(settings.log_level, "info");
    assert_eq!(settings.log_file, "");
}

#[test]
fn set_server_settings_writes_non_default_values() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    document.set_server_settings(
        &["stdio".to_owned(), "http".to_owned()],
        "debug",
        "/var/log/contextos.log",
    )?;

    let config = Config::try_from(document.render().as_str())?;
    assert_eq!(config.server.transports.len(), 2);
    assert_eq!(config.server.log_file, "/var/log/contextos.log");
    let settings = document.server_settings();
    assert_eq!(
        settings.transports,
        vec!["stdio".to_owned(), "http".to_owned()]
    );
    assert_eq!(settings.log_level, "debug");
    assert_eq!(settings.log_file, "/var/log/contextos.log");
    Ok(())
}

#[test]
fn set_server_settings_omits_keys_equal_to_the_schema_default()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    document.set_server_settings(&["http".to_owned()], "debug", "/var/log/contextos.log")?;

    document.set_server_settings(&["stdio".to_owned()], "info", "")?;

    let rendered = document.render();
    assert!(!rendered.contains("[server]"));
    Ok(())
}

#[test]
fn set_server_settings_rejects_an_invalid_log_level_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.set_server_settings(&["stdio".to_owned()], "verbose", "");

    assert!(matches!(result, Err(ConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn set_server_settings_rejects_an_empty_transports_list_and_leaves_the_document_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    // Regression: an operator clearing the transports prompt to blank must
    // not silently produce a `transports = []` server that starts neither
    // stdio nor HTTP and serves nothing (`ConfigError::NoTransports`,
    // `config.rs`'s `Config::validate`, is the schema-level guard this
    // relies on).
    let vault_dir = tempdir()?;
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;
    let before = document.render();

    let result = document.set_server_settings(&[], "info", "");

    assert!(matches!(result, Err(ConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn parse_rejects_malformed_toml() {
    let result = ConfigDocument::parse("not [ valid toml");

    assert!(matches!(result, Err(ConfigWriterError::Toml { .. })));
}

#[test]
fn parse_round_trips_an_existing_file_from_disk() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let config_dir = tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"mine\"\n",
        vault_dir.path()
    );
    let config_path = config_dir.path().join("config.toml");
    fs::write(&config_path, &source)?;

    let document = ConfigDocument::parse(&fs::read_to_string(&config_path)?)?;

    assert_eq!(document.render(), source);
    Ok(())
}
