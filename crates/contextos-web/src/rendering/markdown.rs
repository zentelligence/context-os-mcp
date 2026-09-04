//! Markdown/OFM rendering pipeline (`web-rendering.md` §1, FR-221, FR-240,
//! FR-241, FR-242): frontmatter strip, Mermaid extraction, wikilink/embed
//! extraction, triple-colon fence and callout resolution, then general
//! Markdown, in that stage order.
//!
//! [`compile`] is the pure half: it extracts every occurrence needing an
//! MCP round trip (wikilink/embed target, Mermaid source) into a
//! placeholder token without making one itself, so it is unit-tested
//! without any MCP dependency (`testing.md`). [`render`] (async) resolves
//! them against a live [`McpClient`] and substitutes the final HTML.
//! Mermaid and wikilink/embed extraction run once, globally, before
//! triple-colon fence and callout extraction, so a fence or callout body
//! may itself contain a resolved wikilink or embed (their placeholder
//! tokens simply land inside that block's own inner HTML, substituted in
//! the same final pass as every top-level occurrence); a fence body may
//! additionally contain a Mermaid diagram, since its lines carry no extra
//! prefix. A callout body's lines each carry a leading `>` (Markdown
//! blockquote continuation), which the global Mermaid scan does not
//! dedent before looking for a fence-open line, so a Mermaid diagram
//! inside a callout is not supported in v1: a documented scope limit, not
//! an oversight. Fences and callouts do not themselves nest.

use std::collections::HashMap;

use pulldown_cmark::{Options, Parser, html};
use serde::Deserialize;

use crate::mcp_client::McpClient;
use crate::rendering::diagnostics::{self, Diagnostic};
use crate::rendering::wikilinks::{LinkOccurrence, LinkSyntax};
use crate::rendering::{callouts, fences, frontmatter, wikilinks};

/// An embed nests at most one level deep (FR-240): a target that is itself
/// an embed of a third file renders as a plain link at that second level,
/// never a second recursive inline.
const MAX_EMBED_DEPTH: u8 = 1;

/// Converts already OFM-extension-resolved text (frontmatter, fences,
/// callouts, wikilinks, and Mermaid already reduced to raw HTML or
/// placeholder tokens) to HTML via CommonMark/GFM (`web-rendering.md` §1
/// stage 5's non-OFM-specific remainder): tables, strikethrough,
/// footnotes, task lists, and smart punctuation enabled, matching OFM's
/// own superset.
#[must_use]
pub fn html_from_commonmark(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    let parser = Parser::new_ext(source, options);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

#[must_use]
fn mermaid_placeholder(index: usize) -> String {
    format!("\u{E000}MERMAID:{index}\u{E000}")
}

/// Extracts every top-level Mermaid-fenced block (an info string of
/// exactly `mermaid` on an opening triple-backtick fence) from `text`,
/// replacing each with its placeholder. A differently-fenced block's body
/// is consumed as opaque content, so a literal Mermaid-fence line inside it
/// (for example, documentation about Mermaid syntax) is never mistaken for
/// a real fence, mirroring `contextos-mcp`'s own `extract_mermaid_fence`.
fn extract_mermaid(text: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut sources = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            let is_mermaid = info.trim().eq_ignore_ascii_case("mermaid");
            if let Some(close) = lines[i + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with("```"))
            {
                let body_start = i + 1;
                let body_end = body_start + close;
                if is_mermaid {
                    let index = sources.len();
                    sources.push(lines[body_start..body_end].join("\n"));
                    if out_lines.last().is_some_and(|l: &String| !l.is_empty()) {
                        out_lines.push(String::new());
                    }
                    out_lines.push(mermaid_placeholder(index));
                    out_lines.push(String::new());
                } else {
                    out_lines.extend(lines[i..=body_end].iter().map(|l| (*l).to_owned()));
                }
                i = body_end + 1;
                continue;
            }
        }
        out_lines.push(lines[i].to_owned());
        i += 1;
    }
    (out_lines.join("\n"), sources)
}

/// Replaces a block-level placeholder (a fence, callout, embed, or Mermaid
/// token that landed on its own paragraph) with `replacement`. Falls back
/// to a bare token replace when the token was not wrapped in `<p>...</p>`
/// (a defensive fallback; every extractor emits its placeholder on a
/// blank-line-delimited line, so this should not occur in practice).
fn replace_block_placeholder(html: &str, token: &str, replacement: &str) -> String {
    let wrapped = format!("<p>{token}</p>");
    if html.contains(&wrapped) {
        html.replacen(&wrapped, replacement, 1)
    } else {
        html.replacen(token, replacement, 1)
    }
}

