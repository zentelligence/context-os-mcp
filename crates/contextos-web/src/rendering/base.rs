//! `.base` HTMX-driven rendering (`web-rendering.md` §2): a `.base`
//! file is a view definition (filters, formulas, views) evaluated against
//! the vault, not a data container; its rendered rows are separate note
//! files elsewhere in the vault. This module renders the active view's
//! matched rows as a card grid via `base_query`, the named view itself
//! switched by the real `?view=` route parameter `routes::vault` already
//! resolves through `base_query` (never a client-side re-filter of
//! already-fetched rows); editing either a row's own content or the view
//! definition itself is a distinct MCP tool call (`frontmatter_update` or
//! `base_apply` respectively), dispatched by `routes::vault_mutations`,
//! never by this rendering module.

use askama::Template;
use serde::Deserialize;
use serde_json::Value;

use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::diagnostics::{self, Diagnostic};

#[derive(Debug, Clone)]
struct RowColumn {
    name: String,
    value: String,
}

#[derive(Debug, Clone)]
struct RowView {
    title: String,
    has_link: bool,
    href: String,
    /// The row's own vault-relative note path, the row-edit form's
    /// `frontmatter_update` target; `None` when the row has no `file.path`
    /// column at all, matching `has_link`.
    note_path: Option<String>,
    columns: Vec<RowColumn>,
}

/// One entry in the view-switcher tab strip (`web-rendering.md` §2): every
/// view the `.base` file's own definition declares, switched via a real
/// `GET ...?view=<name>` request through `base_query`, not a client-side
/// re-filter.
#[derive(Debug, Clone)]
struct ViewTab {
    name: String,
    active: bool,
}

#[derive(Template)]
#[template(path = "base_view.html")]
struct BaseViewTemplate<'a> {
    vault_name: &'a str,
    path: &'a str,
    views: &'a [ViewTab],
    matched: u64,
    truncated: bool,
    rows: &'a [RowView],
    diagnostics_html: String,
    has_diagnostics: bool,
    /// The file's own top-level `filters` value, pretty-printed JSON, the
    /// view-definition editor's pre-filled textarea content; empty when
    /// unset or unreadable.
    filters_json: String,
}

#[derive(Debug, Deserialize)]
struct BaseQueryStructured {
    content: String,
    columns: Vec<String>,
    matched: u64,
    truncated: bool,
    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Deserialize)]
struct BaseReadStructured {
    definition: serde_json::Map<String, Value>,
}

/// Renders the addressed `.base` file's active view as an HTMX card grid
/// (`web-rendering.md` §2), or a diagnostic panel when the file itself
/// fails to parse (a diagnostic naming path `"$"`, this crate's convention
/// for a whole-file parse failure).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails; a tool-level error or a parse/render failure is reported inline
/// as a diagnostic panel in the returned `Ok(String)` instead, matching
/// `web-rendering.md`'s never-a-server-error contract for this content
/// type.
pub async fn render_view(
    mcp: &McpClient,
    vault_name: &str,
    path: &str,
    view: Option<&str>,
) -> Result<String, McpCallError> {
    let (view_names, filters_json) = read_definition(mcp, vault_name, path).await?;

    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    if let Some(view_name) = view {
        args.insert("view".to_owned(), Value::String(view_name.to_owned()));
    }
    args.insert("format".to_owned(), Value::String("json".to_owned()));

    let result = mcp.call_tool("base_query".to_owned(), args).await?;
    let Ok(structured) = result.into_typed::<BaseQueryStructured>() else {
        return Ok(diagnostics::render_diagnostic_panel(&[Diagnostic {
            code: "base/query-failed".to_owned(),
            path: path.to_owned(),
            message: "The base view could not be evaluated.".to_owned(),
        }]));
    };

    if structured.diagnostics.iter().any(|d| d.path == "$") {
        return Ok(diagnostics::render_diagnostic_panel(&structured.diagnostics));
    }

    let active_view = view.map_or_else(|| view_names.first().cloned(), |v| Some(v.to_owned()));
    let views: Vec<ViewTab> = view_names
        .into_iter()
        .map(|name| {
            let active = active_view.as_deref() == Some(name.as_str());
            ViewTab { name, active }
        })
        .collect();

    let rows_json: Vec<serde_json::Map<String, Value>> = serde_json::from_str(&structured.content).unwrap_or_default();
    let rows: Vec<RowView> = rows_json
        .iter()
        .map(|row| row_view(row, &structured.columns, vault_name))
        .collect();

    let diagnostics_html = if structured.diagnostics.is_empty() {
        String::new()
    } else {
        diagnostics::render_diagnostic_panel(&structured.diagnostics)
    };

    let template = BaseViewTemplate {
        vault_name,
        path,
        views: &views,
        matched: structured.matched,
        truncated: structured.truncated,
        rows: &rows,
        has_diagnostics: !structured.diagnostics.is_empty(),
        diagnostics_html,
        filters_json,
    };
    Ok(template.render().unwrap_or_else(|_| {
        diagnostics::render_diagnostic_panel(&[Diagnostic {
            code: "base/render-failed".to_owned(),
            path: path.to_owned(),
            message: "The base view template could not be rendered.".to_owned(),
        }])
    }))
}

/// Reads the `.base` file's own definition via `base_read` for the parts
/// `base_query`'s result does not carry: every declared view's name (the
/// tab strip) and the top-level `filters` value (the view-definition
/// editor's pre-filled content). A read or parse failure degrades to an
/// empty tab strip and an empty filter editor rather than failing the
/// whole render: the row grid itself is still meaningful without them.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
async fn read_definition(mcp: &McpClient, vault_name: &str, path: &str) -> Result<(Vec<String>, String), McpCallError> {
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = mcp.call_tool("base_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok((Vec::new(), String::new()));
    }
    let Ok(read) = result.into_typed::<BaseReadStructured>() else {
        return Ok((Vec::new(), String::new()));
    };
    let view_names = read
        .definition
        .get("views")
        .and_then(Value::as_array)
        .map(|views| {
            views
                .iter()
                .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let filters_json = read
        .definition
        .get("filters")
        .map(|filters| serde_json::to_string_pretty(filters).unwrap_or_default())
        .unwrap_or_default();
    Ok((view_names, filters_json))
}

fn row_view(row: &serde_json::Map<String, Value>, columns: &[String], vault_name: &str) -> RowView {
    let file_path = row.get("file.path").and_then(Value::as_str);
    let (has_link, href) = match file_path {
        Some(p) => (true, format!("/{vault_name}/{p}")),
        None => (false, String::new()),
    };
    let title = file_path
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_owned())
        .or_else(|| columns.first().and_then(|c| row.get(c)).map(stringify))
        .unwrap_or_else(|| "(untitled)".to_owned());
    let column_views = columns
        .iter()
        .filter(|c| c.as_str() != "file.path")
        .map(|name| RowColumn {
            name: name.clone(),
            value: row.get(name).map(stringify).unwrap_or_default(),
        })
        .collect();
    RowView {
        title,
        has_link,
        href,
        note_path: file_path.map(str::to_owned),
        columns: column_views,
    }
}

fn stringify(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(items) => items.iter().map(stringify).collect::<Vec<_>>().join(", "),
        Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
#[path = "base_test.rs"]
mod tests;
