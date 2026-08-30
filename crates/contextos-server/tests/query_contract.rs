use std::fs::File;
use std::time::{Duration, SystemTime};

use contextos_server::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ProgressNotificationParam};
use rmcp::service::NotificationContext;
use rmcp::{ClientHandler, RoleClient};
use serde_json::{Map, json};

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

#[tokio::test]
async fn catalogue_advertises_the_five_query_tools_with_object_schemas_and_output_fields()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let catalogue = ContextOsServer::catalogue();
    // Each tool's output schema is the flat, merged success-and-`ToolFailure`
    // shape `fallible_output_schema_for` builds (`tool_contract.rs`'s
    // `every_remaining_tool_advertises_a_flat_fallible_output_schema`
    // covers this generally): `code`/`message`/`path`/`remediation` are
    // `ToolFailure`'s fields, present because every one of these tools'
    // error path populates `structured_content` with that shape too.
    let expected = [
        (
            "query_text",
            vec![
                "code",
                "hits",
                "index_freshness",
                "message",
                "path",
                "remediation",
            ],
        ),
        (
            "query_semantic",
            vec!["code", "hits", "message", "path", "remediation"],
        ),
        (
            "query_graph",
            vec!["code", "edges", "message", "nodes", "path", "remediation"],
        ),
        (
            "query_index_status",
            vec![
                "code",
                "graph",
                "message",
                "path",
                "remediation",
                "semantic",
                "state_directory",
                "text",
            ],
        ),
        (
            "query_index_rebuild",
            vec![
                "code",
                "graph",
                "message",
                "path",
                "remediation",
                "semantic",
                "text",
            ],
        ),
    ];

    for (name, expected_fields) in expected {
        let tool = catalogue
            .get(name)
            .ok_or_else(|| std::io::Error::other(format!("missing tool {name}")))?;
        assert_eq!(
            tool.input_schema
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("object")
        );
        assert!(tool.description.is_some());
        let schema = tool
            .output_schema
            .as_ref()
            .ok_or_else(|| std::io::Error::other(format!("{name} omitted output schema")))?;
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| std::io::Error::other(format!("{name} omitted output properties")))?;
        let mut actual_fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
        actual_fields.sort_unstable();
        assert_eq!(actual_fields, expected_fields, "{name} schema drifted");
    }
    Ok(())
}

#[test]
fn query_index_status_advertises_a_plain_string_state_directory_type()
-> Result<(), Box<dyn std::error::Error>> {
    // Same array-form-nullable failure class documented on
    // `optional_u64_schema` and `optional_path_schema`: schemars' default
    // schema for an `Option<String>` field is `"type": ["string", "null"]`,
    // which broke Cowork's per-task tool registry for every tool sharing a
    // field shaped that way (found live on `ToolFailure.path`). This field
    // must advertise a plain `"type": "string"` instead, absent from
    // `required` since the vault may have search disabled.
    let catalogue = ContextOsServer::catalogue();
    let tool = catalogue
        .get("query_index_status")
        .ok_or("missing tool query_index_status")?;
    let schema = tool
        .output_schema
        .as_ref()
        .ok_or("query_index_status omitted output schema")?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or("query_index_status omitted output properties")?;
    assert_eq!(
        properties
            .get("state_directory")
            .and_then(|schema| schema.get("type")),
        Some(&json!("string")),
        "state_directory schema should be a plain string type, not array-form nullable"
    );
    // Regression: `schema_with` overriding a field's generated subschema
    // also defeats schemars' usual `Option<T>` -> "absent from required"
    // inference (it derives that from the same overridden subschema,
    // which a bare function's return type does not itself mark
    // optional); `#[serde(default)]` restores it. Without that,
    // `state_directory` was advertised as required despite genuinely
    // being omitted whenever a vault has search disabled, a schema/data
    // mismatch a strict validator could reject outright.
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !required.contains(&json!("state_directory")),
        "state_directory must stay optional in required, got {required:?}"
    );
    Ok(())
}

