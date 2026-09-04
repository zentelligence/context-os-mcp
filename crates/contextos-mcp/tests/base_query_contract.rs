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

fn structured(
    result: &CallToolResult,
) -> Result<&serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn write_notes(vault: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(vault.join("notes"))?;
    std::fs::write(
        vault.join("notes/alpha.md"),
        "---\nstatus: active\ntags:\n  - project/alpha\nprice: 12\n---\nAlpha body.\n",
    )?;
    std::fs::write(
        vault.join("notes/beta.md"),
        "---\nstatus: archived\nprice: 5\n---\nBeta body.\n",
    )?;
    std::fs::write(
        vault.join("notes/gamma.md"),
        "---\nstatus: active\nprice: 20\n---\nGamma body.\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn base_query_resolves_a_named_view_sorts_limits_and_renders_every_format()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let definition = json!({
        "filters": "file.ext == \"md\"",
        "formulas": {
            "display_status": "if(status == \"active\", \"yes\", \"no\")"
        },
        "properties": {
            "status": {"displayName": "Status"},
            "formula.display_status": {"displayName": "Display"}
        },
        "views": [
            {
                "type": "table",
                "name": "Active",
                "filters": "status == \"active\"",
                "order": ["file.name", "status", "formula.display_status"],
                "sort": [{"property": "file.name", "direction": "DESC"}],
                "limit": 1
            },
            {
                "type": "table",
                "name": "Tagged",
                "filters": "file.hasTag(\"project/alpha\")",
                "order": ["file.name"]
            }
        ]
    });
    let created = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({"path": "catalogue.base", "definition": definition}))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "created: {created:?}");

    let active = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({"path": "catalogue.base", "view": "Active"}))?,
    )
    .await?;
    assert_eq!(active.is_error, Some(false), "active: {active:?}");
    let active = structured(&active)?;
    assert_eq!(active.get("matched"), Some(&json!(2)));
    assert_eq!(active.get("truncated"), Some(&json!(false)));
    assert_eq!(
        active.get("columns"),
        Some(&json!(["file.name", "status", "formula.display_status"]))
    );
    assert_eq!(active.get("diagnostics"), Some(&json!([])));
    assert_eq!(
        active.get("content"),
        Some(&json!(
            "| file.name | status | formula.display_status |\n| --- | --- | --- |\n| gamma.md | active | formula.display_status (not evaluated) |\n"
        ))
    );

    let tagged_json = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({"path": "catalogue.base", "view": "Tagged", "format": "json"}))?,
    )
    .await?;
    assert_eq!(tagged_json.is_error, Some(false), "tagged_json: {tagged_json:?}");
    let tagged_json = structured(&tagged_json)?;
    assert_eq!(tagged_json.get("matched"), Some(&json!(1)));
    let content = tagged_json
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_query omitted content"))?;
    let parsed: serde_json::Value = serde_json::from_str(content)?;
    assert_eq!(parsed, json!([{"file.name": "alpha.md"}]));

    let tagged_csv = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({"path": "catalogue.base", "view": "Tagged", "format": "csv"}))?,
    )
    .await?;
    assert_eq!(tagged_csv.is_error, Some(false), "tagged_csv: {tagged_csv:?}");
    assert_eq!(
        structured(&tagged_csv)?.get("content"),
        Some(&json!("file.name\nalpha.md\n"))
    );
    Ok(())
}

