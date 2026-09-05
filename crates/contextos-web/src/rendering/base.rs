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

use std::collections::HashMap;

use askama::Template;
use serde::Deserialize;
use serde_json::Value;

use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::diagnostics::{self, Diagnostic};

#[derive(Debug, Clone)]
struct RowColumn {
    /// The raw column key (`"status"`, `"formula.id_link"`): the row-edit
    /// form's own `data-field`, a real frontmatter key `frontmatter_update`
    /// can patch, so this is never replaced by `label`.
    name: String,
    /// The column's own `properties.<name>.displayName` from the `.base`
    /// document when set (`"ID"` for `formula.id_link`, say), falling back
    /// to `name` itself: purely the text shown to a reader, never used to
    /// address anything.
    label: String,
    value: String,
    /// `Some(href)` when this column's value is a `base_query` link
    /// (`file.asLink`'s resolved JSON shape, `{"type":"link", "target":
    /// ..., "display": ...}`): `value` is the link's own display text,
    /// rendered as an anchor to `href` rather than plain text.
    href: Option<String>,
    /// Whether `name` is a `formula.*` display column: computed, never
    /// real frontmatter, so the row-edit form never offers it as a
    /// patchable field (posting a formula name back through
    /// `frontmatter_update` would write a bogus frontmatter key, not
    /// update anything the note actually has).
    is_formula: bool,
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
    let (view_names, filters_json, display_names) = read_definition(mcp, vault_name, path).await?;

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
        .map(|row| row_view(row, &structured.columns, vault_name, &display_names))
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
/// tab strip), the top-level `filters` value (the view-definition editor's
/// pre-filled content), and every column's own `properties.<name>
/// .displayName` (a reader-facing label only, e.g. `"ID"` for
/// `formula.id_link`; the row-edit form still addresses the column by its
/// raw name, never this label). A read or parse failure degrades to an
/// empty tab strip, an empty filter editor, and raw column names as labels,
/// rather than failing the whole render: the row grid itself is still
/// meaningful without them.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
async fn read_definition(
    mcp: &McpClient,
    vault_name: &str,
    path: &str,
) -> Result<(Vec<String>, String, HashMap<String, String>), McpCallError> {
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = mcp.call_tool("base_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok((Vec::new(), String::new(), HashMap::new()));
    }
    let Ok(read) = result.into_typed::<BaseReadStructured>() else {
        return Ok((Vec::new(), String::new(), HashMap::new()));
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
    let display_names = read
        .definition
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .filter_map(|(name, definition)| {
                    let display_name = definition.get("displayName")?.as_str()?;
                    Some((name.clone(), display_name.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((view_names, filters_json, display_names))
}

/// Builds one row's card data. The card's own identity (title, link,
/// `frontmatter_update` target) is always the row's file path, found
/// however it happens to be available: an explicit `file.path` column when
/// the view's own `order` includes one, or otherwise any `file.asLink(...)`
/// column's own `target` (every such link necessarily targets the row's own
/// file, regardless of where that column sits in `order`). This is
/// deliberately independent of column order or position: `order` controls
/// only which data a view chooses to display, and in what sequence a reader
/// scans it, not which one is somehow "the" title, a distinction Obsidian's
/// own Bases schema does not draw either.
fn row_view(
    row: &serde_json::Map<String, Value>,
    columns: &[String],
    vault_name: &str,
    display_names: &HashMap<String, String>,
) -> RowView {
    let file_path = row
        .get("file.path")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| columns.iter().find_map(|name| link_target(row.get(name))));
    let (has_link, href) = match &file_path {
        Some(p) => (true, format!("/{vault_name}/{p}")),
        None => (false, String::new()),
    };
    // Obsidian's own `file.basename` (the file's own name without its
    // extension) is the more appropriate title text than `file.name`
    // (with it): a card header is a title, not a filename listing.
    let title = file_path.as_deref().map_or_else(
        || "(untitled)".to_owned(),
        |p| {
            std::path::Path::new(p)
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or(p)
                .to_owned()
        },
    );
    let column_views = columns
        .iter()
        .filter(|c| c.as_str() != "file.path")
        .map(|name| {
            let (value, href) = column_value(row.get(name), vault_name);
            RowColumn {
                name: name.clone(),
                label: display_names.get(name).cloned().unwrap_or_else(|| name.clone()),
                value,
                href,
                is_formula: name.starts_with("formula."),
            }
        })
        .collect();
    RowView {
        title,
        has_link,
        href,
        note_path: file_path,
        columns: column_views,
    }
}

/// The vault-relative path a `base_query` link (`file.asLink`'s resolved
/// JSON shape) targets, or `None` for any other value shape.
fn link_target(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::Object(object)) if object.get("type").and_then(Value::as_str) == Some("link") => {
            object.get("target").and_then(Value::as_str).map(str::to_owned)
        }
        _ => None,
    }
}

/// Resolves one column's display text and, when the column's value is a
/// `base_query` link, the vault route it should link to.
fn column_value(value: Option<&Value>, vault_name: &str) -> (String, Option<String>) {
    if let Some(target) = link_target(value) {
        let display = value
            .and_then(Value::as_object)
            .and_then(|object| object.get("display"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return (display, Some(format!("/{vault_name}/{target}")));
    }
    match value {
        Some(value) => (stringify(value), None),
        None => (String::new(), None),
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
