//! Substrate contract tests for the cross-cutting services every mutating
//! tool routes through: Git staging/commit/restore and root-restore
//! exclusions (`FR-30` to `FR-34`), the operation log (`FR-23`,
//! `FR-24`), managed `index.md` reconciliation and rebuild (`FR-20` to
//! `FR-22`), and the Obsidian note/Base/Canvas format tools (`FR-40` to
//! `FR-46`). Split from `tool_contract.rs` to keep both files under the
//! project's file-size limit.

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

#[tokio::test]
async fn fr_30_to_fr_34_git_tools_stage_commit_and_restore_through_substrates()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let unavailable = call_tool(server.clone(), "git_status", Map::new()).await?;
    assert_eq!(
        unavailable
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("git/not-a-repo"))
    );
    let initialised = call_tool(server.clone(), "git_init", Map::new()).await?;
    assert_eq!(initialised.is_error, Some(false));

    let moved = call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "note.md", "content": "original\n"}))?,
    )
    .await?;
    assert_eq!(moved.is_error, Some(false));
    let status = call_tool(server.clone(), "git_status", Map::new()).await?;
    assert_eq!(status.is_error, Some(false));
    let diff = call_tool(
        server.clone(),
        "git_diff",
        serde_json::from_value(json!({"path": "note.md"}))?,
    )
    .await?;
    assert!(
        diff.structured_content
            .as_ref()
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|content| content.contains("+original"))
    );
    let first_commit = call_tool(server.clone(), "git_commit", Map::new()).await?;
    let first_id = first_commit
        .structured_content
        .as_ref()
        .and_then(|value| value.get("commit_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("git_commit omitted commit id"))?
        .to_owned();
    let log = call_tool(
        server.clone(),
        "git_log",
        serde_json::from_value(json!({"path": "note.md", "limit": 10}))?,
    )
    .await?;
    assert_eq!(
        log.structured_content
            .as_ref()
            .and_then(|value| value.get("entries"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "note.md", "content": "damaged\n", "force": true}))?,
    )
    .await?;

    let restored = call_tool(
        server,
        "git_restore",
        serde_json::from_value(json!({"path": "note.md", "ref": first_id}))?,
    )
    .await?;

    assert_eq!(restored.is_error, Some(false));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "original\n"
    );
    assert!(vault.path().join("index.md").exists());
    assert!(vault.path().join("memory/log").exists());
    Ok(())
}

