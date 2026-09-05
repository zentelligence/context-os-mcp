//! App registry and app-serving routes (`web-routes.md` §4): `GET
//! /{{vault_name}}/apps/`, `GET /{{vault_name}}/apps/{{slug}}/...`, and
//! the operator-initiated rescan (`web-apps.md` §4's answered open
//! question) `POST /{{vault_name}}/apps/rescan`.
//!
//! Discovery ([`crate::apps::discover_apps`]) runs on a vault's first
//! registry-route request and is cached in [`AppRoutesState`] thereafter,
//! mirroring the vault content routes' own established pattern of
//! resolving a vault's identity per request through an MCP tool call
//! rather than this crate maintaining a second, locally pre-loaded vault
//! registry; there is exactly one discovery code path, exercised
//! identically by a real request and by this module's own tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};

use axum::http::{HeaderMap, StatusCode};

use crate::apps::{self, AppKind, AppStatus, AppTarget, RegisteredApp};
use crate::config;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{Attached, fetch_attached, html_response, not_found, server_not_configured, unreachable};

/// Shared state an app route needs: the connected MCP sessions, the
/// `[[mcp_server]]` name every vault operation is issued against, the
/// full configured `[[mcp_server]]` name list (a manifest's own
/// `mcp_servers` allow-list is validated against this), the per-vault
/// discovered-app cache, and `web.toml`'s own path (read fresh per
/// request for the nav shell's `[server.ui]` appearance; this route
/// never writes it).
#[derive(Clone)]
pub struct AppRoutesState {
    clients: Arc<McpClientSet>,
    primary_server: String,
    mcp_server_names: Arc<Vec<String>>,
    registry: Arc<RwLock<HashMap<String, Vec<RegisteredApp>>>>,
    web_config_path: Arc<PathBuf>,
}

#[derive(Clone, Copy)]
enum RegistryError {
    ServerNotConfigured,
    Unreachable,
}

impl From<McpCallError> for RegistryError {
    fn from(_source: McpCallError) -> Self {
        Self::Unreachable
    }
}

impl AppRoutesState {
    #[must_use]
    pub fn new(
        clients: Arc<McpClientSet>,
        primary_server: String,
        mcp_server_names: Vec<String>,
        web_config_path: Arc<PathBuf>,
    ) -> Self {
        Self {
            clients,
            primary_server,
            mcp_server_names: Arc::new(mcp_server_names),
            registry: Arc::new(RwLock::new(HashMap::new())),
            web_config_path,
        }
    }

    fn client(&self) -> Result<&Arc<McpClient>, RegistryError> {
        self.clients
            .get(&self.primary_server)
            .ok_or(RegistryError::ServerNotConfigured)
    }

    fn cached(&self, vault_name: &str) -> Option<Vec<RegisteredApp>> {
        self.registry.read().ok()?.get(vault_name).cloned()
    }

    fn store(&self, vault_name: &str, apps: Vec<RegisteredApp>) {
        if let Ok(mut guard) = self.registry.write() {
            guard.insert(vault_name.to_owned(), apps);
        }
    }

    /// Returns `vault_name`'s app list, discovering it first if this is
    /// the first request for that vault since startup or the last
    /// [`rescan`](Self::rescan).
    async fn apps_for(&self, vault_name: &str) -> Result<Vec<RegisteredApp>, RegistryError> {
        if let Some(cached) = self.cached(vault_name) {
            return Ok(cached);
        }
        self.rescan(vault_name).await
    }

    /// Forces a fresh discovery pass for `vault_name` (`web-apps.md` §4's
    /// answered rescan question), replacing whatever was cached. Not
    /// `pub`: only this module's own route handlers call it.
    async fn rescan(&self, vault_name: &str) -> Result<Vec<RegisteredApp>, RegistryError> {
        let client = self.client()?;
        let discovered = apps::discover_apps(client, vault_name, &self.mcp_server_names).await?;
        self.store(vault_name, discovered.clone());
        Ok(discovered)
    }
}

fn not_yet_supported(slug: &str) -> Response {
    #[derive(serde::Serialize)]
    struct Body<'a> {
        error: &'static str,
        slug: &'a str,
    }
    (
        StatusCode::NOT_FOUND,
        axum::Json(Body {
            error: "app/not-yet-supported",
            slug,
        }),
    )
        .into_response()
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. }) => Err(Box::new(unreachable())),
    }
}

fn registry_error_response(error: RegistryError) -> Response {
    match error {
        RegistryError::ServerNotConfigured => server_not_configured(),
        RegistryError::Unreachable => unreachable(),
    }
}

struct AppListEntry {
    name: String,
    slug: String,
    kind_label: &'static str,
    servable: bool,
    target_attr: &'static str,
    /// The mock's `<dl>` "Opens" row (`outbox/2026-09-04-contextos-web-
    /// mock.html`'s `tpl-apps`): a human label for `target_attr`, kept
    /// distinct from it since `target_attr` is an HTML attribute value and
    /// this is prose.
    opens_label: &'static str,
}

