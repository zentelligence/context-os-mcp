//! Vault content rendering pipeline (`web-rendering.md`): Markdown/OFM,
//! triple-colon fences, wikilinks/embeds, callouts, `.base` HTMX views,
//! `.canvas` SVG, and Mermaid SVG.
//!
//! Every stage that needs no MCP round trip (fence/callout/wikilink
//! scanning, frontmatter strip, fence/callout HTML composition) is a pure
//! function of its own text input and is unit-tested without any MCP
//! dependency, matching `testing.md`'s layering. The stages that do need
//! one (wikilink/embed resolution, Mermaid rendering, `.base`/`.canvas`
//! data) live in [`markdown`], [`base`], and [`canvas`]'s own
//! orchestration functions, which call out through an injected
//! [`mcp_client::McpClient`](crate::mcp_client::McpClient).

pub mod base;
pub mod block_ids;
pub mod callouts;
pub mod canvas;
pub mod comments;
pub mod diagnostics;
pub mod fences;
pub mod frontmatter;
pub mod highlight;
pub mod markdown;
pub mod page;
pub mod shell;
pub mod tags;
pub mod wikilinks;

pub use diagnostics::Diagnostic;

/// Escapes `input` for safe inclusion in HTML text or attribute-value
/// context. Used by every hand-composed HTML fragment in this module (the
/// pure fence/callout/wikilink renderers); Askama-templated fragments
/// ([`diagnostics`], [`base`], [`canvas`]) rely on Askama's own default
/// auto-escaping instead, so this helper is not used there.
#[must_use]
pub fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