/// The pure "compile" result: HTML still carrying `LINK`/`EMBED`/`MERMAID`
/// placeholder tokens, plus what each needs resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub html: String,
    /// Wikilink/embed occurrences in original scan order; index `i`
    /// corresponds to `wikilinks::placeholder(i)` or
    /// `wikilinks::embed_placeholder(i)` depending on `occurrences[i].syntax`.
    pub occurrences: Vec<LinkOccurrence>,
    /// Mermaid sources in original scan order; index `i` corresponds to
    /// `mermaid_placeholder(i)`.
    pub mermaid_sources: Vec<String>,
}

/// Runs every MCP-free stage of the pipeline (`web-rendering.md` §1 stages
/// 1 to 5, minus Mermaid's own render call).
#[must_use]
pub fn compile(raw_source: &str) -> Compiled {
    let body = frontmatter::strip(raw_source);
    let (body, mermaid_sources) = extract_mermaid(body);
    let wiki = wikilinks::extract(&body);
    let fence_result = fences::extract(&wiki.text);
    let callout_result = callouts::extract(&fence_result.text);
    let mut html = html_from_commonmark(&callout_result.text);

    for (index, block) in callout_result.blocks.iter().enumerate() {
        let body_html = html_from_commonmark(&block.body);
        let rendered = callouts::render(&block.open, &body_html);
        html = replace_block_placeholder(&html, &callouts::placeholder(index), &rendered);
    }
    for (index, block) in fence_result.blocks.iter().enumerate() {
        let inner_html = html_from_commonmark(&block.inner);
        let rendered = fences::render(&block.open, &inner_html);
        html = replace_block_placeholder(&html, &fences::placeholder(index), &rendered);
    }

    Compiled {
        html,
        occurrences: wiki.occurrences,
        mermaid_sources,
    }
}

/// The final rendered page body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMarkdown {
    pub html: String,
}

/// Resolves every placeholder [`compile`] left behind against a live
/// [`McpClient`] and returns the final HTML (FR-244: a pure function of
/// the tool calls' own results, so identical vault state renders
/// byte-identical output).
pub async fn render(
    mcp: &McpClient,
    vault_name: &str,
    raw_source: &str,
    embed_depth: u8,
) -> RenderedMarkdown {
    let compiled = compile(raw_source);
    let mut html = compiled.html;

    for (index, source) in compiled.mermaid_sources.iter().enumerate() {
        let rendered = render_mermaid_block(mcp, source).await;
        html = replace_block_placeholder(&html, &mermaid_placeholder(index), &rendered);
    }

    // A single render pass resolves each distinct wikilink/embed target at
    // most once: a note repeating the same target many times (a heavily
    // cross-referenced index page) would otherwise cost one MCP round trip
    // per occurrence rather than per distinct target, the dominant cost
    // `NFR-W03`'s performance budget actually measures.
    let mut href_cache: HashMap<String, Option<String>> = HashMap::new();
    for (index, occurrence) in compiled.occurrences.iter().enumerate() {
        match occurrence.syntax {
            LinkSyntax::Link => {
                let href =
                    resolve_href_cached(mcp, vault_name, &occurrence.target, &mut href_cache).await;
                let rendered = match href {
                    Some(h) => wikilinks::render_link(occurrence, &h),
                    None => wikilinks::render_dead_link(occurrence),
                };
                html = html.replacen(&wikilinks::placeholder(index), &rendered, 1);
            }
            LinkSyntax::Embed => {
                let rendered = render_embed_occurrence(
                    mcp,
                    vault_name,
                    occurrence,
                    embed_depth,
                    &mut href_cache,
                )
                .await;
                html = replace_block_placeholder(
                    &html,
                    &wikilinks::embed_placeholder(index),
                    &rendered,
                );
            }
        }
    }

    RenderedMarkdown { html }
}

async fn resolve_href_cached(
    mcp: &McpClient,
    vault_name: &str,
    target: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    if let Some(cached) = cache.get(target) {
        return cached.clone();
    }
    let resolved = resolve_href(mcp, vault_name, target).await;
    cache.insert(target.to_owned(), resolved.clone());
    resolved
}