#[test]
fn optional_numeric_input_fields_advertise_a_plain_integer_type()
-> Result<(), Box<dyn std::error::Error>> {
    // schemars' default schema for an `Option<u64>` field is `"type":
    // ["integer", "null"]` (JSON Schema 2020-12's array-form type, used to
    // express nullability). Several real-world MCP clients only recognise
    // a single-string `type` and, finding none, fall back to serialising
    // the argument as a string, which the server then rejects. These
    // fields must advertise a plain `"type": "integer"` instead; the field
    // stays optional by simply being absent from `required`.
    let catalogue = ContextOsServer::catalogue();

    let rebuild = catalogue
        .get("query_index_rebuild")
        .ok_or("missing tool query_index_rebuild")?;
    let rebuild_props = rebuild
        .input_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or("query_index_rebuild omitted input properties")?;
    assert_eq!(
        rebuild_props
            .get("budget_seconds")
            .and_then(|schema| schema.get("type")),
        Some(&json!("integer"))
    );
    assert!(
        !rebuild
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|required| required.contains(&json!("budget_seconds"))),
        "budget_seconds must remain optional"
    );

    let read = catalogue
        .get("fs_read_text_file")
        .ok_or("missing tool fs_read_text_file")?;
    let read_props = read
        .input_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or("fs_read_text_file omitted input properties")?;
    let read_required = read
        .input_schema
        .get("required")
        .and_then(serde_json::Value::as_array);
    for field in ["head", "tail"] {
        assert_eq!(
            read_props.get(field).and_then(|schema| schema.get("type")),
            Some(&json!("integer")),
            "{field} schema drifted"
        );
        assert!(
            !read_required.is_some_and(|required| required.contains(&json!(field))),
            "{field} must remain optional"
        );
    }
    Ok(())
}

#[tokio::test]
async fn fr_50_query_text_finds_a_note_written_through_the_server_with_no_manual_indexing()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // Managed index.md generation is disabled so this test observes only
    // the notes it wrote, not the derived index.md files FR-22 also
    // produces (which are themselves legitimately searchable content,
    // covered separately).
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "notes/gadget.md",
            "content": "# Gadget\n\nA searchable gadget note.\n",
        }))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "notes/other.md",
            "content": "# Other\n\nUnrelated prose.\n",
        }))?,
    )
    .await?;

    let result = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "gadget"}))?,
    )
    .await?;
    let content = structured(&result)?;
    assert_eq!(result.is_error, Some(false));
    let hits = content
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get("path"), Some(&json!("notes/gadget.md")));
    assert!(
        hits[0]
            .get("snippet")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|snippet| !snippet.is_empty())
    );
    assert!(content.get("index_freshness").is_some());
    Ok(())
}

#[tokio::test]
async fn query_text_scoping_is_governed_by_search_exclude_not_index_md_exclude()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    // Managed index.md generation is disabled so this test observes only
    // the notes it wrote, not the derived index.md files FR-22 also
    // produces (which are themselves legitimately searchable content and
    // would otherwise add an extra "gadget" hit via the root index.md's
    // listing of private-notes/gadget.md, confounding the exact hit count
    // this test asserts).
    config.vaults[0].index_md.enabled = false;
    // `index_md.exclude` governs which directories receive a managed
    // `index.md`; it must not also narrow what `query_text` can find.
    config.vaults[0].index_md.exclude = vec!["private-notes".to_owned()];
    // `search.exclude` is the independent knob for search scoping. Extend
    // rather than replace the default list: the default includes
    // `memory/log`, and replacing it would make the operation log
    // searchable, which would add its own "gadget" hit (it records
    // "Created private-notes/gadget.md") and confound this test's exact
    // hit count.
    config.vaults[0]
        .search
        .exclude
        .push("not-searchable".to_owned());
    let server = ContextOsServer::try_from(config)?;

    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "private-notes/gadget.md",
            "content": "# Gadget\n\nA searchable gadget note.\n",
        }))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "not-searchable/widget.md",
            "content": "# Widget\n\nAn unsearchable widget note.\n",
        }))?,
    )
    .await?;

    let gadget_result = call_tool(
        server.clone(),
        "query_text",
        serde_json::from_value(json!({"query": "gadget"}))?,
    )
    .await?;
    let gadget_content = structured(&gadget_result)?;
    let gadget_hits = gadget_content
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(
        gadget_hits.len(),
        1,
        "a directory excluded only from index_md must still be searchable"
    );
    assert_eq!(
        gadget_hits[0].get("path"),
        Some(&json!("private-notes/gadget.md"))
    );

    let widget_result = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "widget"}))?,
    )
    .await?;
    let widget_content = structured(&widget_result)?;
    let widget_hits = widget_content
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(
        widget_hits.len(),
        0,
        "a directory excluded from search.exclude must not be searchable"
    );
    Ok(())
}