fn write_folder_notes(vault: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(vault.join("memory/tasks/deep"))?;
    std::fs::write(vault.join("root.md"), "---\nstatus: active\n---\nRoot body.\n")?;
    std::fs::write(
        vault.join("memory/tasks/foo.md"),
        "---\nstatus: active\n---\nFoo body.\n",
    )?;
    std::fs::write(
        vault.join("memory/tasks/deep/bar.md"),
        "---\nstatus: active\n---\nBar body.\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn base_query_filters_on_a_note_dot_prefixed_property_the_same_as_the_bare_name()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    // The real-world shape memory/tasks.base's views use for every
    // state-filtered view (Backlog, Next, Done, ...): note.<key> naming
    // the same frontmatter key as the bare <key>, per Obsidian's own
    // documented property-namespace equivalence.
    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "note.status == \"active\"",
                "order": ["file.name"]
            }
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("matched"), Some(&json!(2)));
    assert_eq!(
        body.get("content"),
        Some(&json!("| file.name |\n| --- |\n| alpha.md |\n| gamma.md |\n"))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_evaluates_a_string_level_and_combinator() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "decision.md",
            "content": "---\nregister: decision\ncompleted:\n---\nBody.\n"
        }))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "closed.md",
            "content": "---\nregister: decision\ncompleted: 2024-01-01\n---\nBody.\n"
        }))?,
    )
    .await?;

    // The real-world shape memory/tasks.base's "Decision pending review"
    // view uses: `&&` combining two comparisons in a single string leaf,
    // previously rejected outright as query/base-query-unsupported-filter.
    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "register == \"decision\" && completed == null",
                "order": ["file.name"]
            }
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("matched"), Some(&json!(1)));
    assert_eq!(
        body.get("content"),
        Some(&json!("| file.name |\n| --- |\n| decision.md |\n"))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_filters_and_narrows_scan_by_file_folder_without_matching_subfolders()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_folder_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    // "memory/tasks" matches only the direct child (foo.md), not the note
    // one level deeper in "memory/tasks/deep" (bar.md): the real-world
    // shape memory/tasks.base uses (`file.folder == "memory/tasks"`),
    // which must not be confused with a recursive `file.inFolder()` match.
    let nested = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.folder == \"memory/tasks\"",
                "order": ["file.name"]
            }
        }))?,
    )
    .await?;
    assert_eq!(nested.is_error, Some(false), "nested: {nested:?}");
    let nested = structured(&nested)?;
    assert_eq!(nested.get("matched"), Some(&json!(1)));
    assert_eq!(
        nested.get("content"),
        Some(&json!("| file.name |\n| --- |\n| foo.md |\n"))
    );

    // A vault-root note's file.folder is the empty string.
    let root = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.folder == \"\"",
                "order": ["file.name"]
            }
        }))?,
    )
    .await?;
    assert_eq!(root.is_error, Some(false), "root: {root:?}");
    let root = structured(&root)?;
    assert_eq!(root.get("matched"), Some(&json!(1)));
    assert_eq!(
        root.get("content"),
        Some(&json!("| file.name |\n| --- |\n| root.md |\n"))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_exposes_basename_size_timestamps_tags_links_and_embeds()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let content = "---\ntags:\n  - project/alpha\n---\n# A\n\n[[b]]\n![[diagram.png]]\n";
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "a.md", "content": content}))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.path == \"a.md\"",
                "order": [
                    "file.name", "file.basename", "file.size", "file.ctime", "file.mtime",
                    "file.tags", "file.links", "file.embeds", "file.properties"
                ]
            },
            "format": "json"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = structured(&result)?;
    let text = structured
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_query omitted content"))?;
    let rows: serde_json::Value = serde_json::from_str(text)?;
    let row = &rows[0];
    assert_eq!(row.get("file.name"), Some(&json!("a.md")));
    assert_eq!(row.get("file.basename"), Some(&json!("a")));
    assert_eq!(row.get("file.size"), Some(&json!(content.len())));
    assert!(row.get("file.ctime").is_some_and(serde_json::Value::is_string));
    assert!(row.get("file.mtime").is_some_and(serde_json::Value::is_string));
    assert_eq!(row.get("file.tags"), Some(&json!(["project/alpha"])));
    assert_eq!(row.get("file.links"), Some(&json!(["b"])));
    assert_eq!(row.get("file.embeds"), Some(&json!(["diagram.png"])));
    assert_eq!(row.get("file.properties"), Some(&json!({"tags": ["project/alpha"]})));
    Ok(())
}

#[tokio::test]
async fn base_query_reports_a_frontmatter_parse_failure_as_a_diagnostic_not_a_silent_null()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    // An unquoted title containing its own ": " is ambiguous, genuinely
    // invalid YAML (the exact real-world shape found in the vault this
    // addendum was written against): "status: active" is a real,
    // well-formed frontmatter value the parser never reaches, because the
    // malformed title line fails the whole document first.
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "bad.md",
            "content": "---\ntitle: Project: a note about things\nstatus: active\n---\nBody.\n"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.path == \"bad.md\"",
                "order": ["file.name", "status"]
            },
            "format": "json"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("matched"), Some(&json!(1)));
    let text = body
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_query omitted content"))?;
    let rows: serde_json::Value = serde_json::from_str(text)?;
    // The row is still returned (graceful degradation), but its
    // frontmatter-backed column is null, not the real "active" value.
    assert_eq!(rows, json!([{"file.name": "bad.md", "status": null}]));
    let diagnostics = body
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("base_query omitted diagnostics"))?;
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].get("code"), Some(&json!("format/frontmatter")));
    assert_eq!(diagnostics[0].get("path"), Some(&json!("bad.md")));

    // The same degraded-to-null frontmatter means a filter on that
    // property silently excludes the row too, not just its display
    // column: this is the real-world confusion the diagnostic exists to
    // explain.
    let filtered = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "status == \"active\"",
                "order": ["file.name"]
            }
        }))?,
    )
    .await?;
    assert_eq!(filtered.is_error, Some(false), "{filtered:?}");
    let filtered = structured(&filtered)?;
    assert_eq!(filtered.get("matched"), Some(&json!(0)));
    let filtered_diagnostics = filtered
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("base_query omitted diagnostics"))?;
    assert!(
        filtered_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.get("path") == Some(&json!("bad.md")))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_fails_closed_on_file_backlinks_when_search_is_disabled()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].search.text = false;
    config.vaults[0].search.graph = false;
    let server = ContextOsServer::try_from(config)?;

    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "a.md", "content": "# A\n"}))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.path == \"a.md\"",
                "order": ["file.backlinks"]
            }
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(&result)?.get("code"), Some(&json!("index/disabled")));
    Ok(())
}

