//! `GET /static/{{path}}` (FR-250): non-vault server assets only, never
//! vault content.

use std::path::Path;

use tower_http::services::ServeDir;

/// Builds the `/static/` service rooted at `static_dir`.
///
/// Traversal protection is delegated to `tower-http`'s `ServeDir`, an
/// already-audited implementation, rather than a second hand-rolled
/// path-escape check in this workspace (`NFR-01`'s precedent: don't
/// re-implement a security-relevant check when a well-tested one already
/// exists at the boundary being crossed).
#[must_use]
pub fn service(static_dir: &Path) -> ServeDir {
    ServeDir::new(static_dir)
}

#[cfg(test)]
#[path = "static_assets_test.rs"]
mod tests;
