//! Contract tests for `vault_info`, `fs_list_allowed_directories`, and
//! `{name}://` vault-selector resolution: effective health reporting
//! without secret exposure,
//! resource-link threshold reporting, configured vault naming, and a
//! bare vault name correctly routing every real tool handler. Split from
//! `tool_contract.rs` to keep both files under the project's file-size
//! limit.

use contextos_mcp::{Config, ContextOsServer, Transport};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

#[tokio::test]
async fn vault_info_reports_effective_health_without_exposing_secrets()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.server.transports = vec![Transport::Stdio, Transport::Http];
    config.server.http.token = "must-not-appear".to_owned();
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "vault_info", Map::new()).await?;
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("vault_info omitted structured content"))?;
    let reported_vault = content
        .get("vaults")
        .and_then(serde_json::Value::as_array)
        .and_then(|vaults| vaults.first())
        .ok_or_else(|| std::io::Error::other("vault_info omitted configured vault"))?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("version"), Some(&json!(env!("CARGO_PKG_VERSION"))));
    assert_eq!(
        content.get("protocol_version"),
        Some(&json!(rmcp::model::ProtocolVersion::LATEST.as_str()))
    );
    assert_eq!(content.get("transports"), Some(&json!(["stdio", "http"])));
    assert_eq!(reported_vault.get("path"), Some(&json!(vault.path())));
    assert_eq!(reported_vault.get("managed"), Some(&json!(true)));
    assert_eq!(
        reported_vault.get("git"),
        Some(&json!({"enabled": true, "repo": false, "pending_commits": 0}))
    );
    let indexes = reported_vault
        .get("indexes")
        .ok_or_else(|| std::io::Error::other("vault_info omitted indexes"))?;
    let state_directory = indexes
        .get("state_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("vault_info omitted state_directory"))?;
    assert!(
        std::path::Path::new(state_directory).is_absolute(),
        "state_directory should be an absolute path, got {state_directory}"
    );
    assert_eq!(
        indexes.get("text").and_then(|text| text.get("enabled")),
        Some(&json!(true))
    );
    assert_eq!(
        indexes.get("text").and_then(|text| text.get("documents")),
        Some(&json!(0))
    );
    assert_eq!(
        indexes.get("graph").and_then(|graph| graph.get("enabled")),
        Some(&json!(true))
    );
    assert_eq!(
        indexes.get("graph").and_then(|graph| graph.get("needs_rebuild")),
        Some(&json!(true))
    );
    // `search.semantic` defaults to `false` for this vault, so the
    // semantic index reports disabled with empty counts and, since it has
    // never built, omits `last_build` entirely rather than sending `null`
    // (the advertised output schema declares it a plain, non-nullable
    // `string`, so `null` would be a genuine contract violation).
    assert_eq!(
        indexes.get("semantic"),
        Some(&json!({
            "enabled": false,
            "documents": 0,
            "chunks": 0,
            "stale_estimate": 0,
        }))
    );
    assert_config_summary_fields(reported_vault);
    assert!(!serde_json::to_string(content)?.contains("must-not-appear"));
    Ok(())
}

/// Asserts `vault_info`'s `config_summary` fields exercised by
/// `vault_info_reports_effective_health_without_exposing_secrets`,
/// extracted so that test stays under clippy's line-count lint as new
/// fields (most recently `search.graph_backend`) are added.
fn assert_config_summary_fields(reported_vault: &serde_json::Value) {
    assert_eq!(
        reported_vault
            .get("config_summary")
            .and_then(|summary| summary.get("git"))
            .and_then(|git| git.get("restore_exclude")),
        Some(&json!(["memory/log", "memory/sessions", "memory/coding"]))
    );
    assert_eq!(
        reported_vault
            .get("config_summary")
            .and_then(|summary| summary.get("search"))
            .and_then(|search| search.get("exclude")),
        Some(&json!([
            ".contextos",
            ".git",
            ".obsidian",
            "memory/log",
            "memory/sessions"
        ]))
    );
    assert_eq!(
        reported_vault
            .get("config_summary")
            .and_then(|summary| summary.get("search"))
            .and_then(|search| search.get("rebuild_budget_seconds")),
        Some(&json!(25))
    );
    // The effective (here, defaulted) graph_backend is reported
    // alongside the existing graph boolean.
    assert_eq!(
        reported_vault
            .get("config_summary")
            .and_then(|summary| summary.get("search"))
            .and_then(|search| search.get("graph_backend")),
        Some(&json!("fjall"))
    );
}

#[tokio::test]
async fn vault_info_reports_resource_link_threshold_and_eligible_file_count()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.txt"), "a")?;
    std::fs::write(vault.path().join("b.md"), "b")?;
    std::fs::create_dir(vault.path().join(".git"))?;
    std::fs::write(vault.path().join(".git/config"), "ignored, matches default hidden")?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.server.resource_link_threshold_kb = 7;
    // `resource_eligible_files` now counts what
    // `resources_list_include` enumerates, not every hidden-excluded file;
    // this test's own concern is the threshold/count reporting mechanism,
    // not the allowlist's scoping, so opt in broadly.
    config.vaults[0].resources_list_include = vec!["**/*".to_owned()];
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "vault_info", Map::new()).await?;
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("vault_info omitted structured content"))?;
    let reported_vault = content
        .get("vaults")
        .and_then(serde_json::Value::as_array)
        .and_then(|vaults| vaults.first())
        .ok_or_else(|| std::io::Error::other("vault_info omitted configured vault"))?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("resource_link_threshold_kb"), Some(&json!(7)));
    assert_eq!(reported_vault.get("resource_eligible_files"), Some(&json!(2)));
    Ok(())
}