#[tokio::test]
async fn fr_51_query_text_reindexes_a_direct_external_edit()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "note.md",
            "content": "# Note\n\nOriginal gadget prose.\n",
        }))?,
    )
    .await?;

    // Bypass the server entirely: edit the file directly on disk and force
    // a distinct modification time so the freshness scan observes staleness
    // even on filesystems with coarse mtime resolution.
    let absolute = vault.path().join("note.md");
    std::fs::write(&absolute, "# Note\n\nReplacement widget prose.\n")?;
    let file = File::options().write(true).open(&absolute)?;
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_770_100_000))?;
    drop(file);

    let result = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "widget"}))?,
    )
    .await?;
    let content = structured(&result)?;
    assert_eq!(result.is_error, Some(false));
    let hits = content
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(hits.len(), 1);
    assert_eq!(
        content
            .get("index_freshness")
            .and_then(|freshness| freshness.get("reindexed")),
        Some(&json!(1))
    );
    Ok(())
}

#[tokio::test]
async fn fr_50_path_prefix_and_tag_filters_scope_results()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "projects/alpha.md",
            "content": "---\ntags: [\"widget\"]\n---\n# Alpha\n\nShared prose.\n",
        }))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "journal/beta.md",
            "content": "---\ntags: [\"gadget\"]\n---\n# Beta\n\nShared prose.\n",
        }))?,
    )
    .await?;

    let by_prefix = call_tool(
        server.clone(),
        "query_text",
        serde_json::from_value(json!({"query": "prose", "path_prefix": "projects"}))?,
    )
    .await?;
    let prefix_hits = structured(&by_prefix)?
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(prefix_hits.len(), 1);
    assert_eq!(
        prefix_hits[0].get("path"),
        Some(&json!("projects/alpha.md"))
    );

    let by_tag = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "prose", "tags": ["gadget"]}))?,
    )
    .await?;
    let tag_hits = structured(&by_tag)?
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(tag_hits.len(), 1);
    assert_eq!(tag_hits[0].get("path"), Some(&json!("journal/beta.md")));
    Ok(())
}

