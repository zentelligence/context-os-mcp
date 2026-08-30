use contextos_server::{Config, ContextOsServer};
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

fn checks(
    result: &CallToolResult,
) -> Result<
    Vec<&serde_json::Map<String, serde_json::Value>>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    structured(result)?
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("doctor result omitted checks").into())
        .map(|checks| {
            checks
                .iter()
                .filter_map(serde_json::Value::as_object)
                .collect()
        })
}

#[tokio::test]
async fn catalogue_advertises_doctor_with_an_object_schema()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let catalogue = ContextOsServer::catalogue();

    let doctor = catalogue
        .get("doctor")
        .ok_or_else(|| std::io::Error::other("missing tool doctor"))?;
    assert_eq!(
        doctor
            .input_schema
            .get("type")
            .and_then(serde_json::Value::as_str),
        Some("object")
    );
    assert!(doctor.description.is_some());
    let schema = doctor
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other("doctor omitted output schema"))?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor omitted output properties"))?;
    let mut fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
    fields.sort_unstable();
    // `doctor`'s output schema is the flat, merged success-and-`ToolFailure`
    // shape `fallible_output_schema_for` builds (`tool_contract.rs`'s
    // `every_remaining_tool_advertises_a_flat_fallible_output_schema`
    // covers this generally): `code`/`message`/`path`/`remediation` are
    // `ToolFailure`'s fields, present because `doctor`'s error path
    // populates `structured_content` with that shape too.
    assert_eq!(
        fields,
        vec![
            "checks",
            "code",
            "has_failures",
            "message",
            "path",
            "remediation"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn fr_90_doctor_reports_a_healthy_vault_with_no_failures()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        structured(&result)?.get("has_failures"),
        Some(&json!(false))
    );
    let managed = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes check reported"))?;
    assert_eq!(managed.get("status"), Some(&json!("pass")));
    assert_eq!(managed.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(managed.get("remediation_tool"), None);
    Ok(())
}

#[tokio::test]
async fn fr_90_doctor_reports_a_missing_managed_index_as_auto_fixable()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let managed = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes check reported"))?;
    assert_eq!(managed.get("status"), Some(&json!("fail")));
    assert_eq!(managed.get("auto_fixable"), Some(&json!(true)));
    assert_eq!(
        managed.get("remediation_tool"),
        Some(&json!("vault_index_rebuild"))
    );
    Ok(())
}

#[tokio::test]
async fn fr_95_doctor_reports_invalid_frontmatter_as_never_auto_fixable()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("broken.md"),
        "---\ntitle: Notes: something happened\n---\nBody\n",
    )?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let frontmatter = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Frontmatter validity")))
        .ok_or_else(|| std::io::Error::other("no Frontmatter validity check reported"))?;
    assert_eq!(frontmatter.get("status"), Some(&json!("fail")));
    assert_eq!(frontmatter.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(frontmatter.get("remediation_tool"), None);
    let message = frontmatter
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Frontmatter validity omitted message"))?;
    assert!(message.contains("broken.md"), "{message}");
    Ok(())
}
