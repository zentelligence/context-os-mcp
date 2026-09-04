//! MCP dispatch-and-error contract tests: unknown/missing/incompatible
//! tool arguments, structured tool-error shape and remediation, batch
//! per-item failure isolation, and the ordered whole-catalogue dispatch
//! matrices for every Phase 1 tool's representative input/output and
//! every documented Phase 1 error path. Shares its `call_tool`/
//! `assert_tool_success`/`assert_tool_error` helpers with every sibling
//! `*_contract.rs` file in this directory (each keeps its own copy,
//! matching this codebase's existing per-file pattern).

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};
use tempfile::tempdir;

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

async fn assert_tool_success(
    vault: &std::path::Path,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
    expected_fields: &[&str],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::try_from(vec![vault.to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let result = call_tool(server, name, arguments).await?;
    let structured = result.structured_content.ok_or_else(|| {
        std::io::Error::other(format!("{name} did not return structured content"))
    })?;

    assert_eq!(result.is_error, Some(false), "{name} returned a tool error");
    for field in expected_fields {
        assert!(
            structured.get(field).is_some(),
            "{name} omitted result field {field}"
        );
    }
    Ok(())
}

async fn assert_tool_error(
    vault: &std::path::Path,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
    expected_code: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::try_from(vec![vault.to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let result = call_tool(server, name, arguments).await?;
    let structured = result.structured_content.ok_or_else(|| {
        std::io::Error::other(format!("{name} did not return structured error content"))
    })?;
    let remediation = structured
        .get("remediation")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other(format!("{name} omitted error remediation")))?;

    assert_eq!(result.is_error, Some(true), "{name} did not mark its error");
    assert_eq!(structured.get("code"), Some(&json!(expected_code)));
    assert!(!remediation.is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_file_is_a_structured_tool_error_with_remediation()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let arguments = Map::from_iter([("path".to_owned(), json!("missing.md"))]);

    let result = call_tool(server, "fs_read_text_file", arguments).await?;
    let structured = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("tool error did not include structured content"))?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured.get("code"), Some(&json!("path/not-found")));
    assert_eq!(
        structured.get("remediation"),
        Some(&json!("Check the path and list its parent directory."))
    );
    // `missing.md` does not exist, so the reported path resolves the
    // vault (the existing ancestor) rather than the raw, possibly
    // unresolved tempdir path (e.g. under Windows 8.3 short names).
    assert_eq!(
        structured.get("path"),
        Some(&json!(
            dunce::canonicalize(vault.path())?.join("missing.md")
        ))
    );
    Ok(())
}

#[tokio::test]
async fn fr_06_fr_07_fs_list_directory_with_sizes_reports_entries_and_rejects_sort_by_without_it()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // FR-06/FR-07: `fs_list_directory_with_sizes` retired in favour of
    // `fs_list_directory`'s own `with_sizes`/`sort_by` parameters, covering
    // the three behaviours that split used to guarantee: the default call
    // stays lightweight (no size/modified fields), `with_sizes: true`
    // reports them and honours `sort_by`, and `sort_by` without
    // `with_sizes` is rejected rather than silently ignored.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("small.md"), "a")?;
    std::fs::write(vault.path().join("large.md"), "a much longer file body")?;

    let plain = call_tool(
        ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?,
        "fs_list_directory",
        Map::from_iter([("path".to_owned(), json!("."))]),
    )
    .await?;
    let plain_entries = plain
        .structured_content
        .as_ref()
        .and_then(|value| value.get("entries"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("fs_list_directory omitted entries"))?;
    assert!(
        !plain_entries.is_empty()
            && plain_entries
                .iter()
                .all(|entry| entry.get("size").is_none()),
        "default fs_list_directory must not report sizes: {plain_entries:?}"
    );

    let with_sizes_result = call_tool(
        ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?,
        "fs_list_directory",
        Map::from_iter([
            ("path".to_owned(), json!(".")),
            ("with_sizes".to_owned(), json!(true)),
            ("sort_by".to_owned(), json!("size")),
        ]),
    )
    .await?;
    let sized_entries = with_sizes_result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("entries"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("fs_list_directory omitted entries"))?;
    let sizes: Vec<u64> = sized_entries
        .iter()
        .filter_map(|entry| entry.get("size").and_then(serde_json::Value::as_u64))
        .collect();
    assert_eq!(
        sizes.len(),
        sized_entries.len(),
        "with_sizes: true must report a size for every entry: {sized_entries:?}"
    );
    assert!(
        sizes.windows(2).all(|pair| pair[0] <= pair[1]),
        "sort_by: size must order ascending by size: {sizes:?}"
    );

    assert_tool_error(
        vault.path(),
        "fs_list_directory",
        Map::from_iter([
            ("path".to_owned(), json!(".")),
            ("sort_by".to_owned(), json!("size")),
        ]),
        "io/invalid-argument",
    )
    .await
}

#[tokio::test]
async fn unknown_tool_arguments_are_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "content")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let arguments = Map::from_iter([
        ("path".to_owned(), json!("note.md")),
        ("unexpected".to_owned(), json!(true)),
    ]);

    let result = call_tool(server, "fs_read_text_file", arguments).await?;
    let message = result
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_text)
        .map(|content| content.text.as_str())
        .ok_or_else(|| std::io::Error::other("tool error did not include text content"))?;

    assert_eq!(result.is_error, Some(true));
    assert!(message.contains("unknown field `unexpected`"));
    Ok(())
}

