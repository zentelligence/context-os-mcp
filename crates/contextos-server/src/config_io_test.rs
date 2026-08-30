use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn load_config_document_starts_fresh_when_the_file_does_not_exist()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");

    let document = load_config_document(&path)?;

    assert_eq!(document.render(), String::new());
    Ok(())
}

#[test]
fn load_config_document_parses_existing_content() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");
    fs::write(&path, "# a comment\n[server]\nlog_level = \"debug\"\n")?;

    let document = load_config_document(&path)?;

    assert!(document.render().contains("# a comment"));
    Ok(())
}

#[test]
fn load_config_document_rejects_malformed_toml() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("config.toml");
    fs::write(&path, "not [ valid toml")?;

    let result = load_config_document(&path);

    assert!(matches!(result, Err(ConfigIoError::InvalidToml { .. })));
    Ok(())
}

#[test]
fn write_config_document_creates_missing_parent_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("nested").join("config.toml");
    let document = ConfigDocument::new();

    write_config_document(&path, &document)?;

    assert!(path.is_file());
    Ok(())
}

#[test]
fn write_config_document_writes_the_rendered_content() -> Result<(), Box<dyn std::error::Error>> {
    let vault_dir = tempdir()?;
    let config_dir = tempdir()?;
    let path = config_dir.path().join("config.toml");
    let mut document = ConfigDocument::new();
    document.add_vault("mine", vault_dir.path(), true)?;

    write_config_document(&path, &document)?;

    assert_eq!(fs::read_to_string(&path)?, document.render());
    Ok(())
}

#[test]
fn write_config_document_overwrites_existing_content() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = tempdir()?;
    let path = config_dir.path().join("config.toml");
    fs::write(&path, "stale content")?;
    let document = ConfigDocument::new();

    write_config_document(&path, &document)?;

    assert_eq!(fs::read_to_string(&path)?, String::new());
    Ok(())
}

#[test]
fn write_config_document_leaves_no_temporary_file_behind() -> Result<(), Box<dyn std::error::Error>>
{
    let config_dir = tempdir()?;
    let path = config_dir.path().join("config.toml");
    let document = ConfigDocument::new();

    write_config_document(&path, &document)?;

    let entries: Vec<_> = fs::read_dir(config_dir.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("config.toml")]);
    Ok(())
}