#[tokio::test]
async fn fr_116_exclude_paths_omits_a_superseded_prefix_from_query_text_results()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "notes/current.md",
            "content": "# Current\n\nShared prose.\n",
        }))?,
    )
    .await?;
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "notes/superseded/draft.md",
            "content": "# Superseded Draft\n\nShared prose.\n",
        }))?,
    )
    .await?;

    let unfiltered = call_tool(
        server.clone(),
        "query_text",
        serde_json::from_value(json!({"query": "prose"}))?,
    )
    .await?;
    let unfiltered_hits = structured(&unfiltered)?
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(unfiltered_hits.len(), 2);

    let excluded = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "prose", "exclude_paths": ["notes/superseded"]}))?,
    )
    .await?;
    let excluded_hits = structured(&excluded)?
        .get("hits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_text omitted hits"))?;
    assert_eq!(excluded_hits.len(), 1);
    assert_eq!(
        excluded_hits[0].get("path"),
        Some(&json!("notes/current.md"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_50_invalid_query_syntax_is_a_stable_tool_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "title:("}))?,
    )
    .await?;
    let content = structured(&result)?;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(content.get("code"), Some(&json!("index/invalid-query")));
    assert!(
        content
            .get("remediation")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.is_empty())
    );
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end walk through every query_graph operation keeps the FR-52 contract auditable in one place"
)]
async fn fr_52_query_graph_covers_neighbours_backlinks_path_and_orphans()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    // b.md is written before a.md so a.md's wikilink resolves to the real
    // note rather than creating a phantom node.
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
    call_tool(
        server.clone(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "c.md", "content": "# C\n\nan island\n"}))?,
    )
    .await?;

    let neighbours = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "a.md"}))?,
    )
    .await?;
    let neighbour_content = structured(&neighbours)?;
    assert_eq!(neighbours.is_error, Some(false));
    let mut neighbour_paths: Vec<String> = neighbour_content
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_graph omitted nodes"))?
        .iter()
        .filter_map(|node| node.get("path").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect();
    neighbour_paths.sort();
    assert_eq!(neighbour_paths, vec!["a.md".to_owned(), "b.md".to_owned()]);

    let backlinks = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "backlinks", "from": "b.md"}))?,
    )
    .await?;
    let backlink_nodes = structured(&backlinks)?
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_graph omitted nodes"))?;
    assert!(
        backlink_nodes
            .iter()
            .any(|node| node.get("path") == Some(&json!("a.md")))
    );

    let path_view = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "path", "from": "a.md", "to": "b.md"}))?,
    )
    .await?;
    let path_edges = structured(&path_view)?
        .get("edges")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_graph omitted edges"))?;
    assert_eq!(path_edges.len(), 1);

    let orphans = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "orphans"}))?,
    )
    .await?;
    let orphan_nodes = structured(&orphans)?
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("query_graph omitted nodes"))?;
    assert!(
        orphan_nodes
            .iter()
            .any(|node| node.get("path") == Some(&json!("c.md")))
    );
    assert!(
        !orphan_nodes
            .iter()
            .any(|node| node.get("path") == Some(&json!("a.md")))
    );

    let too_deep = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "a.md", "depth": 5}))?,
    )
    .await?;
    assert_eq!(too_deep.is_error, Some(true));
    assert_eq!(
        structured(&too_deep)?.get("code"),
        Some(&json!("index/invalid-query"))
    );

    let unknown_from = call_tool(
        server.clone(),
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "missing.md"}))?,
    )
    .await?;
    assert_eq!(unknown_from.is_error, Some(true));
    assert_eq!(
        structured(&unknown_from)?.get("code"),
        Some(&json!("path/not-found"))
    );

    let missing_from = call_tool(
        server,
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours"}))?,
    )
    .await?;
    assert_eq!(missing_from.is_error, Some(true));
    assert!(
        structured(&missing_from)?
            .get("remediation")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.is_empty())
    );
    Ok(())
}

/// Captures every `notifications/progress` message it receives, so a test
/// can assert on live activity streamed during a long-running tool call
/// rather than only on the final response.
#[derive(Clone, Default)]
struct ProgressCapturingClient {
    updates: std::sync::Arc<tokio::sync::Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for ProgressCapturingClient {
    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        let updates = self.updates.clone();
        async move {
            updates.lock().await.push(params);
        }
    }
}

#[tokio::test]
async fn fr_55_query_index_rebuild_streams_progress_notifications_when_requested()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n\nFirst note.\n")?;
    std::fs::write(vault.path().join("b.md"), "# B\n\n[[a]]\n")?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    let client_handler = ProgressCapturingClient::default();
    let updates = client_handler.updates.clone();
    let mut client = client_handler.serve(client_transport).await?;

    // rmcp's client transport assigns a progress token to every outgoing
    // request automatically (`send_request_with_option`, unconditionally),
    // so no manual `_meta.progressToken` is needed here to have the server
    // treat this call as progress-tracked.
    let params = CallToolRequestParams::new("query_index_rebuild")
        .with_arguments(serde_json::from_value(json!({"index": "all"}))?);

    let result = client.call_tool(params).await?;
    assert_eq!(result.is_error, Some(false));

    client.close().await?;
    server_handle.await??;

    let updates = updates.lock().await;
    assert!(
        !updates.is_empty(),
        "expected at least one progress notification for a progress-tracked call"
    );
    let token = &updates[0].progress_token;
    assert!(
        updates.iter().all(|update| &update.progress_token == token),
        "every update in one rebuild call should share the same progress token"
    );
    assert!(
        updates
            .iter()
            .any(|update| update.message.as_deref() == Some("rebuilding text index"))
    );
    assert!(
        updates
            .iter()
            .any(|update| update.message.as_deref() == Some("link graph rebuilt"))
    );
    // The MCP spec requires progress to increase on every update.
    let sequence: Vec<f64> = updates.iter().map(|update| update.progress).collect();
    let mut sorted = sequence.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    assert_eq!(sequence, sorted);
    Ok(())
}

