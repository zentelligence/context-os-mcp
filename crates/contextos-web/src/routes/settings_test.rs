use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use super::*;
use crate::app::build_router;

type BoxError = Box<dyn std::error::Error>;

const BASE: &str = r#"
[server]
bind = "127.0.0.1:7332"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--stdio"]
"#;

async fn router_with_web_toml(contents: &str) -> Result<(tempfile::TempDir, axum::Router), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), contents)?;
    // No real `contextos-mcp` process for these unit tests: every case here
    // either never removes an `[[mcp_server]]` name (so the dependency
    // check is never reached) or deliberately exercises the
    // `mcp/server-not-configured` path for a removal with no live session
    // at all.
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );
    Ok((dir, router))
}

async fn request(
    router: &axum::Router,
    method: Method,
    body: &str,
) -> Result<(StatusCode, serde_json::Value), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri("/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let value = if status == StatusCode::OK {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    Ok((status, value))
}

async fn get_body(router: &axum::Router) -> Result<String, BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/settings/").body(Body::empty())?)
        .await?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn get_renders_the_configured_mcp_server() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .clone()
        .oneshot(Request::builder().uri("/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("contextos"));
    assert!(body.contains("127.0.0.1:7332"));
    Ok(())
}

/// `web-architecture.md` §6: `/settings/` is an SSR+HTMX
/// surface, so a plain browser navigation (no `HX-Request` header) gets the
/// full page shell, htmx and its `json-enc` extension included, matching
/// `standards/http-routing-response-contract-standard.md`'s "no HX-Request
/// -> full HTML page" contract.
#[tokio::test]
async fn get_without_hx_request_returns_the_full_page_shell_with_htmx_loaded() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .oneshot(Request::builder().uri("/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("<html"));
    assert!(body.contains("/static/htmx.min.js"));
    assert!(body.contains("/static/htmx-ext-json-enc.min.js"));
    assert!(body.contains("hx-post=\"/settings/\""));
    assert!(body.contains("hx-delete=\"/settings/\""));
    // The HTTP-transport add form (endpoint + token_env), not just stdio.
    assert!(body.contains("id=\"mcp-server-add-http\""));
    assert!(body.contains("data-transport=\"http\""));
    // Every configured entry gets a per-row Edit toggle, not delete-only.
    assert!(body.contains("mcp-server-edit-toggle"));
    Ok(())
}

/// The `HX-Request: true` counterpart to the test above: an htmx-issued
/// request gets the bare fragment it swaps in, never the full page shell
/// again (`standards/http-routing-response-contract-standard.md`'s "HTMX
/// Surface: HTML fragment only").
#[tokio::test]
async fn get_with_hx_request_returns_a_bare_fragment() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/settings/")
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(!body.contains("<html"));
    assert!(body.contains("contextos"));
    Ok(())
}

#[tokio::test]
async fn post_adds_a_new_mcp_server_entry_and_a_following_get_reflects_it() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"stdio","name":"second","command":"other","args":[]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("second"));
    // A write response is what htmx swaps into place: a bare fragment,
    // never a second full page shell nested inside the first.
    assert!(!rendered.contains("<html"));

    assert!(get_body(&router).await?.contains("second"));
    Ok(())
}

#[tokio::test]
async fn post_with_a_duplicate_name_is_rejected_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"stdio","name":"contextos","command":"other","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "settings/invalid-configuration");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn post_with_a_malformed_body_is_a_400_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(&router, Method::POST, "not json").await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "route/malformed-body");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn a_fuzzed_body_naming_a_vault_field_never_touches_config_toml() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), BASE)?;
    std::fs::write(
        dir.path().join("config.toml"),
        "[[vault]]\npath = \"/some/real/vault\"\nname = \"real\"\n",
    )?;
    let before_config_toml = std::fs::read_to_string(dir.path().join("config.toml"))?;
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    // A submission with no legitimate mapping to `web.toml`'s own schema at
    // all (a `vault` array, the shape `config.toml`'s `[[vault]]` blocks
    // take): rejected as malformed, since `McpServerConfig` has no `vault`
    // field and this crate never opens `config.toml` from any handler.
    let (status, _body) = request(&router, Method::POST, r#"{"vault":[{"path":"/etc","name":"evil"}]}"#).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml"))?,
        before_config_toml,
        "config.toml must never be touched by a /settings/ submission"
    );
    Ok(())
}

#[tokio::test]
async fn patch_partially_updates_an_existing_mcp_server_entry() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"contextos","patch":{"command":"patched"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("patched"));
    Ok(())
}

/// The "auth for an MCP server" case (distinct from `contextos-web`'s own,
/// deliberately deferred, HTTP surface auth): an HTTP `[[mcp_server]]`
/// entry's `token_env` round-trips through `POST`, the read-only summary,
/// and the edit form's pre-filled value.
#[tokio::test]
async fn post_adds_an_http_mcp_server_with_a_token_env_and_it_is_visible_and_editable() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","token_env":"REMOTE_TOKEN"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("token via $REMOTE_TOKEN"), "{rendered}");
    // The edit form pre-fills the current token_env so an operator can see
    // and change it, not just the read-only summary line.
    assert!(
        rendered.contains("value=\"REMOTE_TOKEN\""),
        "edit form did not pre-fill token_env: {rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn patch_updates_an_http_mcp_servers_token_env() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(
        &(BASE.to_owned()
            + "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n"),
    )
    .await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"remote","patch":{"token_env":"REMOTE_TOKEN"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("token via $REMOTE_TOKEN"), "{rendered}");
    Ok(())
}

#[tokio::test]
async fn patch_partially_updates_server_ui() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(&router, Method::PATCH, r#"{"target":"ui","patch":{"theme":"dark"}}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("dark"));
    Ok(())
}

#[tokio::test]
async fn patch_on_an_unknown_mcp_server_name_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"does-not-exist","patch":{"command":"x"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    Ok(())
}

#[tokio::test]
async fn put_fully_replaces_an_existing_entry_keeping_its_name() -> Result<(), BoxError> {
    // Keeping the same `name` on both sides of the replacement means no
    // `[[mcp_server]]` name is removed, so this test (deliberately, no live
    // MCP session) never reaches the registered-app-dependency check;
    // `tests/settings_routes.rs`'s
    // `put_renaming_an_entry_with_no_dependent_apps_succeeds` covers the
    // rename case against a real session.
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PUT,
        r#"{"current_name":"contextos","transport":"stdio","name":"contextos","command":"full-replacement","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("full-replacement"));
    Ok(())
}

#[tokio::test]
async fn put_replacing_an_unknown_entry_is_a_404_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(
        &router,
        Method::PUT,
        r#"{"current_name":"does-not-exist","transport":"stdio","name":"x","command":"y","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn delete_removing_the_only_mcp_server_with_no_live_session_fails_closed() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    // Removing `contextos` would remove a name that was present before the
    // edit; with no live MCP session at all (`McpClientSet::connect(&[])`
    // in this test's own setup) the registered-app-dependency check cannot
    // run, so the edit is rejected rather than silently skipping the check.
    let (status, body) = request(&router, Method::DELETE, r#"{"name":"contextos"}"#).await?;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "mcp/server-not-configured");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn delete_on_an_unknown_name_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(&router, Method::DELETE, r#"{"name":"does-not-exist"}"#).await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    Ok(())
}

#[tokio::test]
async fn get_returns_a_validation_error_when_web_toml_cannot_be_read() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    // No web.toml written at all.
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(
        clients,
        dir.path(),
        &dir.path().join("web.toml"),
        "contextos".to_owned(),
    );

    let response = router
        .oneshot(Request::builder().uri("/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}