#[tokio::test]
async fn base_query_resolves_file_backlinks_through_the_link_graph()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    // b.md is written before a.md so a.md's wikilink resolves to the real
    // note rather than creating a phantom node (same ordering the
    // query_graph contract test uses).
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "b.md", "content": "# B\n\nno links\n"}))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "a.md", "content": "# A\n\n[[b]]\n"}))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {
                "filters": "file.path == \"b.md\"",
                "order": ["file.name", "file.backlinks"]
            },
            "format": "json"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let text = structured(&result)?
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_query omitted content"))?;
    let rows: serde_json::Value = serde_json::from_str(text)?;
    // "index.md" is also a genuine backlink: fs_write_file's managed
    // directory-listing block links every file in a directory from that
    // directory's index.md, so the vault root's index.md really does link
    // to b.md, sorted alongside the deliberate a.md -> b.md wikilink.
    assert_eq!(
        rows,
        json!([{"file.name": "b.md", "file.backlinks": ["a.md", "index.md"]}])
    );
    Ok(())
}

#[tokio::test]
async fn base_query_fails_closed_on_formula_filters_and_unsupported_expressions()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let formula_filter = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {"filters": "formula.display_status != \"\"", "order": ["file.name"]}
        }))?,
    )
    .await?;
    assert_eq!(formula_filter.is_error, Some(true), "{formula_filter:?}");
    assert_eq!(
        structured(&formula_filter)?.get("code"),
        Some(&json!("query/base-query-formula-reference"))
    );

    let unsupported = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "definition": {"filters": "price > 10", "order": ["file.name"]}
        }))?,
    )
    .await?;
    assert_eq!(unsupported.is_error, Some(true), "{unsupported:?}");
    assert_eq!(
        structured(&unsupported)?.get("code"),
        Some(&json!("query/base-query-unsupported-filter"))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_reports_an_unknown_view_and_a_columnless_view_by_distinct_codes()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let bare_definition = json!({
        "views": [{"type": "table", "name": "Bare"}]
    });
    let created = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({"path": "bare.base", "definition": bare_definition}))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "created: {created:?}");

    let no_columns = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({"path": "bare.base"}))?,
    )
    .await?;
    assert_eq!(no_columns.is_error, Some(true), "{no_columns:?}");
    assert_eq!(
        structured(&no_columns)?.get("code"),
        Some(&json!("query/base-query-no-columns"))
    );

    let unknown_view = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({"path": "bare.base", "view": "Missing"}))?,
    )
    .await?;
    assert_eq!(unknown_view.is_error, Some(true), "{unknown_view:?}");
    assert_eq!(
        structured(&unknown_view)?.get("code"),
        Some(&json!("query/base-view-not-found"))
    );
    Ok(())
}

#[tokio::test]
async fn base_query_requires_exactly_one_of_path_or_definition_and_confines_paths_to_the_vault()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let both = call_tool(
        server.clone(),
        "base_query",
        serde_json::from_value(json!({
            "path": "catalogue.base",
            "definition": {"order": ["file.name"]}
        }))?,
    )
    .await?;
    assert_eq!(both.is_error, Some(true), "{both:?}");
    assert_eq!(structured(&both)?.get("code"), Some(&json!("io/invalid-argument")));

    let neither = call_tool(server.clone(), "base_query", Map::new()).await?;
    assert_eq!(neither.is_error, Some(true), "{neither:?}");
    assert_eq!(structured(&neither)?.get("code"), Some(&json!("io/invalid-argument")));

    let outside = tempdir()?;
    std::fs::write(
        outside.path().join("escape.base"),
        "views: [{type: table, name: All}]\n",
    )?;
    let escaped = call_tool(
        server,
        "base_query",
        serde_json::from_value(json!({"path": outside.path().join("escape.base")}))?,
    )
    .await?;
    assert_eq!(escaped.is_error, Some(true), "{escaped:?}");
    assert_eq!(structured(&escaped)?.get("code"), Some(&json!("path/outside-root")));
    Ok(())
}