#[tokio::test]
async fn fr_33_root_restore_preserves_the_default_active_exclusion_list()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for directory in ["memory/log", "memory/sessions", "memory/coding"] {
        std::fs::create_dir_all(vault.path().join(directory))?;
        std::fs::write(vault.path().join(directory).join("state.md"), "baseline\n")?;
    }
    std::fs::write(vault.path().join("note.md"), "baseline\n")?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let initialised = call_tool(server.clone(), "git_init", Map::new()).await?;
    let baseline = initialised
        .structured_content
        .as_ref()
        .and_then(|value| value.get("commit_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("git_init omitted commit id"))?;

    for directory in ["memory/log", "memory/sessions", "memory/coding"] {
        std::fs::write(vault.path().join(directory).join("state.md"), "active\n")?;
    }
    std::fs::write(vault.path().join("note.md"), "damaged\n")?;

    let restored = call_tool(
        server,
        "git_restore",
        serde_json::from_value(json!({"path": ".", "ref": baseline}))?,
    )
    .await?;

    assert_eq!(restored.is_error, Some(false));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "baseline\n"
    );
    for directory in ["memory/log", "memory/sessions", "memory/coding"] {
        assert_eq!(
            std::fs::read_to_string(vault.path().join(directory).join("state.md"))?,
            "active\n"
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn fr_30_zero_quiet_period_debounces_to_one_automatic_commit()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.commit_debounce_s = 0;
    let server = ContextOsServer::try_from(config)?;
    call_tool(server.clone(), "git_init", Map::new()).await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "debounced.md", "content": "one\n"}))?,
    )
    .await?;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;

    let log = call_tool(
        server,
        "git_log",
        serde_json::from_value(json!({"path": "debounced.md"}))?,
    )
    .await?;
    let entries = log
        .structured_content
        .as_ref()
        .and_then(|value| value.get("entries"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("git_log omitted entries"))?;
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]
            .get("message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.starts_with("mcp:"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_30_graceful_flush_commits_pending_paths_before_shutdown()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    call_tool(server.clone(), "git_init", Map::new()).await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "shutdown.md", "content": "safe\n"}))?,
    )
    .await?;

    let commits = server.flush_git()?;

    assert_eq!(commits.len(), 1);
    assert!(commits[0].commit_id.is_some());
    assert!(
        commits[0]
            .committed_paths
            .iter()
            .any(|path| path == std::path::Path::new("shutdown.md"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_30_graceful_flush_attempts_every_vault_before_reporting_a_failure()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let first = tempfile::Builder::new().prefix("first").tempdir()?;
    let second = tempfile::Builder::new().prefix("second").tempdir()?;
    let mut config = Config::try_from(vec![
        first.path().to_path_buf(),
        second.path().to_path_buf(),
    ])?;
    config.vaults[0].git.commit_debounce_s = 3600;
    config.vaults[1].git.commit_debounce_s = 3600;
    let server = ContextOsServer::try_from(config)?;
    for vault in [first.path(), second.path()] {
        call_tool(
            server.clone(),
            "git_init",
            serde_json::from_value(json!({"vault": vault}))?,
        )
        .await?;
    }
    for (vault, name) in [(first.path(), "first.md"), (second.path(), "second.md")] {
        call_tool(
            server.clone(),
            "fs_write_file",
            serde_json::from_value(json!({
                "path": vault.join(name),
                "content": "pending\n",
            }))?,
        )
        .await?;
    }
    std::fs::rename(first.path().join(".git"), first.path().join(".git-away"))?;

    assert!(server.flush_git().is_err());
    let repository = git2::Repository::open(second.path())?;
    let tree = repository.head()?.peel_to_commit()?.tree()?;
    assert!(tree.get_path(std::path::Path::new("second.md")).is_ok());
    Ok(())
}

#[tokio::test]
async fn fr_30_cross_vault_move_stages_source_and_destination_in_their_own_repositories()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let first = tempfile::Builder::new().prefix("first").tempdir()?;
    let second = tempfile::Builder::new().prefix("second").tempdir()?;
    let mut config = Config::try_from(vec![
        first.path().to_path_buf(),
        second.path().to_path_buf(),
    ])?;
    config.vaults[0].git.commit_debounce_s = 3600;
    config.vaults[1].git.commit_debounce_s = 3600;
    let server = ContextOsServer::try_from(config)?;
    for vault in [first.path(), second.path()] {
        call_tool(
            server.clone(),
            "git_init",
            serde_json::from_value(json!({"vault": vault}))?,
        )
        .await?;
    }
    let source = first.path().join("move.md");
    let destination = second.path().join("moved.md");
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": source, "content": "move\n"}))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "git_commit",
        serde_json::from_value(json!({"vault": first.path()}))?,
    )
    .await?;
    let moved = call_tool(
        server.clone(),
        "fs_move_file",
        serde_json::from_value(json!({"source": source, "destination": destination}))?,
    )
    .await?;
    assert_eq!(moved.is_error, Some(false));

    let first_status = call_tool(
        server.clone(),
        "git_status",
        serde_json::from_value(json!({"vault": first.path()}))?,
    )
    .await?;
    let second_status = call_tool(
        server.clone(),
        "git_status",
        serde_json::from_value(json!({"vault": second.path()}))?,
    )
    .await?;
    assert!(
        first_status
            .structured_content
            .as_ref()
            .and_then(|value| value.get("pending_paths"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|paths| paths.iter().any(|path| path == "move.md"))
    );
    assert!(
        second_status
            .structured_content
            .as_ref()
            .and_then(|value| value.get("pending_paths"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|paths| paths.iter().any(|path| path == "moved.md")),
        "second status: {:?}",
        second_status.structured_content
    );
    let mismatched_filter = call_tool(
        server,
        "git_log",
        serde_json::from_value(json!({
            "vault": first.path(),
            "path": destination,
        }))?,
    )
    .await?;
    assert_eq!(mismatched_filter.is_error, Some(true));
    assert_eq!(
        mismatched_filter
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("io/invalid-argument"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_40_to_fr_43_note_tools_preserve_defaults_body_order_and_links()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("Plan.md"), "# Plan\n")?;
    std::fs::write(vault.path().join("Source.md"), "# Source\n")?;
    std::fs::write(vault.path().join("image.png"), b"image")?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let created = call_tool(
        server.clone(),
        "note_create",
        serde_json::from_value(json!({
            "path": "daily-reflection.md",
            "frontmatter": {"priority": 2},
            "content": "# Daily Reflection\n\nSee [[Plan]], ![[image.png]], and [[Missing]].\n",
            "references": [{"target": "Source", "summary": "Background"}]
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false));

    let read = call_tool(
        server.clone(),
        "frontmatter_read",
        serde_json::from_value(json!({"path": "daily-reflection.md"}))?,
    )
    .await?;
    let frontmatter = read
        .structured_content
        .as_ref()
        .and_then(|value| value.get("frontmatter"))
        .ok_or_else(|| std::io::Error::other("frontmatter_read omitted frontmatter"))?;
    assert_eq!(frontmatter.get("type"), Some(&json!("note")));
    assert_eq!(frontmatter.get("entity"), Some(&json!("personal")));
    assert_eq!(frontmatter.get("status"), Some(&json!("new")));

    let updated = call_tool(
        server.clone(),
        "frontmatter_update",
        serde_json::from_value(json!({
            "path": "daily-reflection.md",
            "patch": {"priority": 3, "status": null}
        }))?,
    )
    .await?;
    assert_eq!(updated.is_error, Some(false));
    let persisted = std::fs::read_to_string(vault.path().join("daily-reflection.md"))?;
    assert!(persisted.contains("priority: 3"));
    assert!(!persisted.contains("status:"));
    assert!(persisted.contains("# References\n\n- [[Source]]: Background\n"));

    let links = call_tool(
        server,
        "links_read",
        serde_json::from_value(json!({"path": "daily-reflection.md"}))?,
    )
    .await?;
    let outgoing = links
        .structured_content
        .as_ref()
        .and_then(|value| value.get("outgoing"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("links_read omitted outgoing links"))?;
    assert_eq!(outgoing.len(), 4);
    assert_eq!(
        links
            .structured_content
            .as_ref()
            .and_then(|value| value.get("unresolved")),
        Some(&json!(["Missing"]))
    );
    // `[[Plan]]` is a plain wikilink with no custom display text, heading,
    // or block anchor: `display`/`heading`/`block` must be omitted from the
    // structured content entirely, never sent as `null` (the advertised
    // output schema declares each a plain, non-nullable `string`).
    let plan_link = outgoing
        .iter()
        .find(|link| link.get("target") == Some(&json!("Plan")))
        .ok_or_else(|| std::io::Error::other("links_read omitted the Plan link"))?;
    assert_eq!(plan_link.get("display"), None);
    assert_eq!(plan_link.get("heading"), None);
    assert_eq!(plan_link.get("block"), None);
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end Base scenario keeps quoting, conflict, and rollback evidence together"
)]
async fn fr_44_base_tools_round_trip_and_reject_a_transaction_that_dangles_a_formula()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let definition = json!({
        "formulas": {
            "quoted": "if(note.status == \"done\", 'it\\'s finished', \">= 10 & < 20\")"
        },
        "properties": {
            "formula.quoted": {"displayName": "Quoted result"}
        },
        "views": [{
            "type": "table",
            "name": "All notes",
            "order": ["file.name", "formula.quoted"]
        }]
    });
    let created = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({"path": "projects.base", "definition": definition}))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "created: {created:?}");
    let created_hash = created
        .structured_content
        .as_ref()
        .and_then(|value| value.get("content_hash"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_create omitted content_hash"))?;

    let updated = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "projects.base",
            "expected_hash": created_hash,
            "operations": [{
                "op": "set_property",
                "name": "status",
                "property": {"displayName": "Current status"}
            }]
        }))?,
    )
    .await?;
    assert_eq!(updated.is_error, Some(false), "updated: {updated:?}");
    let stale = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "projects.base",
            "expected_hash": created_hash,
            "operations": [{
                "op": "set_summary", "name": "counted", "expression": "values.length"
            }]
        }))?,
    )
    .await?;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(
        stale
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("io/conflict"))
    );

    let read = call_tool(
        server.clone(),
        "base_read",
        serde_json::from_value(json!({"path": "projects.base"}))?,
    )
    .await?;
    assert_eq!(read.is_error, Some(false));
    assert_eq!(
        read.structured_content
            .as_ref()
            .and_then(|value| value.pointer("/definition/formulas/quoted")),
        Some(&json!(
            "if(note.status == \"done\", 'it\\'s finished', \">= 10 & < 20\")"
        ))
    );
    let removed = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "projects.base",
            "operations": [
                {"op": "set_summary", "name": "counted", "expression": "values.length"},
                {"op": "remove_summary", "name": "counted"},
                {"op": "remove_property", "name": "status"}
            ]
        }))?,
    )
    .await?;
    assert_eq!(removed.is_error, Some(false), "removed: {removed:?}");
    let after_removal = call_tool(
        server.clone(),
        "base_read",
        serde_json::from_value(json!({"path": "projects.base"}))?,
    )
    .await?;
    assert_eq!(after_removal.is_error, Some(false));
    assert_eq!(
        after_removal
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/definition/properties/status")),
        None
    );
    assert_eq!(
        after_removal
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/definition/summaries")),
        Some(&json!({}))
    );

    let missing_removal = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "projects.base",
            "operations": [{"op": "remove_property", "name": "missing"}]
        }))?,
    )
    .await?;
    assert_eq!(missing_removal.is_error, Some(true));
    assert_eq!(
        missing_removal
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("format/base-schema"))
    );

    let before = std::fs::read(vault.path().join("projects.base"))?;
    let failed = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "projects.base",
            "operations": [{"op": "remove_formula", "name": "quoted"}]
        }))?,
    )
    .await?;
    assert_eq!(failed.is_error, Some(true));
    assert_eq!(
        failed
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("format/base-schema"))
    );
    assert_eq!(std::fs::read(vault.path().join("projects.base"))?, before);

    std::fs::write(
        vault.path().join("invalid.base"),
        "views:\n  - type: table\n    name: Broken\n    order: [formula.missing]\n",
    )?;
    let validation = call_tool(
        server,
        "base_read",
        serde_json::from_value(json!({"path": "invalid.base"}))?,
    )
    .await?;
    assert_eq!(validation.is_error, Some(false));
    assert_eq!(
        validation
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/diagnostics/0/path")),
        Some(&json!("$.views[0].order[0]"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_45_canvas_tools_group_nodes_cascade_edges_and_report_dangling_endpoints()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let created = call_tool(
        server.clone(),
        "canvas_create",
        serde_json::from_value(json!({
            "path": "map.canvas",
            "nodes": [
                {"id": "left", "type": "text", "text": "Left", "x": 0, "y": 0, "width": 200, "height": 100},
                {"id": "right", "type": "text", "text": "Right", "x": 400, "y": 100, "width": 200, "height": 100}
            ],
            "edges": [
                {"id": "connection", "fromNode": "left", "toNode": "right"}
            ]
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "created: {created:?}");
    let applied = call_tool(
        server.clone(),
        "canvas_apply",
        serde_json::from_value(json!({
            "path": "map.canvas",
            "operations": [
                {"op": "group", "group": {"id": "pair", "type": "group", "label": "Pair"}, "members": ["left", "right"]},
                {"op": "remove_node", "id": "right"}
            ]
        }))?,
    )
    .await?;
    assert_eq!(applied.is_error, Some(false), "applied: {applied:?}");
    let read = call_tool(
        server.clone(),
        "canvas_read",
        serde_json::from_value(json!({"path": "map.canvas"}))?,
    )
    .await?;
    assert_eq!(
        read.structured_content
            .as_ref()
            .and_then(|value| value.get("edges")),
        Some(&json!([]))
    );
    assert_eq!(
        read.structured_content
            .as_ref()
            .and_then(|value| value.pointer("/nodes/0/id")),
        Some(&json!("pair"))
    );

    std::fs::write(
        vault.path().join("dangling.canvas"),
        r#"{"nodes":[{"id":"only","type":"text","text":"Only","x":0,"y":0,"width":100,"height":100}],"edges":[{"id":"bad","fromNode":"only","toNode":"missing"}]}"#,
    )?;
    let validation = call_tool(
        server,
        "canvas_read",
        serde_json::from_value(json!({"path": "dangling.canvas"}))?,
    )
    .await?;
    assert_eq!(validation.is_error, Some(false));
    assert_eq!(
        validation
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/diagnostics/0/path")),
        Some(&json!("edges[0].toNode"))
    );
    Ok(())
}

#[tokio::test]
async fn d_31_base_and_canvas_read_report_parse_failures_as_diagnostics_and_enforce_extensions()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // D-31: a `.base`/`.canvas` file that fails to parse at all (not just soft
    // schema diagnostics) is reported by `base_read`/`canvas_read` as a
    // normal, non-error diagnostic result, not a tool error. This absorbed
    // `base_validate`/`canvas_validate`'s only capability `base_read`/
    // `canvas_read` didn't already have, letting those two tools retire.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("broken.base"), "views: [\n")?;
    std::fs::write(vault.path().join("broken.canvas"), "{\n")?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    for (tool, path, code) in [
        ("base_read", "broken.base", "format/base-schema"),
        ("canvas_read", "broken.canvas", "format/canvas-schema"),
    ] {
        let result = call_tool(
            server.clone(),
            tool,
            serde_json::from_value(json!({"path": path}))?,
        )
        .await?;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.pointer("/diagnostics/0/code")),
            Some(&json!(code))
        );
    }

    let wrong_extension = call_tool(
        server,
        "base_read",
        serde_json::from_value(json!({"path": "broken.canvas"}))?,
    )
    .await?;
    assert_eq!(wrong_extension.is_error, Some(true));
    assert_eq!(
        wrong_extension
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("io/invalid-argument"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_23_and_fr_24_mutations_and_manual_entries_share_the_same_daily_log()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let written = call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "note.md", "content": "# Note\n"}))?,
    )
    .await?;
    assert_eq!(written.is_error, Some(false));

    let manual = call_tool(
        server,
        "vault_log_append",
        serde_json::from_value(json!({
            "entry": "Reviewed the note",
            "files": ["note.md"]
        }))?,
    )
    .await?;

    assert_eq!(manual.is_error, Some(false));
    let log_root = vault.path().join("memory/log");
    let log_path = std::fs::read_dir(log_root)?
        .next()
        .ok_or_else(|| std::io::Error::other("log year was not created"))??
        .path();
    let log_path = std::fs::read_dir(log_path)?
        .next()
        .ok_or_else(|| std::io::Error::other("log month was not created"))??
        .path();
    let log_path = std::fs::read_dir(log_path)?
        .next()
        .ok_or_else(|| std::io::Error::other("daily log was not created"))??
        .path();
    let persisted = std::fs::read_to_string(log_path)?;
    assert!(persisted.contains("| fs_write_file | create | Created note.md"));
    assert!(persisted.contains("| manual | log | Reviewed the note | files: note.md"));
    Ok(())
}

