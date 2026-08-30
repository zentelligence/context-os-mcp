//! FR-61 / FR-62: the authenticated streamable HTTP transport and its parity
//! with the stdio transport.
//!
//! HTTP-client choice for the parity, soak, and shutdown-flush tests: these
//! drive the server through the official rmcp streamable-HTTP reqwest client
//! (`rmcp` features `client` + `transport-streamable-http-client-reqwest`,
//! dev-dependency only) rather than hand-rolled raw HTTP. The vendored rmcp
//! `=2.2.0` client already implements the full streamable-HTTP JSON-RPC
//! protocol (session negotiation, SSE/JSON response handling, retries), so
//! reusing it is more robust than reimplementing that protocol by hand, and
//! it exercises the server the same way a real MCP host would. The one
//! exception is the auth-rejection matrix and the body-cap test, which
//! intentionally build raw `http` requests against the `axum::Router`
//! directly (via `tower::ServiceExt::oneshot`) because those cases are
//! rejected by our own middleware before any MCP semantics apply, and a
//! socket-free `oneshot` call is both simpler and faster than a real client
//! round trip.
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use contextos_server::{Config, ContextOsServer, HttpConfig, HttpTransportError};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const INIT_BODY_2026_07_28: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"http-transport-test","version":"1.0"}}}"#;

fn http_config(bind: &str, token: &str) -> HttpConfig {
    HttpConfig {
        bind: bind.to_owned(),
        token: token.to_owned(),
        max_body_kb: 2048,
    }
}

/// Binds an ephemeral loopback port, serves `router` on it, and returns the
/// base MCP URL plus a handle and cancellation token the caller uses to shut
/// the server down. Binding completes synchronously (the OS backlog is
/// established before `TcpListener::bind` returns), so no sleep is needed to
/// know the server is ready to accept connections.
async fn spawn_http_server(
    server: ContextOsServer,
    http: &HttpConfig,
) -> Result<(SocketAddr, String, JoinHandle<()>, CancellationToken), BoxError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_server::build_router(server, http)?;
    let shutdown = CancellationToken::new();
    let shutdown_for_serve = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_serve.cancelled().await })
            .await;
    });
    let url = format!("http://{addr}{}", contextos_server::HTTP_MOUNT_PATH);
    Ok((addr, url, handle, shutdown))
}

async fn stop_http_server(
    handle: JoinHandle<()>,
    shutdown: CancellationToken,
) -> Result<(), BoxError> {
    shutdown.cancel();
    handle.await?;
    Ok(())
}

async fn http_client(
    url: &str,
    token: &str,
) -> Result<rmcp::service::RunningService<rmcp::RoleClient, ()>, BoxError> {
    let config =
        StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(config);
    Ok(().serve(transport).await?)
}

async fn call_http_tool(
    url: &str,
    token: &str,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let client = http_client(url, token).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.cancel().await?;
    let result = result?;
    if result.is_error == Some(true) {
        return Err(std::io::Error::other(format!(
            "{name} returned a tool error: {:?}",
            result.structured_content
        ))
        .into());
    }
    Ok(result)
}

async fn call_http_list_tools(url: &str, token: &str) -> Result<Vec<Tool>, BoxError> {
    let client = http_client(url, token).await?;
    let tools = client.list_all_tools().await;
    client.cancel().await?;
    Ok(tools?)
}

fn structured<'a>(
    result: &'a CallToolResult,
    context: &str,
) -> Result<&'a serde_json::Map<String, Value>, BoxError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| {
            std::io::Error::other(format!("{context} omitted structured content")).into()
        })
}

async fn assert_auth_rejected(router: Router, request: Request<Body>) -> Result<(), BoxError> {
    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let parsed: Value = serde_json::from_slice(&body)?;
    assert_eq!(parsed.get("code"), Some(&json!("auth/missing-token")));
    Ok(())
}

