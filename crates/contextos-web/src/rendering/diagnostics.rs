//! Shared diagnostic-panel rendering (`web-rendering.md` §2 step 5, §3
//! final paragraph, §4): a `.base`/`.canvas`/Mermaid parse or render
//! failure renders as an inline error panel instead of the structured
//! content, never a server error.

use std::fmt::Write as _;

use askama::Template;
use serde::Deserialize;

/// One diagnostic entry, the same shape `base_read`/`canvas_read`/
/// `mermaid_validate` already report at the tool layer (`D-31`), and the
/// shape this module deserialises a tool's `structured_content` into
/// directly (field names match verbatim).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

#[derive(Template)]
#[template(path = "diagnostic_panel.html")]
struct DiagnosticPanelTemplate<'a> {
    diagnostics: &'a [Diagnostic],
}

/// Renders `diagnostics` as the inline error panel every structured-content
/// diagnostic (`.base`, `.canvas`, Mermaid) uses (`web-rendering.md` §2.5,
/// §3, §4).
#[must_use]
pub fn render_diagnostic_panel(diagnostics: &[Diagnostic]) -> String {
    DiagnosticPanelTemplate { diagnostics }
        .render()
        .unwrap_or_else(|_| fallback_panel(diagnostics))
}

/// A fixed, compile-time-checked template rendering only string
/// interpolation cannot fail in practice; this exists so the function
/// above stays infallible without an `unwrap` in a production path
/// (`rust-quality.md`).
fn fallback_panel(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::from("<div class=\"diagnostic-panel\">");
    for d in diagnostics {
        // `write!` into a `String` cannot fail; only a `fmt::Write`
        // implementation that can error (not `String`'s) would return
        // `Err`, so this is not swallowing a real failure.
        let _ = write!(
            out,
            "<div class=\"diagnostic\"><span class=\"diagnostic-code\">{}</span><p class=\"diagnostic-message\">{}</p></div>",
            crate::rendering::escape_html(&d.code),
            crate::rendering::escape_html(&d.message)
        );
    }
    out.push_str("</div>");
    out
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod tests;
