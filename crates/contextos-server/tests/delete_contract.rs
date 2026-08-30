//! Contract tests for `fs_delete_file` (`FR-14`): destructive-delete
//! gating, the `D-30` managed-`index.md` emptiness relaxation, and
//! `FR-115` bulk deletion via `paths`/`pattern`. Split out from
//! `tool_contract.rs` to keep both files under the project's file-size
//! limit rather than growing that already-oversized file further.

use contextos_server::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};
use tempfile::tempdir;

#[tokio::test]
async fn fr_14_hard_delete_requires_configuration_and_preserves_the_file_when_denied()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("protected.md"), "protected")?;
    let denied_server =
        ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let denied = call_tool(
        denied_server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "protected.md", "hard": true}))?,
    )
    .await?;

    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        Some(&json!("io/destructive-delete-disabled"))
    );
    assert!(vault.path().join("protected.md").exists());

    let mut enabled_config = Config::try_from(vec![vault.path().to_path_buf()])?;
    enabled_config.vaults[0].git.destructive_delete = true;
    let enabled = call_tool(
        ContextOsServer::try_from(enabled_config)?,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "protected.md", "hard": true}))?,
    )
    .await?;

    assert_eq!(enabled.is_error, Some(false));
    assert!(!vault.path().join("protected.md").exists());
    Ok(())
}

/// `D-30`: `contextos-index`'s own reconciliation (`FR-20`) recreates
/// `index.md` after every mutation, so once the sole real file in a
/// directory is deleted, that directory is never literally empty again.
/// `fs_delete_file` must still be able to remove such a directory: its
/// only remaining content is a managed artefact the index service itself
/// owns, not operator content.
#[tokio::test]
async fn d_30_directory_delete_succeeds_when_only_content_is_managed_index_md()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    call_tool(
        server,
        "fs_write_file",
        serde_json::from_value(json!({"path": "notes/only.md", "content": "content\n"}))?,
    )
    .await?;
    assert!(vault.path().join("notes/index.md").exists());

    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let deleted_file = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "notes/only.md", "hard": false}))?,
    )
    .await?;
    assert_eq!(deleted_file.is_error, Some(false));
    // `FR-20` reconciliation recreated `notes/index.md` as the directory's
    // sole remaining entry.
    assert!(vault.path().join("notes/index.md").exists());
    assert_eq!(std::fs::read_dir(vault.path().join("notes"))?.count(), 1);

    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let deleted_directory = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "notes", "hard": false}))?,
    )
    .await?;

    assert_eq!(
        deleted_directory
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        None,
        "unexpected error: {:?}",
        deleted_directory.structured_content
    );
    assert_eq!(deleted_directory.is_error, Some(false));
    assert!(!vault.path().join("notes").exists());
    Ok(())
}

/// The `D-30` relaxation must not apply when this directory's `index.md`
/// is not actually managed by the index service: an unmanaged root or an
/// `index_md.exclude`d subtree can hold a real, operator-authored file
/// literally named `index.md`, and deleting the directory must still
/// require it to be genuinely empty.
#[tokio::test]
async fn d_30_directory_delete_still_requires_real_emptiness_outside_index_management()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir(vault.path().join("archive"))?;
    std::fs::write(vault.path().join("archive/index.md"), "operator content\n")?;

    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.exclude = vec!["archive".to_owned()];
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "archive", "hard": false}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        Some(&json!("io/directory-not-empty"))
    );
    assert!(vault.path().join("archive/index.md").exists());
    Ok(())
}

/// `FR-115`: `paths` deletes several explicit targets in one call, and
/// isolates each target's failure (`FR-02`'s partial-success pattern) so
/// one missing or invalid path never fails the whole batch.
#[tokio::test]
async fn fr_115_paths_deletes_multiple_targets_and_isolates_per_item_failures()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempdir()?;
    std::fs::write(vault.path().join("first.md"), "first")?;
    std::fs::write(vault.path().join("second.md"), "second")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({
            "paths": [
                "first.md",
                "second.md",
                "missing.md",
                outside.path().join("secret.md"),
            ]
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let results = result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("results"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("delete batch omitted results"))?;
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].get("deleted"), Some(&json!(true)));
    assert_eq!(results[1].get("deleted"), Some(&json!(true)));
    assert_eq!(
        results[2].get("error").and_then(|error| error.get("code")),
        Some(&json!("path/not-found"))
    );
    assert_eq!(
        results[3].get("error").and_then(|error| error.get("code")),
        Some(&json!("path/outside-root"))
    );
    assert!(!vault.path().join("first.md").exists());
    assert!(!vault.path().join("second.md").exists());
    Ok(())
}

/// `FR-115`: `pattern` deletes every glob match under `path` in one call.
/// A populated directory among the matches keeps the `D-30`-aware
/// non-recursive guard: it reports `io/directory-not-empty` as that one
/// item's error rather than deleting its contents or failing the batch.
#[tokio::test]
async fn fr_115_pattern_deletes_every_match_and_keeps_the_non_recursive_guard()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("draft-one.tmp"), "one")?;
    std::fs::write(vault.path().join("draft-two.tmp"), "two")?;
    std::fs::create_dir(vault.path().join("draft-dir.tmp"))?;
    std::fs::write(vault.path().join("draft-dir.tmp/keep.md"), "keep")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": ".", "pattern": "*.tmp"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let results = result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("results"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("delete batch omitted results"))?;
    assert_eq!(results.len(), 3);
    let failed = results
        .iter()
        .filter(|item| item.get("error").is_some())
        .count();
    assert_eq!(failed, 1);
    assert!(
        results
            .iter()
            .any(|item| item.get("error").and_then(|error| error.get("code"))
                == Some(&json!("io/directory-not-empty")))
    );
    assert!(!vault.path().join("draft-one.tmp").exists());
    assert!(!vault.path().join("draft-two.tmp").exists());
    assert!(vault.path().join("draft-dir.tmp").exists());
    Ok(())
}

/// `FR-115`: giving more than one selector style (here `path` and `paths`
/// together) is a caller mistake, not a per-item failure, so it rejects
/// the whole call up front rather than guessing which selector wins.
#[tokio::test]
async fn fr_115_ambiguous_target_selection_is_a_whole_call_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("first.md"), "first")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"path": "first.md", "paths": ["first.md"]}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        Some(&json!("io/invalid-argument"))
    );
    assert!(vault.path().join("first.md").exists());
    Ok(())
}

/// `FR-115`: bulk delete shares `fs_read_multiple_files`'s batch cap
/// (`max_batch_files`), so an oversized `paths` list is rejected up front
/// rather than partially processed.
#[tokio::test]
async fn fr_115_paths_over_the_batch_cap_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].limits.max_batch_files = 2;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "fs_delete_file",
        serde_json::from_value(json!({"paths": ["a.md", "b.md", "c.md"]}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|content| content.get("code")),
        Some(&json!("io/batch-too-large"))
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
