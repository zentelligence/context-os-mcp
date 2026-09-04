//! `/settings/` (`web-routes.md` §5): a `web.toml`-scoped configuration
//! UI. `GET` renders the current effective configuration; `POST` adds an
//! `[[mcp_server]]` entry; `PATCH` partially updates an entry or
//! `[server.ui]`; `PUT` fully replaces an entry; `DELETE` removes one.
//! Every writing method validates the resulting document
//! ([`crate::config_writer::WebConfigDocument`]) and checks that no
//! currently-registered app's manifest still names a server the edit would
//! remove, before persisting anything; a rejected edit leaves `web.toml`
//! byte-for-byte unchanged. No method here ever opens `config.toml`: the
//! vault list it holds is exclusively a `contextos-mcp` CLI or hand-edit
//! concern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::apps::{self, RegisteredApp};
use crate::atomic_write::write_atomically;
use crate::config::{McpServerConfig, WebConfig, current_appearance};
use crate::config_writer::{WebConfigDocument, WebConfigWriterError};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};

/// Shared state the settings route needs: the connected MCP sessions (to
/// run the registered-app-dependency check required before removing an
/// `[[mcp_server]]` entry), the `[[mcp_server]]` name every such check is
/// issued against, and the on-disk path of the `web.toml` this route reads
/// and writes.
#[derive(Clone)]
pub struct SettingsRoutesState {
    clients: Arc<McpClientSet>,
    primary_server: String,
    web_config_path: Arc<PathBuf>,
}

impl SettingsRoutesState {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>, primary_server: String, web_config_path: PathBuf) -> Self {
        Self {
            clients,
            primary_server,
            web_config_path: Arc::new(web_config_path),
        }
    }

    fn client(&self) -> Option<&Arc<McpClient>> {
        self.clients.get(&self.primary_server)
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    used_by: Vec<AppReference>,
}

#[derive(Serialize)]
struct AppReference {
    vault: String,
    slug: String,
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: code,
            detail: None,
            server: None,
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

fn malformed_body() -> Response {
    error(StatusCode::BAD_REQUEST, "route/malformed-body")
}

fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "route/method-not-allowed")
}

fn invalid_configuration(detail: impl std::fmt::Display) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorBody {
            error: "settings/invalid-configuration",
            detail: Some(detail.to_string()),
            server: None,
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

fn mcp_server_in_use(server: &str, used_by: Vec<AppReference>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorBody {
            error: "settings/mcp-server-in-use",
            detail: Some(format!(
                "mcp_server {server:?} cannot be removed: at least one registered app's \
                 manifest still names it"
            )),
            server: Some(server.to_owned()),
            used_by,
        }),
    )
        .into_response()
}

fn unreachable() -> Response {
    error(StatusCode::BAD_GATEWAY, "mcp/unreachable")
}

fn server_not_configured() -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, "mcp/server-not-configured")
}

fn read_failure(source: &std::io::Error) -> Response {
    invalid_configuration(format!("web.toml could not be read: {source}"))
}

// ---------------------------------------------------------------------
// GET /settings/
// ---------------------------------------------------------------------

/// One `[[mcp_server]]` entry as the settings page renders it: `detail` is
/// the read-only summary line; the remaining fields are structured so the
/// per-entry edit form (`PATCH ... target: "mcp_server"`) can pre-fill
/// exactly the fields its own transport has, `token_env` included (the
/// "auth for an MCP server" case, distinct from `contextos-web`'s own,
/// deliberately deferred, HTTP surface auth).
struct McpServerRow {
    name: String,
    transport: &'static str,
    detail: String,
    command: Option<String>,
    args: Option<String>,
    endpoint: Option<String>,
    token_env: Option<String>,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    bind: String,
    static_dir: String,
    log_level: String,
    log_file: String,
    ui: Vec<(String, String)>,
    mcp_servers: Vec<McpServerRow>,
}