async fn render_embed_occurrence(
    mcp: &McpClient,
    vault_name: &str,
    occurrence: &LinkOccurrence,
    embed_depth: u8,
    href_cache: &mut HashMap<String, Option<String>>,
) -> String {
    let Some(candidate) =
        resolve_href_cached(mcp, vault_name, &occurrence.target, href_cache).await
    else {
        return wikilinks::render_dead_link(occurrence);
    };
    if embed_depth >= MAX_EMBED_DEPTH {
        // A doubly-embedded file renders as a plain link at the second
        // level, never a second recursive inline (FR-240).
        return wikilinks::render_link(occurrence, &candidate);
    }
    let relative_path = candidate
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix(vault_name))
        .and_then(|p| p.strip_prefix('/'))
        .unwrap_or(&candidate);
    let path = format!("{vault_name}://{relative_path}");
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), serde_json::Value::String(path));
    let Ok(result) = mcp.call_tool("fs_read_text_file".to_owned(), args).await else {
        return wikilinks::render_dead_link(occurrence);
    };
    if result.is_error == Some(true) {
        return wikilinks::render_dead_link(occurrence);
    }
    let Ok(read) = result.into_typed::<ReadTextResult>() else {
        return wikilinks::render_dead_link(occurrence);
    };
    let rendered = Box::pin(render(mcp, vault_name, &read.content, embed_depth + 1)).await;
    format!(
        "<div class=\"embed-block\"><div class=\"embed-label\">{label}</div>{body}</div>",
        label = crate::rendering::escape_html(&occurrence.target),
        body = rendered.html
    )
}

#[derive(Debug, Deserialize)]
struct ReadTextResult {
    content: String,
}

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

/// Resolves a wikilink target to its `/{{vault_name}}/{{relative-path}}`
/// route: an exact relative-path match (with `.md` assumed when the target
/// carries no extension) is tried first, falling back to a basename search
/// across the vault when exactly one candidate matches. `None` means dead
/// (FR-240).
async fn resolve_href(mcp: &McpClient, vault_name: &str, target: &str) -> Option<String> {
    let candidate = normalise_target(target);
    let exact = format!("{vault_name}://{candidate}");
    let mut args = serde_json::Map::new();
    args.insert("path".to_owned(), serde_json::Value::String(exact));
    if let Ok(result) = mcp.call_tool("fs_get_file_info".to_owned(), args).await
        && result.is_error != Some(true)
    {
        return Some(format!("/{vault_name}/{candidate}"));
    }

    let basename = candidate.rsplit('/').next().unwrap_or(&candidate);
    let mut search_args = serde_json::Map::new();
    search_args.insert(
        "path".to_owned(),
        serde_json::Value::String(format!("{vault_name}://.")),
    );
    search_args.insert(
        "pattern".to_owned(),
        serde_json::Value::String(format!("**/{basename}")),
    );
    let result = mcp
        .call_tool("fs_search_files".to_owned(), search_args)
        .await
        .ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let found = result.into_typed::<SearchFilesResult>().ok()?;
    if found.paths.len() == 1 {
        let relative = found.paths[0].trim_start_matches('/');
        Some(format!("/{vault_name}/{relative}"))
    } else {
        None
    }
}

fn normalise_target(target: &str) -> String {
    let last_segment = target.rsplit('/').next().unwrap_or(target);
    if last_segment.contains('.') {
        target.to_owned()
    } else {
        format!("{target}.md")
    }
}

/// Renders a standalone `.mermaid` file's own full content as one diagram
/// (`web-routes.md` §2: "standalone file, not a fenced block"), unlike a
/// ` ```mermaid ` fenced block within a `.md` note.
pub async fn render_mermaid_source(mcp: &McpClient, source: &str) -> String {
    render_mermaid_block(mcp, source).await
}

async fn render_mermaid_block(mcp: &McpClient, source: &str) -> String {
    let mut args = serde_json::Map::new();
    args.insert(
        "source".to_owned(),
        serde_json::Value::String(source.to_owned()),
    );
    let Ok(result) = mcp.call_tool("mermaid_render".to_owned(), args).await else {
        return diagnostics::render_diagnostic_panel(&[Diagnostic {
            code: "mcp/unreachable".to_owned(),
            path: String::new(),
            message: "Mermaid rendering is unavailable.".to_owned(),
        }]);
    };
    if let Some(svg) = extract_svg(&result) {
        return format!("<div class=\"mermaid-diagram\">{svg}</div>");
    }
    let diagnostics_result = result.into_typed::<MermaidDiagnosticsResult>().map_or_else(
        |_| {
            vec![Diagnostic {
                code: "mermaid/render-failed".to_owned(),
                path: String::new(),
                message: "The diagram could not be rendered.".to_owned(),
            }]
        },
        |r| r.diagnostics,
    );
    diagnostics::render_diagnostic_panel(&diagnostics_result)
}

fn extract_svg(result: &rmcp::model::CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Resource(embedded) => match &embedded.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                mime_type: Some(mime),
                text,
                ..
            } if mime == "image/svg+xml" => Some(text.clone()),
            _ => None,
        },
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
struct MermaidDiagnosticsResult {
    diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
#[path = "markdown_test.rs"]
mod tests;
