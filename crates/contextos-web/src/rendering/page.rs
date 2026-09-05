//! The full-page shell every route wraps its rendered fragment into for a
//! plain browser navigation (no `HX-Request` header,
//! `standards/http-routing-response-contract-standard.md`): a header (logo
//! and the configured `system_name` title), nav shell (vault switcher,
//! primary nav, current-directory listing), breadcrumb, and content area,
//! adapted from `outbox/2026-09-04-contextos-web-mock.html` (descriptive,
//! not prescriptive, per `delivery-plan.md`'s Phase 14 note).
//!
//! This module is a pure template renderer: it knows nothing about MCP. Its
//! caller ([`crate::rendering::shell`]) fetches the live data ([`NavData`])
//! this template needs and hands it in already assembled, keeping the
//! async/MCP concern and the template-rendering concern separate, matching
//! this crate's established pattern ([`crate::rendering::canvas`] takes
//! already-fetched node/edge data the identical way).

use askama::Template;

pub use crate::config::Appearance;

/// One configured vault, as offered in the nav shell's vault switcher.
pub struct NavVault {
    pub name: String,
    pub is_current: bool,
}

/// One immediate child of the directory the current page's nav tree section
/// is scoped to (`shell::build_nav`'s own "current directory only, not a
/// recursive whole-vault walk" scoping decision).
pub struct NavEntry {
    pub name: String,
    pub href: String,
    pub is_dir: bool,
}

/// One segment of a clickable ancestor trail: the top bar's own breadcrumb
/// and the nav tree section's own directory heading
/// ([`NavData::breadcrumb`], [`NavData::directory_breadcrumb`]) are both
/// built from this, via [`crate::rendering::shell::breadcrumb_segments`].
/// `href` is `Some` for every ancestor (ordered from the vault root down)
/// and `None` for exactly the last segment, the current page or directory
/// itself: never a link back to where the reader already is.
pub struct BreadcrumbSegment {
    pub label: String,
    pub href: Option<String>,
}

/// Everything the nav shell needs to render around one page's own content,
/// assembled by [`crate::rendering::shell::build_nav`].
pub struct NavData {
    pub vaults: Vec<NavVault>,
    /// `None` for a vault-independent page (`/settings/`); the vault
    /// switcher still lists every configured vault, but no tree section or
    /// vault-scoped breadcrumb segment is rendered.
    pub current_vault: Option<String>,
    /// The vault the "Vault browser" and "Apps" primary-nav links target:
    /// `current_vault` itself when there is one, otherwise the first
    /// configured vault, so those two links stay clickable from a vault-
    /// independent page like `/settings/` instead of degrading to inert
    /// text merely because the current page has no vault of its own.
    /// `None` only when no vault is configured at all (or none could be
    /// listed), the one case those links have nothing to point at.
    pub nav_target_vault: Option<String>,
    /// The nav tree section's own heading, as a clickable ancestor trail
    /// for the directory it lists (vault root first, that directory itself
    /// last and unlinked); `None` when there is no current-vault context
    /// at all.
    pub directory_breadcrumb: Option<Vec<BreadcrumbSegment>>,
    pub entries: Vec<NavEntry>,
    /// The top bar's own clickable ancestor trail for the page currently
    /// showing in `shell-main`: vault root first, the page itself
    /// (unlinked) last. A single unlinked `"settings"` segment on a
    /// vault-independent page.
    pub breadcrumb: Vec<BreadcrumbSegment>,
    pub active_vault_screen: bool,
    pub active_apps_screen: bool,
    pub active_settings_screen: bool,
    /// Vault-scoped `POST .../apps/rescan` target for the shell footer's
    /// "Rescan apps" link; `None` for a vault-independent page.
    pub rescan_href: Option<String>,
    /// `web.toml`'s `[server.ui]` theme/font/size/`system_name` (the
    /// `/settings/` Appearance pane): `theme`/`font`/`size` are applied as
    /// `<html data-theme="..." data-font="..." data-size="...">` so
    /// `contextos-web.css`'s corresponding attribute selectors take effect
    /// on the very next page render after a save; `system_name` is the
    /// header's own title, next to the logo. Each of `theme`/`font`/`size`
    /// absent (the key unset, non-string, or `web.toml` unreadable) falls
    /// back to the built-in default (system colour scheme, the default
    /// sans-serif stack, the default text size); `system_name` falls back to
    /// [`crate::config::DEFAULT_SYSTEM_NAME`]. Never a broken or
    /// half-applied appearance.
    pub appearance: Appearance,
}

#[derive(Template)]
#[template(path = "page_shell.html")]
struct PageShellTemplate<'a> {
    title: &'a str,
    body: &'a str,
    nav: &'a NavData,
}

/// Wraps `body` (an already-rendered content fragment) in the full-page
/// shell, titled `title`, chromed with `nav`.
#[must_use]
pub fn render_page(nav: &NavData, title: &str, body: &str) -> String {
    PageShellTemplate { title, body, nav }
        .render()
        .unwrap_or_else(|_| body.to_owned())
}

#[cfg(test)]
#[path = "page_test.rs"]
mod tests;
