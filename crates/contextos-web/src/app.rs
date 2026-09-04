//! Startup orchestration and router assembly.
//!
//! [`connect`] is the handshake gate FR-204 requires: it resolves only once
//! every configured `[[mcp_server]]` session has completed its `initialize`
//! handshake, so [`build_router`]'s result never serves a request against a
//! session that has not yet, or never will, come up. A caller that wires
//! `connect` before binding any listener (as `main.rs` does) gets this gate
//! for free: a failed handshake surfaces before the process ever accepts a
//! TCP connection.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::routing::post;

use crate::config::WebConfig;
use crate::mcp_client::{McpClientSet, McpConnectError};
use crate::{proxy, static_assets};

/// Connects every `[[mcp_server]]` entry in `config`, failing fast on the
/// first handshake failure (FR-204).
///
/// # Errors
///
/// Propagates the first [`McpConnectError`] any configured entry produces.
pub async fn connect(config: &WebConfig) -> Result<Arc<McpClientSet>, McpConnectError> {
    let clients = McpClientSet::connect(&config.mcp_servers).await?;
    Ok(Arc::new(clients))
}

/// Builds the full HTTP router: the MCP proxy route (FR-210) and `/static/`
/// (FR-250). Vault content routes (FR-220 to FR-225) and `/settings/`
/// (FR-251) are Phase 15 and Phase 17 work respectively and are not part of
/// this phase's router.
pub fn build_router(clients: Arc<McpClientSet>, static_dir: &Path) -> Router {
    Router::new()
        .route("/mcp/{server_name}/{tool_name}", post(proxy::handle))
        .nest_service("/static", static_assets::service(static_dir))
        .with_state(clients)
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
