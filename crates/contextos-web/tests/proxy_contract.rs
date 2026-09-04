//! FR-210 to FR-213: `POST /mcp/{server_name}/{tool_name}` against a real
//! `contextos-mcp` stdio session. Covers success, an MCP-level tool error
//! (still `200`), an unconfigured `server_name` (`404`), and a killed
//! `contextos-mcp` process mid-session (`502`, NFR-W05), the delivery-plan
//! Phase 14 gate's own enumeration of this contract test's required cases.

mod support;

use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt as _;

use contextos_web::config::McpServerConfig;
use contextos_web::mcp_client::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn connected_clients(entries: &[McpServerConfig]) -> Result<Arc<McpClientSet>, BoxError> {
    Ok(Arc::new(McpClientSet::connect(entries).await?))
}

#[tokio::test]
async fn a_successful_tool_call_returns_200_with_the_real_tool_result() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = connected_clients(&[entry]).await?;
    let router = contextos_web::build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/contextos/vault_info")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_ne!(json["isError"], serde_json::Value::Bool(true));
    let vaults = json["structuredContent"]["vaults"]
        .as_array()
        .ok_or("vault_info returns a vaults array")?;
    assert_eq!(vaults[0]["name"], "contract-fixture");
    Ok(())
}

#[tokio::test]
async fn an_mcp_level_tool_error_is_still_a_200() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = connected_clients(&[entry]).await?;
    let router = contextos_web::build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/contextos/fs_read_text_file")
                .header("content-type", "application/json")
                .body(Body::from(json!({"path": "does-not-exist.md"}).to_string()))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["isError"], serde_json::Value::Bool(true));
    assert!(json["structuredContent"]["code"].is_string());
    Ok(())
}

#[tokio::test]
async fn an_unconfigured_server_name_is_a_404_against_a_real_session() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = connected_clients(&[entry]).await?;
    let router = contextos_web::build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/some-other-server/vault_info")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_killed_contextos_mcp_process_is_a_502_not_a_hang() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = McpClientSet::connect(&[entry]).await?;
    let pid = clients
        .get("contextos")
        .ok_or("the just-connected client is present")?
        .pid()
        .ok_or("a stdio client reports its child PID")?;
    let clients = Arc::new(clients);
    let router = contextos_web::build_router(
        Arc::clone(&clients),
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;

    // The transport does not notice the closed pipe synchronously with the
    // kill signal; poll with a bounded overall timeout rather than a fixed
    // sleep, so the test is as fast as the OS allows and still
    // deterministic (it can only pass by observing the real 502, never by
    // outlasting a fixed delay).
    let poll = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/mcp/contextos/vault_info")
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))?,
                )
                .await?;
            if response.status() == StatusCode::BAD_GATEWAY {
                return Ok::<_, BoxError>(response);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let Ok(response) = poll else {
        return Err("the proxy must report 502 well before this timeout, never hang".into());
    };
    let response = response?;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"], "mcp/unreachable");
    Ok(())
}