#[tokio::test]
async fn fr_23_graceful_shutdown_flush_retries_buffered_operation_log_entries()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let now = time::OffsetDateTime::now_utc();
    let log_path = vault.path().join(format!(
        "memory/log/{:04}/{:02}/{:04}-{:02}-{:02}.md",
        now.year(),
        u8::from(now.month()),
        now.year(),
        u8::from(now.month()),
        now.day(),
    ));
    std::fs::create_dir_all(&log_path)?;
    let written = call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "buffered.md", "content": "# Buffered\n"}))?,
    )
    .await?;
    assert_eq!(written.is_error, Some(false));
    assert!(log_path.is_dir());
    std::fs::remove_dir(&log_path)?;

    let events = server.flush_operation_logs()?;

    assert_eq!(events.len(), 1);
    let persisted = std::fs::read_to_string(log_path)?;
    assert!(persisted.contains("| fs_write_file | create | Created buffered.md"));
    Ok(())
}

#[tokio::test]
async fn fr_20_to_fr_22_mcp_rebuild_renames_legacy_index_and_covers_every_folder()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("projects/nested"))?;
    std::fs::write(vault.path().join("_index.md"), "# Operator Root\n")?;
    std::fs::write(vault.path().join("projects/note.md"), "# Note\n")?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let preview = call_tool(
        server.clone(),
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": ".", "dry_run": true}))?,
    )
    .await?;
    assert_eq!(preview.is_error, Some(false));
    assert!(vault.path().join("_index.md").exists());
    assert!(!vault.path().join("index.md").exists());
    assert!(!vault.path().join("projects/index.md").exists());
    let preview = preview
        .structured_content
        .ok_or_else(|| std::io::Error::other("rebuild preview omitted structured result"))?;
    assert_eq!(preview.get("directories_scanned"), Some(&json!(3)));
    assert_eq!(preview.get("indexes_created"), Some(&json!(2)));
    assert_eq!(preview.get("indexes_updated"), Some(&json!(1)));

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "."}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(!vault.path().join("_index.md").exists());
    assert!(vault.path().join("index.md").exists());
    assert!(vault.path().join("projects/index.md").exists());
    assert!(vault.path().join("projects/nested/index.md").exists());
    let structured = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("rebuild omitted structured result"))?;
    assert_eq!(structured.get("directories_scanned"), Some(&json!(3)));
    assert_eq!(structured.get("indexes_created"), Some(&json!(2)));
    assert_eq!(structured.get("indexes_updated"), Some(&json!(1)));
    Ok(())
}

#[tokio::test]
async fn fr_20_directory_creation_automatically_indexes_root_ancestors_and_leaf()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "fs_create_directory",
        serde_json::from_value(json!({"path": "area/topic/leaf"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    for relative in [
        "index.md",
        "area/index.md",
        "area/topic/index.md",
        "area/topic/leaf/index.md",
    ] {
        assert!(
            vault.path().join(relative).exists(),
            "automatic reconciliation omitted {relative}"
        );
    }
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