#[derive(Template)]
#[template(path = "apps_list.html")]
struct AppsListTemplate<'a> {
    vault_name: &'a str,
    apps: Vec<AppListEntry>,
}

fn list_entry(app: &RegisteredApp) -> AppListEntry {
    AppListEntry {
        name: app.name.clone(),
        slug: app.slug.clone(),
        kind_label: match app.kind {
            AppKind::Spa => "spa",
            AppKind::Htmx => "htmx",
        },
        servable: app.status == AppStatus::Supported,
        target_attr: match app.target {
            AppTarget::Blank => "_blank",
            AppTarget::Embed => "_self",
        },
        opens_label: match app.target {
            AppTarget::Blank => "new tab",
            AppTarget::Embed => "inline",
        },
    }
}

fn render_list(vault_name: &str, registered: &[RegisteredApp]) -> String {
    let template = AppsListTemplate {
        vault_name,
        apps: registered.iter().map(list_entry).collect(),
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>The app registry could not be rendered.</p>".to_owned())
}

/// `GET /{{vault_name}}/apps/`: lists every registered app.
/// Follows the identical `HX-Request` full-page/fragment split every other
/// full-page route in this crate uses
/// (`standards/http-routing-response-contract-standard.md`).
pub async fn list(State(state): State<AppRoutesState>, Path(vault_name): Path<String>, headers: HeaderMap) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(client, &vault_name).await {
        return *response;
    }
    match state.apps_for(&vault_name).await {
        Ok(registered) => {
            let fragment = render_list(&vault_name, &registered);
            if headers.contains_key("hx-request") {
                html_response(fragment)
            } else {
                // The nav shell and `web.toml`'s appearance are independent
                // reads: the appearance load is also a blocking filesystem
                // read and parse, so it runs on `spawn_blocking` rather than
                // the async executor thread, concurrently with the nav
                // shell's own MCP round trips rather than after them.
                let web_config_path = Arc::clone(&state.web_config_path);
                let (mut nav, appearance) = tokio::join!(
                    Box::pin(shell::build_nav(
                        client,
                        ActiveScreen::Apps,
                        Some(&vault_name),
                        Some("apps"),
                        None
                    )),
                    async move {
                        tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                            .await
                            .unwrap_or_default()
                    },
                );
                nav.appearance = appearance;
                html_response(page::render_page(&nav, "Apps", &fragment))
            }
        }
        Err(error) => registry_error_response(error),
    }
}

/// `POST /{{vault_name}}/apps/rescan` (`web-apps.md` §4): re-discovers
/// `vault_name`'s `registry/apps/` on demand (the refresh icon on
/// [`list`]'s page), then redirects back to the listing so a plain HTML
/// form submission (no client-side script required) re-renders the
/// refreshed list.
pub async fn rescan(State(state): State<AppRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(client, &vault_name).await {
        return *response;
    }
    match state.rescan(&vault_name).await {
        Ok(_apps) => Redirect::to(&format!("/{vault_name}/apps/")).into_response(),
        Err(error) => registry_error_response(error),
    }
}

/// `GET /{{vault_name}}/apps/{{slug}}/`.
pub async fn serve_root(
    State(state): State<AppRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
) -> Response {
    serve(state, vault_name, slug, String::new()).await
}

/// `GET /{{vault_name}}/apps/{{slug}}/{{*sub_path}}`.
pub async fn serve_path(
    State(state): State<AppRoutesState>,
    Path((vault_name, slug, sub_path)): Path<(String, String, String)>,
) -> Response {
    serve(state, vault_name, slug, sub_path).await
}

async fn serve(state: AppRoutesState, vault_name: String, slug: String, sub_path: String) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(client, &vault_name).await {
        return *response;
    }
    let registered = match state.apps_for(&vault_name).await {
        Ok(registered) => registered,
        Err(error) => return registry_error_response(error),
    };
    let Some(app) = registered.into_iter().find(|app| app.slug == slug) else {
        return not_found();
    };
    if app.status != AppStatus::Supported {
        return not_yet_supported(&slug);
    }

    let trimmed = sub_path.trim_start_matches('/');
    let target = if trimmed.is_empty() {
        app.entry.clone()
    } else {
        trimmed.to_owned()
    };
    let bundle_path = format!("{vault_name}://registry/apps/{slug}/{target}");
    match fetch_attached(client, bundle_path).await {
        Ok(Attached::Found(response)) => response,
        Ok(Attached::NotFound) => serve_entry_fallback(client, &vault_name, &slug, &app.entry).await,
        Err(McpCallError::Unreachable { .. }) => unreachable(),
    }
}

/// Standard SPA fallback (`web-routes.md` §4): an unmatched sub-path
/// serves the bundle's own entry file, letting the SPA's own client-side
/// router handle it.
async fn serve_entry_fallback(client: &McpClient, vault_name: &str, slug: &str, entry: &str) -> Response {
    let entry_path = format!("{vault_name}://registry/apps/{slug}/{entry}");
    match fetch_attached(client, entry_path).await {
        Ok(Attached::Found(response)) => response,
        Ok(Attached::NotFound) => not_found(),
        Err(McpCallError::Unreachable { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
