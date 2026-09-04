//! Vault content routes (`web-routes.md` §2, FR-220 to FR-225a):
//! `GET /{{vault_name}}/{{relative-path}}` and its directory form, plus the
//! `POST`/`PATCH`/`PUT`/`DELETE` mutation dispatch in
//! [`mutate`](super::vault_mutations).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::{base, canvas, markdown, page};

/// Shared state a vault content route needs: the connected MCP sessions,
/// and the name of the `[[mcp_server]]` entry every vault operation is
/// issued against (`web.toml`'s first configured entry, `FR-203`: "the
/// first entry always the local `contextos-mcp` instance").
#[derive(Clone)]
pub struct VaultRoutesState {
    pub clients: Arc<McpClientSet>,
    pub primary_server: String,
}

impl VaultRoutesState {
    fn client(&self) -> Result<&Arc<McpClient>, Box<Response>> {
        self.clients
            .get(&self.primary_server)
            .ok_or_else(|| Box::new(server_not_configured()))
    }
}

fn server_not_configured() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "mcp/server-not-configured",
    )
}

fn error_response(status: StatusCode, code: &'static str) -> Response {
    #[derive(Serialize)]
    struct Body {
        error: &'static str,
    }
    (status, Json(Body { error: code })).into_response()
}

fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "route/not-found")
}

fn unreachable() -> Response {
    error_response(StatusCode::BAD_GATEWAY, "mcp/unreachable")
}

#[derive(Debug, Default, Deserialize)]
pub struct VaultQuery {
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub view: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileInfoResult {
    kind: String,
}

enum Kind {
    File,
    Dir,
}

async fn resolve_kind(
    client: &McpClient,
    vault_name: &str,
    trimmed: &str,
) -> Result<Kind, Box<Response>> {
    let target = if trimmed.is_empty() {
        format!("{vault_name}://.")
    } else {
        format!("{vault_name}://{trimmed}")
    };
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(target));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Err(McpCallError::Unreachable { .. }) => Err(Box::new(unreachable())),
        Ok(result) if result.is_error == Some(true) => Err(Box::new(not_found())),
        Ok(result) => match result.into_typed::<FileInfoResult>() {
            Ok(info) if info.kind == "dir" => Ok(Kind::Dir),
            Ok(_) => Ok(Kind::File),
            Err(_) => Err(Box::new(not_found())),
        },
    }
}

#[derive(Debug, Deserialize)]
struct ReadTextResult {
    content: String,
}

async fn read_text(client: &McpClient, path: String) -> Result<String, Box<Response>> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(path));
    match client.call_tool("fs_read_text_file".to_owned(), args).await {
        Err(McpCallError::Unreachable { .. }) => Err(Box::new(unreachable())),
        Ok(result) if result.is_error == Some(true) => Err(Box::new(not_found())),
        Ok(result) => result
            .into_typed::<ReadTextResult>()
            .map(|r| r.content)
            .map_err(|_| Box::new(not_found())),
    }
}

pub(crate) fn extension_of(relative_path: &str) -> &str {
    relative_path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map_or("", |(_, ext)| ext)
}

fn html_response(html: String) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

fn raw_response(content: String) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        content,
    )
        .into_response()
}

/// `GET /{{vault_name}}/` (root directory, `FR-224`).
pub async fn get_root(
    State(state): State<VaultRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(
        state,
        vault_name,
        String::new(),
        VaultQuery::default(),
        headers,
    )
    .await
}

/// `GET /{{vault_name}}/{{relative-path}}` (`FR-220` to `FR-224`).
pub async fn get_path(
    State(state): State<VaultRoutesState>,
    Path((vault_name, relative_path)): Path<(String, String)>,
    Query(query): Query<VaultQuery>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, relative_path, query, headers).await
}

async fn get_dispatch(
    state: VaultRoutesState,
    vault_name: String,
    relative_path: String,
    query: VaultQuery,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let trimmed = relative_path.trim_end_matches('/');
    let kind = match resolve_kind(client, &vault_name, trimmed).await {
        Ok(kind) => kind,
        Err(response) => return *response,
    };
    match kind {
        Kind::Dir => render_directory(client, &vault_name, trimmed).await,
        Kind::File => {
            render_file(
                client,
                &vault_name,
                trimmed,
                &query,
                headers.contains_key("hx-request"),
            )
            .await
        }
    }
}

async fn render_directory(client: &McpClient, vault_name: &str, trimmed: &str) -> Response {
    let index_path = if trimmed.is_empty() {
        format!("{vault_name}://index.md")
    } else {
        format!("{vault_name}://{trimmed}/index.md")
    };
    let raw = match read_text(client, index_path).await {
        Ok(raw) => raw,
        Err(response) => return *response,
    };
    let rendered = markdown::render(client, vault_name, &raw, 0).await;
    html_response(page::render_page(trimmed, &rendered.html))
}

