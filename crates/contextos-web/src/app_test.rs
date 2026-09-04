use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::build_router;
use crate::mcp_client::McpClientSet;

#[tokio::test]
async fn the_proxy_route_is_reachable_through_the_assembled_router()
-> Result<(), Box<dyn std::error::Error>> {
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let dir = tempfile::tempdir()?;
    let router = build_router(clients, dir.path());

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/nope/some_tool")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"], "mcp/server-not-configured");
    Ok(())
}

#[tokio::test]
async fn the_static_route_is_reachable_through_the_assembled_router()
-> Result<(), Box<dyn std::error::Error>> {
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("app.js"), b"console.log('hi');")?;
    let router = build_router(clients, dir.path());

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}