#[tokio::test]
async fn vault_info_and_allowed_directories_report_the_configured_vault_name()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!("[[vault]]\npath = {:?}\nname = \"mine\"\n", vault.path());
    let server = ContextOsServer::try_from(Config::try_from(source.as_str())?)?;

    let info = call_tool(server.clone(), "vault_info", Map::new()).await?;
    let info_content = info
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("vault_info omitted structured content"))?;
    let reported_vault = info_content
        .get("vaults")
        .and_then(serde_json::Value::as_array)
        .and_then(|vaults| vaults.first())
        .ok_or_else(|| std::io::Error::other("vault_info omitted configured vault"))?;
    assert_eq!(reported_vault.get("name"), Some(&json!("mine")));

    let allowed = call_tool(server, "fs_list_allowed_directories", Map::new()).await?;
    let allowed_content = allowed
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("fs_list_allowed_directories omitted structured content"))?;
    let reported_directory = allowed_content
        .get("directories")
        .and_then(serde_json::Value::as_array)
        .and_then(|directories| directories.first())
        .ok_or_else(|| std::io::Error::other("fs_list_allowed_directories omitted a directory"))?;
    assert_eq!(reported_directory.get("name"), Some(&json!("mine")));

    Ok(())
}

/// The Git tools' `vault` selector, `vault_index_rebuild`'s
/// `path`, and `doctor_resolve`'s `path` all accept a bare configured
/// vault name, not just an absolute path or the `{name}://` prefixed
/// form. Proven end to end through the real MCP handlers, not
/// just `VaultPath::try_from_vault_selector`'s own `contextos-core` unit
/// tests: each check confirms the *correct* vault was selected, not only
/// that the call succeeded, so a bug that silently picked the wrong root
/// would still fail this test.
#[tokio::test]
async fn a_bare_vault_name_selects_the_right_vault_through_the_real_tool_handlers()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mine = tempfile::Builder::new().prefix("mine").tempdir()?;
    let family = tempfile::Builder::new().prefix("family").tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"mine\"\n[[vault]]\npath = {:?}\nname = \"family\"\n",
        mine.path(),
        family.path()
    );
    let server = ContextOsServer::try_from(Config::try_from(source.as_str())?)?;

    for vault in ["mine", "family"] {
        let initialised = call_tool(
            server.clone(),
            "git_init",
            serde_json::from_value(json!({"vault": vault}))?,
        )
        .await?;
        assert_eq!(initialised.is_error, Some(false));
    }

    // vault_index_rebuild via the bare name "mine" scopes to that vault
    // only, before either vault has any other file written to it: mine's
    // freshly created index.md exists, family's does not. (Writing a file
    // elsewhere first would auto-create index.md via the write pipeline's
    // own indexing, independent of this call; order matters here
    // so the assertion is not accidentally satisfied for the wrong reason.)
    let rebuild = call_tool(
        server.clone(),
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "mine"}))?,
    )
    .await?;
    assert_eq!(rebuild.is_error, Some(false));
    assert!(mine.path().join("index.md").is_file());
    assert!(!family.path().join("index.md").is_file());

    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": family.path().join("note.md"), "content": "family only\n"}))?,
    )
    .await?;

    // git_status via the bare name "family" must see family's own pending
    // path, not mine's (empty) one: proves the bare name selected the
    // correct root, not merely that the call did not error.
    let family_status = call_tool(
        server.clone(),
        "git_status",
        serde_json::from_value(json!({"vault": "family"}))?,
    )
    .await?;
    assert!(
        family_status
            .structured_content
            .as_ref()
            .and_then(|value| value.get("pending_paths"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|paths| paths.iter().any(|path| path == "note.md"))
    );
    let mine_status = call_tool(
        server.clone(),
        "git_status",
        serde_json::from_value(json!({"vault": "mine"}))?,
    )
    .await?;
    assert!(
        mine_status
            .structured_content
            .as_ref()
            .and_then(|value| value.get("pending_paths"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|paths| !paths.iter().any(|path| path == "note.md"))
    );

    // doctor_resolve via the bare name "family" scopes remediation to
    // that vault only: a dry run must not report touching "mine".
    let resolve = call_tool(
        server,
        "doctor_resolve",
        serde_json::from_value(json!({"path": "family", "dry_run": true}))?,
    )
    .await?;
    assert_eq!(resolve.is_error, Some(false));
    let outcomes = resolve
        .structured_content
        .as_ref()
        .and_then(|value| value.get("outcomes"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted outcomes"))?;
    assert!(!outcomes.is_empty());
    let mine_resolved = dunce::canonicalize(mine.path())?.display().to_string();
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.get("vault") != Some(&json!(mine_resolved))),
        "doctor_resolve scoped to \"family\" must not report acting on \"mine\": {outcomes:?}"
    );

    Ok(())
}

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}