async fn render_file(
    client: &McpClient,
    vault_name: &str,
    relative_path: &str,
    query: &VaultQuery,
    is_hx_request: bool,
) -> Response {
    let extension = extension_of(relative_path).to_ascii_lowercase();
    let vault_path = format!("{vault_name}://{relative_path}");

    if query.raw.as_deref() == Some("1") {
        return match read_text(client, vault_path).await {
            Ok(content) => raw_response(content),
            Err(response) => *response,
        };
    }

    match extension.as_str() {
        "md" => {
            let raw = match read_text(client, vault_path).await {
                Ok(raw) => raw,
                Err(response) => return *response,
            };
            let rendered = markdown::render(client, vault_name, &raw, 0).await;
            html_response(page::render_page(relative_path, &rendered.html))
        }
        "base" => {
            match base::render_view(client, vault_name, relative_path, query.view.as_deref()).await
            {
                Ok(fragment) if is_hx_request => html_response(fragment),
                Ok(fragment) => html_response(page::render_page(relative_path, &fragment)),
                Err(McpCallError::Unreachable { .. }) => unreachable(),
            }
        }
        "canvas" => render_canvas(client, vault_name, relative_path).await,
        "mermaid" => render_standalone_mermaid(client, vault_name, relative_path).await,
        _ => render_other_file(client, vault_name, relative_path).await,
    }
}

/// Any extension not otherwise dispatched (`web-routes.md` §2's dispatch
/// table, "anything else"): served as the MCP-resolved MIME type, reusing
/// `fs_attach_file`'s existing detection (`D-15`), never a 404 for a real
/// file that simply has no dedicated rendering pipeline.
async fn render_other_file(client: &McpClient, vault_name: &str, relative_path: &str) -> Response {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{relative_path}")),
    );
    let result = match client.call_tool("fs_attach_file".to_owned(), args).await {
        Ok(result) => result,
        Err(McpCallError::Unreachable { .. }) => return unreachable(),
    };
    if result.is_error == Some(true) {
        return not_found();
    }
    for block in &result.content {
        let rmcp::model::ContentBlock::Resource(embedded) = block else {
            continue;
        };
        match &embedded.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                mime_type, text, ..
            } => {
                let content_type = mime_type
                    .clone()
                    .unwrap_or_else(|| "text/plain; charset=utf-8".to_owned());
                return (
                    StatusCode::OK,
                    [("content-type", content_type)],
                    text.clone(),
                )
                    .into_response();
            }
            rmcp::model::ResourceContents::BlobResourceContents {
                mime_type, blob, ..
            } => {
                use base64::Engine;
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(blob) {
                    let content_type = mime_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".to_owned());
                    return (StatusCode::OK, [("content-type", content_type)], bytes)
                        .into_response();
                }
            }
            // `ResourceContents` is `#[non_exhaustive]`: `fs_attach_file`
            // only ever returns the two variants matched above, but a
            // future third variant must fail closed here (fall through to
            // `not_found()` below), never panic.
            _ => {}
        }
    }
    not_found()
}

#[derive(Debug, Deserialize)]
struct CanvasReadResult {
    nodes: Vec<canvas::CanvasNode>,
    edges: Vec<canvas::CanvasEdge>,
    #[serde(default)]
    diagnostics: Vec<crate::rendering::Diagnostic>,
}

async fn render_canvas(client: &McpClient, vault_name: &str, relative_path: &str) -> Response {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{relative_path}")),
    );
    let result = match client.call_tool("canvas_read".to_owned(), args).await {
        Ok(result) => result,
        Err(McpCallError::Unreachable { .. }) => return unreachable(),
    };
    let Ok(parsed) = result.into_typed::<CanvasReadResult>() else {
        return not_found();
    };
    let body = if parsed.diagnostics.is_empty() {
        canvas::render_svg(&parsed.nodes, &parsed.edges, vault_name)
    } else {
        crate::rendering::diagnostics::render_diagnostic_panel(&parsed.diagnostics)
    };
    html_response(page::render_page(relative_path, &body))
}

async fn render_standalone_mermaid(
    client: &McpClient,
    vault_name: &str,
    relative_path: &str,
) -> Response {
    let raw = match read_text(client, format!("{vault_name}://{relative_path}")).await {
        Ok(raw) => raw,
        Err(response) => return *response,
    };
    let body = markdown::render_mermaid_source(client, &raw).await;
    html_response(page::render_page(relative_path, &body))
}

#[cfg(test)]
#[path = "vault_test.rs"]
mod tests;