#[tokio::test]
async fn fr_61_auth_rejection_matrix() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let token = "matrix-token";
    let http = http_config("127.0.0.1:0", token);
    let router = contextos_server::build_router(server.clone(), &http)?;
    let mount = contextos_server::HTTP_MOUNT_PATH;

    // No Authorization header.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::empty())?,
    )
    .await?;

    // Wrong scheme.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(Body::empty())?,
    )
    .await?;

    // Wrong token.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Bearer wrong-token")
            .body(Body::empty())?,
    )
    .await?;

    // Token supplied only as a query parameter must not be accepted.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(format!("{mount}?token={token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::empty())?,
    )
    .await?;

    // Correct bearer succeeds: a full initialize round trip over `oneshot`.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(mount)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::HOST, "127.0.0.1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(INIT_BODY_2026_07_28))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let parsed: Value = serde_json::from_slice(&body)?;
    assert_eq!(parsed.get("jsonrpc"), Some(&json!("2.0")));
    let result = parsed
        .get("result")
        .ok_or_else(|| std::io::Error::other("initialize response omitted result"))?;
    assert!(result.is_object());
    assert_eq!(
        result.get("protocolVersion"),
        Some(&json!("2026-07-28")),
        "server did not negotiate the FR-61 required protocol revision"
    );

    // Plus at least one real-socket positive case, over an actual TCP
    // connection rather than an in-process `oneshot` call.
    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;
    let tools = call_http_list_tools(&url, token).await?;
    assert!(!tools.is_empty());
    stop_http_server(handle, shutdown).await?;

    Ok(())
}

/// Configuration/build-level test: `validate_bind` is the "validate" half of
/// a deliberate validate-then-bind seam, so this never asks the operating
/// system to listen on `0.0.0.0`.
#[test]
fn fr_61_non_loopback_bind_without_token_is_refused() -> Result<(), BoxError> {
    let refusal = contextos_server::validate_bind("0.0.0.0:7331", "");
    let Err(HttpTransportError::NonLoopbackBindWithoutToken { bind }) = refusal else {
        return Err(std::io::Error::other("expected a NonLoopbackBindWithoutToken refusal").into());
    };
    assert_eq!(bind, "0.0.0.0:7331");
    let message = HttpTransportError::NonLoopbackBindWithoutToken { bind }.to_string();
    assert!(
        message.contains("0.0.0.0:7331"),
        "message omitted the bind: {message}"
    );
    assert!(
        message.to_lowercase().contains("token"),
        "message did not name a way to supply a token: {message}"
    );

    // The same bind with a token configured constructs without error.
    contextos_server::validate_bind("0.0.0.0:7331", "configured-token")?;

    Ok(())
}

