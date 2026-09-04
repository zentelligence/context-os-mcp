use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use tower::ServiceExt as _;

use super::handle;
use crate::mcp_client::McpClientSet;

async fn router_with_no_configured_servers() -> Result<Router, Box<dyn std::error::Error>> {
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    Ok(Router::new()
        .route("/mcp/{server_name}/{tool_name}", post(handle))
        .with_state(clients))
}

#[tokio::test]
async fn an_unconfigured_server_name_is_a_404() -> Result<(), Box<dyn std::error::Error>> {
    let router = router_with_no_configured_servers().await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/does-not-exist/some_tool")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"], "mcp/server-not-configured");
    assert_eq!(json["server"], "does-not-exist");
    Ok(())
}

#[tokio::test]
async fn a_malformed_json_body_is_a_400_before_any_server_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let router = router_with_no_configured_servers().await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/does-not-exist/some_tool")
                .header("content-type", "application/json")
                .body(Body::from("not json"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"], "route/malformed-body");
    Ok(())
}

#[tokio::test]
async fn a_json_body_that_is_not_an_object_is_a_400() -> Result<(), Box<dyn std::error::Error>> {
    let router = router_with_no_configured_servers().await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/does-not-exist/some_tool")
                .header("content-type", "application/json")
                .body(Body::from("[1, 2, 3]"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn an_empty_body_is_treated_as_no_arguments_not_malformed()
-> Result<(), Box<dyn std::error::Error>> {
    let router = router_with_no_configured_servers().await?;

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/does-not-exist/some_tool")
                .body(Body::empty())?,
        )
        .await?;

    // An empty body is valid (no arguments); the 404 proves it reached the
    // server lookup rather than being rejected as malformed.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}