#[tokio::test]
async fn fr_55_status_and_rebuild_reflect_writes_and_report_zero_staleness_afterwards()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
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

    let status = call_tool(server.clone(), "query_index_status", Map::new()).await?;
    let status_content = structured(&status)?;
    assert_eq!(
        status_content
            .get("text")
            .and_then(|text| text.get("documents")),
        Some(&json!(2))
    );
    assert_eq!(
        status_content
            .get("graph")
            .and_then(|graph| graph.get("nodes")),
        Some(&json!(2))
    );
    assert_eq!(
        status_content
            .get("graph")
            .and_then(|graph| graph.get("edges")),
        Some(&json!(1))
    );

    let rebuild = call_tool(
        server.clone(),
        "query_index_rebuild",
        serde_json::from_value(json!({"index": "all"}))?,
    )
    .await?;
    assert_eq!(rebuild.is_error, Some(false));

    let status_after = call_tool(server.clone(), "query_index_status", Map::new()).await?;
    let status_after_content = structured(&status_after)?;
    assert_eq!(
        status_after_content
            .get("text")
            .and_then(|text| text.get("stale_estimate")),
        Some(&json!(0))
    );
    assert_eq!(
        status_after_content
            .get("graph")
            .and_then(|graph| graph.get("needs_rebuild")),
        Some(&json!(false))
    );

    let semantic = call_tool(
        server,
        "query_index_rebuild",
        serde_json::from_value(json!({"index": "semantic"}))?,
    )
    .await?;
    assert_eq!(semantic.is_error, Some(true));
    assert_eq!(
        structured(&semantic)?.get("code"),
        Some(&json!("index/disabled"))
    );
    Ok(())
}

#[tokio::test]
async fn disabled_text_search_reports_the_stable_disabled_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].search.text = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "anything"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        structured(&result)?.get("code"),
        Some(&json!("index/disabled"))
    );
    Ok(())
}

#[tokio::test]
async fn disabled_semantic_search_reports_the_stable_disabled_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // `search.semantic` defaults to `false`, so no embedding provider
    // configuration is needed for this vault.
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "query_semantic",
        serde_json::from_value(json!({"query": "anything"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        structured(&result)?.get("code"),
        Some(&json!("index/disabled"))
    );
    Ok(())
}

#[test]
fn semantic_enabled_with_local_provider_and_no_model_directory_fails_construction_clearly()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].search.semantic = true;
    // `provider` defaults to `local`; no `model_directory` is configured,
    // which must be an explicit, actionable startup failure rather than a
    // silently disabled semantic index or a fetch attempt.
    let Err(error) = ContextOsServer::try_from(config) else {
        return Err("expected server construction to fail without a model_directory".into());
    };
    assert!(error.to_string().contains("model_directory"));
    Ok(())
}

#[tokio::test]
async fn unmanaged_vault_reports_search_disabled_rather_than_erroring_on_status()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].managed = false;
    let server = ContextOsServer::try_from(config)?;

    let status = call_tool(server.clone(), "query_index_status", Map::new()).await?;
    assert_eq!(status.is_error, Some(false));
    assert_eq!(
        structured(&status)?
            .get("text")
            .and_then(|text| text.get("enabled")),
        Some(&json!(false))
    );

    let query = call_tool(
        server,
        "query_text",
        serde_json::from_value(json!({"query": "anything"}))?,
    )
    .await?;
    assert_eq!(query.is_error, Some(true));
    assert_eq!(
        structured(&query)?.get("code"),
        Some(&json!("index/disabled"))
    );
    Ok(())
}
