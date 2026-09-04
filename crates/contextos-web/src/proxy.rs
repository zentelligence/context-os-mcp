//! `POST /mcp/{{server_name}}/{{tool_name}}` (FR-210 to FR-213): the MCP
//! tool proxy every registered app's `callTool` (`FR-211`) and every direct
//! API consumer dispatches through.

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClientSet};

/// Shared state a proxy request needs: the connected MCP sessions.
/// Held as `Arc<McpClientSet>` rather than embedding it in a larger
/// `AppState` at this layer, since the proxy route's only dependency is the
/// client set (`app.rs` composes this alongside `/static/` at the router
/// level).
pub type SharedClients = Arc<McpClientSet>;

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<&'a str>,
}

fn error_response(status: StatusCode, code: &'static str, server: Option<&str>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: code,
            server,
        }),
    )
        .into_response()
}

/// Handles `POST /mcp/{{server_name}}/{{tool_name}}`.
///
/// Every branch logs exactly one `INFO` line (method, server, tool,
/// duration, outcome) before returning, and never includes the request
/// body or the tool's result content in that log line (`FR-212`,
/// `NFR-W04`).
pub async fn handle(
    State(clients): State<SharedClients>,
    Path((server_name, tool_name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let start = Instant::now();

    let Ok(arguments) = parse_arguments(&body) else {
        log_outcome(&server_name, &tool_name, start, "malformed-body");
        return error_response(StatusCode::BAD_REQUEST, "route/malformed-body", None);
    };

    let Some(client) = clients.get(&server_name) else {
        log_outcome(&server_name, &tool_name, start, "server-not-configured");
        return error_response(
            StatusCode::NOT_FOUND,
            "mcp/server-not-configured",
            Some(&server_name),
        );
    };

    match client.call_tool(tool_name.clone(), arguments).await {
        Ok(result) => {
            let outcome = if result.is_error == Some(true) {
                "tool-error"
            } else {
                "success"
            };
            log_outcome(&server_name, &tool_name, start, outcome);
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(McpCallError::Unreachable { .. }) => {
            log_outcome(&server_name, &tool_name, start, "unreachable");
            error_response(
                StatusCode::BAD_GATEWAY,
                "mcp/unreachable",
                Some(&server_name),
            )
        }
    }
}

/// Parses the raw request body as a JSON object, treating an empty body as
/// an empty arguments object (a tool that takes no arguments). Any other
/// shape (invalid JSON, a JSON value that is not an object) is
/// `route/malformed-body` (`400`).
fn parse_arguments(body: &[u8]) -> Result<Map<String, Value>, ()> {
    if body.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) | Err(_) => Err(()),
    }
}

fn log_outcome(server: &str, tool: &str, start: Instant, outcome: &'static str) {
    tracing::info!(
        method = "POST",
        server,
        tool,
        duration_ms = start.elapsed().as_millis(),
        outcome,
        "MCP proxy call"
    );
}

#[cfg(test)]
#[path = "proxy_test.rs"]
mod tests;