fn render(config: &WebConfig) -> String {
    let mcp_servers = config
        .mcp_servers
        .iter()
        .map(|entry| match entry {
            McpServerConfig::Stdio { name, command, args } => McpServerRow {
                name: name.clone(),
                transport: "stdio",
                detail: format!("{command} {}", args.join(" ")),
                command: Some(command.clone()),
                args: Some(args.join(" ")),
                endpoint: None,
                token_env: None,
            },
            McpServerConfig::Http {
                name,
                endpoint,
                token_env,
            } => McpServerRow {
                name: name.clone(),
                transport: "http",
                detail: token_env.as_ref().map_or_else(
                    || endpoint.clone(),
                    |variable| format!("{endpoint} (token via ${variable})"),
                ),
                command: None,
                args: None,
                endpoint: Some(endpoint.clone()),
                token_env: token_env.clone(),
            },
        })
        .collect();
    let ui = config
        .server
        .ui
        .iter()
        .map(|(key, value)| (key.clone(), ui_value_display(value)))
        .collect();
    let template = SettingsTemplate {
        bind: config.server.bind.clone(),
        static_dir: config.server.static_dir.as_ref().map_or_else(
            || "(none configured; bundled assets serve /static/)".to_owned(),
            |path| path.display().to_string(),
        ),
        log_level: format!("{:?}", config.server.log_level).to_ascii_lowercase(),
        log_file: config.server.log_file.clone(),
        ui,
        mcp_servers,
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>Settings could not be rendered.</p>".to_owned())
}

/// A `[server.ui]` value as plain text for the appearance form's editable
/// inputs: a bare string's own content (an operator editing "dark" should
/// see `dark`, not the TOML-syntax `"dark"` a raw `Display` would produce),
/// falling back to `toml::Value`'s own TOML-syntax rendering for every
/// other value kind (`[server.ui]`'s key set is intentionally unenumerated,
/// `config.rs`, so a non-string value is possible and must still display
/// as something re-parseable).
fn ui_value_display(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn html_response(html: String) -> Response {
    (StatusCode::OK, [("content-type", "text/html; charset=utf-8")], html).into_response()
}

/// `GET /settings/`: renders the current effective `web.toml`. Follows
/// `standards/http-routing-response-contract-standard.md`'s `HX-Request`
/// convention (`web-architecture.md` §6): a plain browser navigation (no
/// `HX-Request` header) gets the full page, chrome and all; an
/// HTMX-driven request gets the bare fragment it will swap in.
pub async fn get(State(state): State<SettingsRoutesState>, headers: HeaderMap) -> Response {
    match read_current(&state.web_config_path) {
        Ok(config) => {
            let fragment = render(&config);
            if headers.contains_key("hx-request") {
                html_response(fragment)
            } else {
                let mut nav = settings_nav(&state).await;
                nav.appearance = current_appearance(&state.web_config_path);
                html_response(page::render_page(&nav, "Settings", &fragment))
            }
        }
        Err(response) => *response,
    }
}

/// Builds the nav shell's data for `/settings/`: vault-independent
/// (`shell::build_nav`'s `vault_name: None` path, so no tree section
/// renders), degrading to an empty vault switcher rather than failing the
/// whole page when the primary MCP session is unavailable, since the
/// switcher is a convenience the settings page itself does not depend on.
async fn settings_nav(state: &SettingsRoutesState) -> page::NavData {
    match state.client() {
        Some(client) => shell::build_nav(client, ActiveScreen::Settings, None, None, None).await,
        None => page::NavData {
            vaults: Vec::new(),
            current_vault: None,
            nav_target_vault: None,
            directory_label: None,
            entries: Vec::new(),
            breadcrumb: "settings".to_owned(),
            active_vault_screen: false,
            active_apps_screen: false,
            active_settings_screen: true,
            rescan_href: None,
            appearance: crate::config::Appearance::default(),
        },
    }
}

fn read_current(path: &Path) -> Result<WebConfig, Box<Response>> {
    let source = std::fs::read_to_string(path).map_err(|source| Box::new(read_failure(&source)))?;
    let config: WebConfig = toml::from_str(&source)
        .map_err(|source| Box::new(invalid_configuration(format!("web.toml is invalid: {source}"))))?;
    Ok(config)
}

// ---------------------------------------------------------------------
// POST / PATCH / PUT / DELETE /settings/
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
enum PatchBody {
    McpServer { name: String, patch: Map<String, Value> },
    Ui { patch: Map<String, Value> },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteBody {
    name: String,
}

enum Edit {
    AddMcpServer(McpServerConfig),
    PatchMcpServer {
        name: String,
        patch: Map<String, Value>,
    },
    PatchUi(Map<String, Value>),
    ReplaceMcpServer {
        current_name: String,
        entry: McpServerConfig,
    },
    RemoveMcpServer(String),
}

fn parse_edit(method: &Method, body: &[u8]) -> Result<Edit, Box<Response>> {
    match *method {
        Method::POST => {
            let entry: McpServerConfig = serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            Ok(Edit::AddMcpServer(entry))
        }
        Method::PATCH => {
            let parsed: PatchBody = serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            Ok(match parsed {
                PatchBody::McpServer { name, patch } => Edit::PatchMcpServer { name, patch },
                PatchBody::Ui { patch } => Edit::PatchUi(patch),
            })
        }
        Method::PUT => {
            let mut map: Map<String, Value> =
                serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            let Some(Value::String(current_name)) = map.remove("current_name") else {
                return Err(Box::new(malformed_body()));
            };
            let entry: McpServerConfig =
                serde_json::from_value(Value::Object(map)).map_err(|_source| Box::new(malformed_body()))?;
            Ok(Edit::ReplaceMcpServer { current_name, entry })
        }
        Method::DELETE => {
            let parsed: DeleteBody = serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            Ok(Edit::RemoveMcpServer(parsed.name))
        }
        _ => Err(Box::new(method_not_allowed())),
    }
}

/// `McpServerConfig` round-trips through JSON as an object (an internally
/// tagged enum over string/array fields only): the only failure mode left
/// once a value of this type exists is a bug in this conversion itself, not
/// anything a caller's request body could trigger.
fn json_object(entry: &McpServerConfig) -> Map<String, Value> {
    match serde_json::to_value(entry) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

enum ApplyOutcome {
    Ok,
    UnknownMcpServer(String),
    Invalid(WebConfigWriterError),
}

fn apply_edit(document: &mut WebConfigDocument, edit: &Edit) -> ApplyOutcome {
    let result = match edit {
        Edit::AddMcpServer(entry) => document.add_mcp_server(&json_object(entry)),
        Edit::PatchMcpServer { name, patch } => document.patch_mcp_server(name, patch),
        Edit::PatchUi(patch) => document.patch_ui(patch),
        Edit::ReplaceMcpServer { current_name, entry } => {
            document.replace_mcp_server(current_name, &json_object(entry))
        }
        Edit::RemoveMcpServer(name) => document.remove_mcp_server(name),
    };
    match result {
        Ok(()) => ApplyOutcome::Ok,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) => ApplyOutcome::UnknownMcpServer(name),
        Err(other) => ApplyOutcome::Invalid(other),
    }
}

/// `POST`/`PATCH`/`PUT`/`DELETE /settings/`.
pub async fn mutate(State(state): State<SettingsRoutesState>, method: Method, body: axum::body::Bytes) -> Response {
    let edit = match parse_edit(&method, &body) {
        Ok(edit) => edit,
        Err(response) => return *response,
    };

    let source = match std::fs::read_to_string(state.web_config_path.as_path()) {
        Ok(source) => source,
        Err(source) => return read_failure(&source),
    };
    let mut document = match WebConfigDocument::parse(&source) {
        Ok(document) => document,
        Err(WebConfigWriterError::Toml { source }) => {
            return invalid_configuration(format!("web.toml is invalid: {source}"));
        }
        Err(other) => return invalid_configuration(other),
    };
    let before_names = document.mcp_server_names();

    match apply_edit(&mut document, &edit) {
        ApplyOutcome::Ok => {}
        ApplyOutcome::UnknownMcpServer(name) => return unknown_mcp_server(&name),
        ApplyOutcome::Invalid(error) => return invalid_configuration(error),
    }

    let after_names = document.mcp_server_names();
    let removed: Vec<String> = before_names
        .iter()
        .filter(|name| !after_names.contains(name))
        .cloned()
        .collect();

    if !removed.is_empty() {
        let Some(client) = state.client() else {
            return server_not_configured();
        };
        match dependent_apps(client, &before_names, &removed).await {
            Ok(conflicts) if conflicts.is_empty() => {}
            Ok(conflicts) => {
                let server = removed
                    .iter()
                    .find(|name| conflicts.iter().any(|c| &c.server == *name))
                    .cloned()
                    .unwrap_or_else(|| removed[0].clone());
                let used_by = conflicts
                    .into_iter()
                    .filter(|c| c.server == server)
                    .map(|c| AppReference {
                        vault: c.vault,
                        slug: c.slug,
                    })
                    .collect();
                return mcp_server_in_use(&server, used_by);
            }
            Err(McpCallError::Unreachable { .. }) => return unreachable(),
        }
    }

    let rendered = document.render();
    if let Err(source) = write_atomically(&state.web_config_path, rendered.as_bytes()) {
        return read_failure(&source);
    }

    match read_current(&state.web_config_path) {
        Ok(config) => html_response(render(&config)),
        Err(response) => *response,
    }
}

struct DependencyConflict {
    vault: String,
    slug: String,
    server: String,
}

#[derive(Debug, Deserialize)]
struct VaultInfoEntry {
    name: String,
}

#[derive(Debug, Deserialize)]
struct VaultInfoResult {
    vaults: Vec<VaultInfoEntry>,
}

/// Lists every configured vault's `name` via `vault_info`, the same MCP
/// tool call an operator would use, rather than `contextos-web` maintaining
/// a second, locally pre-loaded vault registry (the same established
/// precedent the vault content routes already follow, extended here to
/// this check).
async fn configured_vault_names(client: &McpClient) -> Result<Vec<String>, McpCallError> {
    let result = client.call_tool("vault_info".to_owned(), Map::new()).await?;
    let Ok(parsed) = result.into_typed::<VaultInfoResult>() else {
        return Ok(Vec::new());
    };
    Ok(parsed.vaults.into_iter().map(|entry| entry.name).collect())
}

/// For every configured vault, discovers its registered apps (using
/// `known_before`, the server list as it stood *before* this edit, so a
/// currently-valid app's registration is not spuriously invalidated by the
/// very edit under validation) and reports every `(vault, app, server)`
/// triple where the app's manifest names one of `removed`.
async fn dependent_apps(
    client: &McpClient,
    known_before: &[String],
    removed: &[String],
) -> Result<Vec<DependencyConflict>, McpCallError> {
    let vault_names = configured_vault_names(client).await?;
    let mut conflicts = Vec::new();
    for vault_name in vault_names {
        let apps: Vec<RegisteredApp> = apps::discover_apps(client, &vault_name, known_before).await?;
        for app in apps {
            for server in &app.mcp_servers {
                if removed.iter().any(|name| name == server) {
                    conflicts.push(DependencyConflict {
                        vault: vault_name.clone(),
                        slug: app.slug.clone(),
                        server: server.clone(),
                    });
                }
            }
        }
    }
    Ok(conflicts)
}

fn unknown_mcp_server(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: "settings/unknown-mcp-server",
            detail: Some(format!("no [[mcp_server]] entry named {name:?} is configured")),
            server: Some(name.to_owned()),
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;
