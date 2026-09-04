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
use axum::routing::{get, post};

use crate::config::WebConfig;
use crate::mcp_client::{McpClientSet, McpConnectError};
use crate::routes::apps::AppRoutesState;
use crate::routes::vault::VaultRoutesState;
use crate::{proxy, routes, static_assets};

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

/// Builds the full HTTP router: the MCP proxy route (FR-210), `/static/`
/// (FR-250), the vault content routes (FR-220 to FR-225a), and the app
/// registry routes (FR-233 to FR-234, `web-apps.md` §4). `/settings/`
/// (FR-251) is Phase 17 work and is not part of this router yet.
///
/// Vault content and app registry routes issue every tool call against
/// `primary_server` (`web.toml`'s first configured `[[mcp_server]]` entry,
/// `FR-203`: "the first entry always the local `contextos-mcp` instance"),
/// distinct from the MCP proxy route, which is addressed by whatever
/// `server_name` the caller names in the URL itself. A registered app's own
/// `manifest.toml` `mcp_servers` allow-list (`FR-232`) is validated against
/// every configured `[[mcp_server]]` name, not just `primary_server`
/// (`clients.names()`, so an app may depend on a non-primary server too).
///
/// App discovery (FR-230) runs lazily, on each vault's first
/// registry-route request, and is cached thereafter
/// ([`routes::apps::AppRoutesState`]); this mirrors the vault content
/// routes' own established pattern (`D-W05`) of resolving a vault's
/// identity per request through an MCP tool call rather than this crate
/// maintaining a second, locally pre-loaded vault registry.
pub fn build_router(
    clients: Arc<McpClientSet>,
    static_dir: &Path,
    primary_server: String,
) -> Router {
    let vault_state = VaultRoutesState {
        clients: Arc::clone(&clients),
        primary_server: primary_server.clone(),
    };
    let app_state = AppRoutesState::new(Arc::clone(&clients), primary_server, clients.names());
    // Three separate state types cannot share one `Router<S>`'s single
    // state slot, so each sub-router resolves its own state before all
    // three are merged into one `Router<()>`.
    let proxy_and_static = Router::new()
        .route("/mcp/{server_name}/{tool_name}", post(proxy::handle))
        .nest_service("/static", static_assets::service(static_dir))
        .with_state(clients);
    let apps = Router::new()
        .route("/{vault_name}/apps/", get(routes::apps::list))
        .route("/{vault_name}/apps/rescan", post(routes::apps::rescan))
        .route("/{vault_name}/apps/{slug}/", get(routes::apps::serve_root))
        .route(
            "/{vault_name}/apps/{slug}/{*sub_path}",
            get(routes::apps::serve_path),
        )
        .with_state(app_state);
    let vault = Router::new()
        .route("/{vault_name}/", get(routes::vault::get_root))
        .route(
            "/{vault_name}/{*relative_path}",
            get(routes::vault::get_path)
                .post(routes::vault_mutations::mutate)
                .patch(routes::vault_mutations::mutate)
                .put(routes::vault_mutations::mutate)
                .delete(routes::vault_mutations::mutate),
        )
        .with_state(vault_state);
    proxy_and_static.merge(apps).merge(vault)
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
