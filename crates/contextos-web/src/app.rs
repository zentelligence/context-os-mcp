//! Startup orchestration and router assembly.
//!
//! [`connect`] is the handshake gate this crate requires before serving any
//! request: it resolves only once every configured `[[mcp_server]]` session
//! has completed its `initialize` handshake, so [`build_router`]'s result
//! never serves a request against a session that has not yet, or never
//! will, come up. A caller that wires `connect` before binding any listener
//! (as `main.rs` does) gets this gate for free: a failed handshake surfaces
//! before the process ever accepts a TCP connection.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::config::WebConfig;
use crate::mcp_client::{McpClientSet, McpConnectError};
use crate::routes::apps::AppRoutesState;
use crate::routes::settings::SettingsRoutesState;
use crate::routes::vault::VaultRoutesState;
use crate::{proxy, routes, static_assets};

/// Connects every `[[mcp_server]]` entry in `config`, failing fast on the
/// first handshake failure.
///
/// # Errors
///
/// Propagates the first [`McpConnectError`] any configured entry produces.
pub async fn connect(config: &WebConfig) -> Result<Arc<McpClientSet>, McpConnectError> {
    let clients = McpClientSet::connect(&config.mcp_servers).await?;
    Ok(Arc::new(clients))
}

/// Builds the full HTTP router: the MCP proxy route, `/static/`, the vault
/// content routes, the app registry routes (`web-apps.md` §4), and
/// `/settings/`.
///
/// Vault content, app registry, and settings routes issue every tool call
/// against `primary_server` (`web.toml`'s first configured `[[mcp_server]]`
/// entry is always the local `contextos-mcp` instance), distinct from the
/// MCP proxy route, which is addressed by whatever `server_name` the caller
/// names in the URL itself. A registered app's own `manifest.toml`
/// `mcp_servers` allow-list is validated against every configured
/// `[[mcp_server]]` name, not just `primary_server` (`clients.names()`, so
/// an app may depend on a non-primary server too); `/settings/`'s own
/// registered-app-dependency check reuses the identical app-discovery path
/// for the same reason.
///
/// App discovery runs lazily, on each vault's first registry-route
/// request, and is cached thereafter ([`routes::apps::AppRoutesState`]);
/// this mirrors the vault content routes' own established pattern of
/// resolving a vault's identity per request through an MCP tool call
/// rather than this crate maintaining a second, locally pre-loaded vault
/// registry. `/settings/`'s dependency check runs its own, uncached
/// discovery pass per write instead (`routes::settings`), since a stale
/// cache could let a removal through that a fresh scan would have blocked.
///
/// `web_config_path` is `web.toml`'s own path on disk, read and
/// validate-then-written by `/settings/`; it is unrelated to `static_dir`
/// even though both are configured under `web.toml`'s own `[server]` table.
pub fn build_router(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    primary_server: String,
) -> Router {
    let web_config_path_buf = Arc::new(web_config_path.to_path_buf());
    let vault_state = VaultRoutesState {
        clients: Arc::clone(&clients),
        primary_server: primary_server.clone(),
        web_config_path: Arc::clone(&web_config_path_buf),
    };
    let app_state = AppRoutesState::new(
        Arc::clone(&clients),
        primary_server.clone(),
        clients.names(),
        Arc::clone(&web_config_path_buf),
    );
    let settings_state = SettingsRoutesState::new(Arc::clone(&clients), primary_server, web_config_path.to_path_buf());
    // Four separate state types cannot share one `Router<S>`'s single state
    // slot, so each sub-router resolves its own state before all four are
    // merged into one `Router<()>`.
    let proxy_and_static = Router::new()
        .route("/mcp/{server_name}/{tool_name}", post(proxy::handle))
        .nest_service("/static", static_assets::service(static_dir))
        .with_state(clients);
    let apps = Router::new()
        .route("/{vault_name}/apps/", get(routes::apps::list))
        .route("/{vault_name}/apps/rescan", post(routes::apps::rescan))
        .route("/{vault_name}/apps/{slug}/", get(routes::apps::serve_root))
        .route("/{vault_name}/apps/{slug}/{*sub_path}", get(routes::apps::serve_path))
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
    let settings = Router::new()
        .route(
            "/settings/",
            get(routes::settings::get)
                .post(routes::settings::mutate)
                .patch(routes::settings::mutate)
                .put(routes::settings::mutate)
                .delete(routes::settings::mutate),
        )
        .with_state(settings_state);
    proxy_and_static.merge(apps).merge(vault).merge(settings)
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
