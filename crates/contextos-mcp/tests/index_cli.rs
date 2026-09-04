use std::process::Command;

use contextos_search::{IndexesText, TantivyIndex, TextIndexConfig};
use tempfile::tempdir;

#[test]
fn contextos_index_rebuilds_the_text_index_and_link_graph_for_every_managed_vault()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    std::fs::write(vault.join("b.md"), "# B\n\nno links\n")?;
    std::fs::write(vault.join("a.md"), "# A\n\n[[b]]\n")?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
            [[vault]]
            path = {vault:?}
            state_directory = ".contextos"
            [vault.git]
            enabled = false
            "#,
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "index"])
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("text: 2 scanned, 2 reindexed, 0 removed"));
    assert!(stdout.contains("graph: 2 notes scanned, 2 nodes, 1 edges"));

    // The built text index is directly readable without going through the
    // server, proving the CLI run persisted real derived state rather than
    // an in-memory report.
    let index = TantivyIndex::try_from(TextIndexConfig {
        directory: vault.join(".contextos").join("index"),
    })?;
    let entries = index.entries()?;
    let mut paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["a.md", "b.md"]);
    Ok(())
}

#[test]
fn contextos_index_reports_disabled_search_without_failing() -> Result<(), Box<dyn std::error::Error>> {
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
            [vault.search]
            text = false
            graph = false
            [vault.git]
            enabled = false
            ",
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "index"])
        .env("XDG_DATA_HOME", data_home.path())
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("search indexing disabled"));
    Ok(())
}

#[test]
fn contextos_index_exits_non_zero_for_an_invalid_configuration() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let config = fixture.path().join("config.toml");
    std::fs::write(&config, "not valid toml === [[[\n")?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", config.to_str().ok_or("non-UTF-8 config")?, "index"])
        .output()?;

    assert!(!output.status.success());
    Ok(())
}
