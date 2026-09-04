use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
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

const VALID_FLOWCHART: &str = include_str!("../../../fixtures/obsidian-formats/mermaid/valid-flowchart.mmd");
const INVALID_FLOWCHART: &str = include_str!("../../../fixtures/obsidian-formats/mermaid/invalid-flowchart.mmd");

#[tokio::test]
async fn catalogue_advertises_mermaid_tools_with_object_schemas() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let catalogue = ContextOsServer::catalogue();

    let validate = catalogue
        .get("mermaid_validate")
        .ok_or_else(|| std::io::Error::other("missing tool mermaid_validate"))?;
    assert_eq!(
        validate.input_schema.get("type").and_then(serde_json::Value::as_str),
        Some("object")
    );
    assert!(validate.description.is_some());
    let schema = validate
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other("mermaid_validate omitted output schema"))?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("mermaid_validate omitted output properties"))?;
    let mut fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
    fields.sort_unstable();
    // `mermaid_validate`'s output schema is the flat, merged success-and-
    // `ToolFailure` shape `fallible_output_schema_for` builds
    // (`tool_contract.rs`'s
    // `every_remaining_tool_advertises_a_flat_fallible_output_schema`
    // covers this generally): `code`/`message`/`path`/`remediation` are
    // `ToolFailure`'s fields, present because `mermaid_validate`'s error
    // path populates `structured_content` with that shape too.
    assert_eq!(
        fields,
        vec!["code", "diagnostics", "message", "path", "remediation", "valid"]
    );

    let render = catalogue
        .get("mermaid_render")
        .ok_or_else(|| std::io::Error::other("missing tool mermaid_render"))?;
    assert_eq!(
        render.input_schema.get("type").and_then(serde_json::Value::as_str),
        Some("object")
    );
    assert!(render.description.is_some());

    Ok(())
}

#[tokio::test]
async fn validate_accepts_inline_source_and_reports_a_well_formed_diagram()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"source": VALID_FLOWCHART}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(true)));
    assert_eq!(structured(&result)?.get("diagnostics"), Some(&json!([])));
    Ok(())
}

#[tokio::test]
async fn validate_reads_the_fenced_mermaid_block_from_a_vault_note()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("diagram.md"),
        format!("# Title\n\nSome text.\n\n```mermaid\n{VALID_FLOWCHART}```\n\nMore text.\n"),
    )?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"path": "diagram.md"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn a_decoy_mermaid_fence_nested_inside_an_unrelated_fenced_block_is_skipped()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("diagram.md"),
        format!(
            "# Title\n\n\
             ```text\n\
             Example:\n\
             ```mermaid\n\
             flowchart TD\n  A[Decoy] -->\n\
             ```\n\
             This text is still part of the outer text block.\n\
             ```\n\n\
             Real diagram below.\n\n\
             ```mermaid\n{VALID_FLOWCHART}```\n"
        ),
    )?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"path": "diagram.md"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn validate_reports_a_stable_code_for_a_malformed_diagram() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"source": INVALID_FLOWCHART}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(false)));
    assert_eq!(
        structured(&result)?.get("diagnostics").and_then(|value| value
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("code"))),
        Some(&json!("mermaid/diagram-parse"))
    );
    Ok(())
}

#[tokio::test]
async fn validate_rejects_oversized_inline_source_with_the_resource_limit_code()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let oversized = "x".repeat(2 * 1024 * 1024 + 1);

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"source": oversized}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(false)));
    assert_eq!(
        structured(&result)?.get("diagnostics").and_then(|value| value
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("code"))),
        Some(&json!("mermaid/resource-limit"))
    );
    Ok(())
}

#[tokio::test]
async fn render_returns_an_embedded_svg_resource_free_of_foreign_object_and_script()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_render",
        serde_json::from_value(json!({"source": VALID_FLOWCHART}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let resource = result
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("render did not return an embedded resource"))?;
    let (mime_type, svg) = match &resource.resource {
        rmcp::model::ResourceContents::TextResourceContents { mime_type, text, .. } => {
            (mime_type.clone(), text.clone())
        }
        _ => {
            return Err(std::io::Error::other("render returned a blob, not text SVG").into());
        }
    };
    assert_eq!(mime_type.as_deref(), Some("image/svg+xml"));
    assert!(svg.trim_start().starts_with("<svg"), "{svg}");
    assert!(!svg.contains("<foreignObject"), "{svg}");
    assert!(!svg.contains("<script"), "{svg}");
    Ok(())
}

#[tokio::test]
async fn render_returns_the_same_diagnostics_as_validate_on_failure()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_render",
        serde_json::from_value(json!({"source": INVALID_FLOWCHART}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("valid"), Some(&json!(false)));
    assert_eq!(
        structured(&result)?.get("diagnostics").and_then(|value| value
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("code"))),
        Some(&json!("mermaid/diagram-parse"))
    );
    Ok(())
}

#[tokio::test]
async fn a_note_without_a_fenced_mermaid_block_reports_the_stable_schema_code()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("plain.md"), "# Title\n\nNo diagram here.\n")?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"path": "plain.md"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)?.get("code"), Some(&json!("format/mermaid-schema")));
    Ok(())
}

#[tokio::test]
async fn supplying_both_path_and_source_is_a_stable_invalid_argument_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("diagram.md"),
        format!("```mermaid\n{VALID_FLOWCHART}```\n"),
    )?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"path": "diagram.md", "source": VALID_FLOWCHART}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)?.get("code"), Some(&json!("io/invalid-argument")));
    Ok(())
}

#[tokio::test]
async fn supplying_neither_path_nor_source_is_a_stable_invalid_argument_error()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(server, "mermaid_validate", serde_json::from_value(json!({}))?).await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)?.get("code"), Some(&json!("io/invalid-argument")));
    Ok(())
}

#[tokio::test]
async fn a_path_outside_every_configured_root_is_rejected() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"path": "../outside.md"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)?.get("code"), Some(&json!("path/outside-root")));
    Ok(())
}

#[tokio::test]
async fn unknown_fields_are_rejected() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "mermaid_validate",
        serde_json::from_value(json!({"source": VALID_FLOWCHART, "unexpected": true}))?,
    )
    .await?;

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
