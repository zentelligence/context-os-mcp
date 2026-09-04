//! Shared helpers for `contextos-web`'s integration tests, none of which is
//! itself an integration test target (this file lives under `tests/support/`
//! rather than directly under `tests/`, matching the standard Cargo
//! convention for test-only shared code).
//!
//! `contextos-web` never depends on `contextos-mcp` in production (FR-200),
//! so `CARGO_BIN_EXE_contextos` is not available here: Cargo only sets that
//! variable for a binary target owned by the package currently under test,
//! not a sibling workspace package's binary. [`contextos_mcp_binary`]
//! resolves the real `contextos` binary another way: it asks Cargo to build
//! it (a fast no-op if it is already up to date) and locates it in the
//! shared workspace `target/` directory, so these contract tests exercise a
//! real `contextos-mcp` stdio session (the delivery-plan Phase 14 gate's
//! own requirement) without a production dependency edge back onto it.

use std::path::PathBuf;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Builds (a fast no-op if already up to date) and returns the path to the
/// real `contextos` binary `contextos-mcp` produces.
pub fn contextos_mcp_binary() -> Result<PathBuf, BoxError> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = std::process::Command::new(cargo)
        .args(["build", "-p", "contextos-mcp", "--bin", "contextos"])
        .status()?;
    if !status.success() {
        return Err("building the contextos-mcp binary failed".into());
    }

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut path = target_dir();
    path.push(profile);
    path.push(format!("contextos{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        return Err(format!("expected the contextos binary at {}", path.display()).into());
    }
    Ok(path)
}

fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop(); // crates/contextos-web -> crates
    path.pop(); // crates -> workspace root
    path.push("target");
    path
}

/// Writes a minimal, valid `config.toml` naming one managed vault rooted at
/// `vault_dir`, and returns the config file's path.
pub fn write_vault_config(
    dir: &std::path::Path,
    vault_dir: &std::path::Path,
) -> std::io::Result<PathBuf> {
    let path = dir.join("config.toml");
    // `Debug`, not `Display`, is the correct formatting here: it quotes and
    // backslash-escapes the path the way a TOML string value requires,
    // which `.display()` (clippy's suggested alternative) would not do for
    // a path containing a backslash (Windows) or an embedded quote.
    #[allow(clippy::unnecessary_debug_formatting)]
    let vault_toml_value = format!("{vault_dir:?}");
    std::fs::write(
        &path,
        format!(
            "[[vault]]\npath = {vault_toml_value}\nname = \"contract-fixture\"\n\
             [vault.search]\ntext = false\ngraph = false\n"
        ),
    )?;
    Ok(path)
}

/// A `[[mcp_server]]` entry pointing at a real `contextos-mcp` instance
/// (stdio, the built binary) configured against `config_path`.
pub fn real_contextos_entry(
    name: &str,
    config_path: &std::path::Path,
) -> Result<contextos_web::McpServerConfig, BoxError> {
    Ok(contextos_web::McpServerConfig::Stdio {
        name: name.to_owned(),
        command: contextos_mcp_binary()?.to_string_lossy().into_owned(),
        args: vec![
            "--config".to_owned(),
            config_path.to_string_lossy().into_owned(),
        ],
    })
}