#[tokio::test]
async fn missing_and_incompatible_tool_arguments_are_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "content")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let missing = call_tool(server, "fs_read_text_file", Map::new()).await?;
    let missing_message = missing
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_text)
        .map(|content| content.text.as_str())
        .ok_or_else(|| std::io::Error::other("missing-field error omitted text"))?;
    assert_eq!(missing.is_error, Some(true));
    assert!(missing_message.contains("missing field `path`"));

    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": "note.md", "head": 1, "tail": 1}))?,
        "io/invalid-argument",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn batch_path_validation_failures_are_isolated_per_item()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempdir()?;
    std::fs::write(vault.path().join("present.md"), "present")?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let arguments = serde_json::from_value(json!({
        "paths": ["present.md", outside.path().join("secret.md")]
    }))?;

    let result = call_tool(server, "fs_read_multiple_files", arguments).await?;
    let files = result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("files"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("batch result omitted files"))?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].get("content"), Some(&json!("present")));
    assert_eq!(
        files[1].get("error").and_then(|error| error.get("code")),
        Some(&json!("path/outside-root"))
    );
    Ok(())
}

// `fs_delete_file` contract tests (`FR-14` hard-delete gating, `D-30`'s
// managed-`index.md` emptiness relaxation, and `FR-115` bulk delete via
// `paths`/`pattern`) live in `tests/delete_contract.rs`, not here, to
// keep this already-large file from growing further.

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one ordered transport matrix makes coverage of all twelve Phase 1 tools auditable"
)]
async fn every_phase_1_tool_dispatches_with_representative_input_and_output()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir(vault.path().join("folder"))?;
    std::fs::write(vault.path().join("read.md"), "hello\n")?;
    std::fs::write(vault.path().join("edit.md"), "before\n")?;
    std::fs::write(vault.path().join("move-source.md"), "move me\n")?;
    std::fs::write(vault.path().join("folder/item.md"), "item\n")?;

    assert_tool_success(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": "read.md", "head": 1}))?,
        &["content", "line_count", "content_hash", "truncated"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_read_multiple_files",
        serde_json::from_value(json!({"paths": ["read.md", "missing.md"]}))?,
        &["files"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "written.md", "content": "written\n"}))?,
        &[
            "path",
            "bytes_written",
            "content_hash",
            "created",
            "warnings",
        ],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_edit_file",
        serde_json::from_value(json!({
            "path": "edit.md",
            "edits": [{"old_text": "before", "new_text": "after"}],
            "dry_run": true
        }))?,
        &["path", "diff", "applied", "content_hash"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_create_directory",
        serde_json::from_value(json!({"path": "created/nested"}))?,
        &["path", "created", "warnings"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_list_directory",
        serde_json::from_value(json!({"path": "."}))?,
        &["entries", "rendered"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_list_directory",
        serde_json::from_value(json!({"path": ".", "with_sizes": true, "sort_by": "size"}))?,
        &["entries", "rendered"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_directory_tree",
        serde_json::from_value(json!({"path": ".", "max_depth": 2}))?,
        &["name", "type", "children"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_move_file",
        serde_json::from_value(json!({"source": "move-source.md", "destination": "moved.md"}))?,
        &["source", "destination", "warnings"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_search_files",
        serde_json::from_value(json!({"path": ".", "pattern": "*.md"}))?,
        &["paths"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_get_file_info",
        serde_json::from_value(json!({"path": "read.md"}))?,
        &["path", "kind", "size", "readonly", "content_hash"],
    )
    .await?;
    assert_tool_success(
        vault.path(),
        "fs_list_allowed_directories",
        Map::new(),
        &["directories"],
    )
    .await?;
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one error matrix keeps stable MCP code coverage visibly complete"
)]
async fn documented_phase_1_errors_are_reachable_through_the_mcp_adapter()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempdir()?;
    std::fs::write(vault.path().join("binary.dat"), [0_u8, 1, 2])?;
    std::fs::write(vault.path().join("conflict.md"), "current")?;
    std::fs::write(vault.path().join("ambiguous.md"), "same same")?;
    std::fs::write(vault.path().join("edit.md"), "current")?;
    std::fs::write(vault.path().join("source.md"), "source")?;
    std::fs::write(vault.path().join("destination.md"), "destination")?;
    let oversized = std::fs::File::create(vault.path().join("oversized.md"))?;
    oversized.set_len(5 * 1024 * 1024 + 1)?;

    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": outside.path()}))?,
        "path/outside-root",
    )
    .await?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), vault.path().join("linked-outside"))?;
        assert_tool_error(
            vault.path(),
            "fs_read_text_file",
            serde_json::from_value(json!({"path": "linked-outside/secret.md"}))?,
            "path/symlink-escape",
        )
        .await?;
    }
    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": "missing.md"}))?,
        "path/not-found",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": "binary.dat"}))?,
        "io/binary",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({"path": "oversized.md"}))?,
        "io/too-large",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_read_text_file",
        serde_json::from_value(json!({
            "path": "edit.md",
            "range": {"from_line": 0, "to_line": 1}
        }))?,
        "io/invalid-range",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "conflict.md",
            "content": "replacement",
            "expected_hash": "0000000000000000000000000000000000000000000000000000000000000000"
        }))?,
        "io/conflict",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "conflict.md",
            "content": "replacement",
            "expected_hash": "not-a-hash"
        }))?,
        "io/invalid-hash",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_edit_file",
        serde_json::from_value(json!({
            "path": "edit.md",
            "edits": [{"old_text": "absent", "new_text": "replacement"}]
        }))?,
        "edit/not-found",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_edit_file",
        serde_json::from_value(json!({
            "path": "ambiguous.md",
            "edits": [{"old_text": "same", "new_text": "replacement"}]
        }))?,
        "edit/ambiguous",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_move_file",
        serde_json::from_value(json!({"source": "source.md", "destination": "destination.md"}))?,
        "io/destination-exists",
    )
    .await?;
    assert_tool_error(
        vault.path(),
        "fs_search_files",
        serde_json::from_value(json!({"path": ".", "pattern": "["}))?,
        "io/invalid-pattern",
    )
    .await?;
    let batch_paths = (0..51)
        .map(|index| json!(outside.path().join(format!("secret-{index}.md"))))
        .collect::<Vec<_>>();
    assert_tool_error(
        vault.path(),
        "fs_read_multiple_files",
        serde_json::from_value(json!({"paths": batch_paths}))?,
        "io/batch-too-large",
    )
    .await?;
    Ok(())
}
