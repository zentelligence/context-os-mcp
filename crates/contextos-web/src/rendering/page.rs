//! The minimal full-page wrapper every vault content route (`FR-220` to
//! `FR-224`) composes its rendered fragment into for a plain browser
//! navigation (no `HX-Request` header, `standards/http-routing-response-
//! contract-standard.md`). Nav chrome, vault browsing, and app/settings
//! surfaces (Phases 16 and 17) are not part of this phase's scope; this
//! wrapper is deliberately minimal so it does not stand in for that work.

use askama::Template;

#[derive(Template)]
#[template(path = "page_shell.html")]
struct PageShellTemplate<'a> {
    title: &'a str,
    body: &'a str,
}

/// Wraps `body` (an already-rendered content fragment) in the full-page
/// shell, titled `title`.
#[must_use]
pub fn render_page(title: &str, body: &str) -> String {
    PageShellTemplate { title, body }
        .render()
        .unwrap_or_else(|_| body.to_owned())
}

#[cfg(test)]
#[path = "page_test.rs"]
mod tests;