#[tokio::test]
async fn fr_61_body_cap_rejects_oversize_requests() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let token = "cap-token";
    let http = HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        max_body_kb: 1,
    };
    let router = contextos_server::build_router(server, &http)?;

    let oversized = "x".repeat(4096);
    let request = Request::builder()
        .method("POST")
        .uri(contextos_server::HTTP_MOUNT_PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_LENGTH, oversized.len().to_string())
        .body(Body::from(oversized))?;

    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn fr_62_tool_catalogue_identical_across_transports() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let token = "parity-token";
    let http = http_config("127.0.0.1:0", token);

    // This instance's actual dispatch router (`D-25`: varies with
    // `[server] astro`, unlike the always-complete `ContextOsServer::catalogue()`),
    // read before `server` moves into `spawn_http_server` below.
    let stdio_tools = server.effective_catalogue().list_all();
    let mut stdio_names: Vec<String> = stdio_tools
        .iter()
        .map(|tool| tool.name.clone().into_owned())
        .collect();
    stdio_names.sort_unstable();

    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;
    let http_tools = call_http_list_tools(&url, token).await?;
    let mut http_names: Vec<String> = http_tools
        .iter()
        .map(|tool| tool.name.clone().into_owned())
        .collect();
    http_names.sort_unstable();

    assert_eq!(
        stdio_names, http_names,
        "tool catalogue names drifted between transports"
    );

    for tool in &stdio_tools {
        let counterpart = http_tools
            .iter()
            .find(|candidate| candidate.name == tool.name)
            .ok_or_else(|| {
                std::io::Error::other(format!("HTTP catalogue omitted {}", tool.name))
            })?;
        assert_eq!(
            *counterpart.input_schema, *tool.input_schema,
            "{} input schema drifted between transports",
            tool.name
        );
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

#[tokio::test]
async fn fr_62_mixed_readwrite_soak_across_ten_clients() -> Result<(), BoxError> {
    const WRITER_COUNT: usize = 10;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let server = ContextOsServer::try_from(Config::try_from(vec![vault_path.clone()])?)?;
    let token = "soak-token";
    let http = http_config("127.0.0.1:0", token);
    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;

    let mut tasks = tokio::task::JoinSet::new();
    for writer in 0..WRITER_COUNT {
        let url = url.clone();
        let token = token.to_owned();
        tasks.spawn(async move {
            let path = format!("soak/writer-{writer}.md");
            let marker = format!("marker-soak-{writer}");
            let content = format!("# Writer {writer}\n\n{marker}\n");

            call_http_tool(
                &url,
                &token,
                "fs_write_file",
                serde_json::from_value(json!({"path": path, "content": content}))?,
            )
            .await?;

            let read = call_http_tool(
                &url,
                &token,
                "fs_read_text_file",
                serde_json::from_value(json!({"path": path}))?,
            )
            .await?;
            let read_content = structured(&read, "fs_read_text_file")?
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| std::io::Error::other("fs_read_text_file omitted content"))?;
            if read_content != content {
                return Err(std::io::Error::other(
                    "fs_read_text_file returned content that did not match the write",
                )
                .into());
            }

            call_http_tool(
                &url,
                &token,
                "query_text",
                serde_json::from_value(json!({"query": marker}))?,
            )
            .await?;

            Ok::<(String, String), BoxError>((path, marker))
        });
    }

    let mut written = Vec::with_capacity(WRITER_COUNT);
    while let Some(joined) = tasks.join_next().await {
        written.push(joined??);
    }
    assert_eq!(written.len(), WRITER_COUNT);

    for (path, marker) in &written {
        let on_disk = std::fs::read_to_string(vault_path.join(path))?;
        if !on_disk.contains(marker.as_str()) {
            return Err(std::io::Error::other(format!(
                "file on disk at {path} is missing its marker"
            ))
            .into());
        }
    }

    for (_, marker) in &written {
        let hits = call_http_tool(
            &url,
            token,
            "query_text",
            serde_json::from_value(json!({"query": marker}))?,
        )
        .await?;
        let hit_count = structured(&hits, "query_text")?
            .get("hits")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        if hit_count == 0 {
            return Err(
                std::io::Error::other(format!("query_text found no hit for {marker}")).into(),
            );
        }
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

#[tokio::test]
async fn fr_61_shutdown_flushes_pending_commits() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    // A long debounce means only the explicit graceful-shutdown flush below
    // produces a commit, matching how
    // `fr_30_graceful_flush_commits_pending_paths_before_shutdown` isolates
    // the flush path from the automatic debounced commit.
    config.vaults[0].git.commit_debounce_s = 3600;
    let server = ContextOsServer::try_from(config)?;
    let token = "flush-token";
    let http = http_config("127.0.0.1:0", token);
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http).await?;

    call_http_tool(&url, token, "git_init", Map::new()).await?;
    call_http_tool(
        &url,
        token,
        "fs_write_file",
        serde_json::from_value(json!({"path": "shutdown-http.md", "content": "safe via http\n"}))?,
    )
    .await?;

    // The same graceful-shutdown flush path main.rs runs on SIGINT/SIGTERM,
    // triggered here programmatically rather than via a real OS signal.
    let commits = server.flush_git()?;

    assert_eq!(commits.len(), 1);
    assert!(commits[0].commit_id.is_some());
    assert!(
        commits[0]
            .committed_paths
            .iter()
            .any(|path| path == std::path::Path::new("shutdown-http.md"))
    );

    stop_http_server(handle, shutdown).await?;
    Ok(())
}
