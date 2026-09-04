//! `GET /static/{path}`: non-vault server assets.
//!
//! The crate's own bundled assets (`static/`: the UI shell's CSS, the
//! app-manifest JS client library, HTMX) are embedded into the binary at
//! compile time ([`BundledAssets`]), so they are servable with no
//! configuration at all, on every platform and every install layout: a
//! `cargo install` into a `PATH` directory carries no sibling `static/`
//! folder, and the process's current working directory at launch has no
//! bearing on whether `/static/` works (FR-250).
//!
//! An operator-configured `static_dir` (`web.toml`'s `[server] static_dir`,
//! optional) is consulted first when present, so an operator can override
//! or add to the bundled set without rebuilding; any path it does not
//! itself contain falls through to the embedded copy.

use std::convert::Infallible;
use std::path::Path;

use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use rust_embed::RustEmbed;
use tower::ServiceExt as _;
use tower::util::BoxCloneSyncService;
use tower_http::services::ServeDir;

/// The lowercase-hex SHA-256 content hash `rust-embed` computes for every
/// bundled file, reused verbatim as that file's `ETag`: cheap to compute
/// once at compile time, content-addressed (changes if and only if the
/// file's bytes do), and already exactly what `If-None-Match` comparison
/// needs, so no separate cache-validation scheme is invented here.
fn etag_for(hash: [u8; 32]) -> HeaderValue {
    use std::fmt::Write as _;
    let mut quoted = String::with_capacity(hash.len() * 2 + 2);
    quoted.push('"');
    for byte in hash {
        // A `write!` into a `String` is infallible; `fmt::Error` can only
        // originate from the `Write` implementation itself, which
        // `String`'s never does.
        let _ = write!(quoted, "{byte:02x}");
    }
    quoted.push('"');
    HeaderValue::from_str(&quoted).unwrap_or(HeaderValue::from_static("\"bundled\""))
}

/// The crate's own `static/` directory, embedded into the binary.
#[derive(RustEmbed)]
#[folder = "static/"]
struct BundledAssets;

type BoxedAssetService = BoxCloneSyncService<Request<Body>, Response<Body>, Infallible>;

/// Builds the `/static/` service. `static_dir`, when present, is tried
/// first; the embedded [`BundledAssets`] serve every path it does not
/// contain (or every path, when `static_dir` is `None`).
///
/// Traversal protection for a configured `static_dir` is delegated to
/// `tower-http`'s `ServeDir`, an already-audited implementation, rather
/// than a second hand-rolled path-escape check in this workspace. The
/// embedded fallback carries no equivalent risk to guard: `RustEmbed::get`
/// is an exact-key lookup against a compiled-in path set, not a filesystem
/// resolution, so a `..` segment simply fails to match any embedded key
/// rather than escaping anywhere.
#[must_use]
pub fn service(static_dir: Option<&Path>) -> BoxedAssetService {
    match static_dir {
        Some(static_dir) => BoxCloneSyncService::new(
            ServeDir::new(static_dir)
                .fallback(embedded_service())
                .map_response(|response| response.map(Body::new)),
        ),
        None => embedded_service(),
    }
}

fn embedded_service() -> BoxedAssetService {
    BoxCloneSyncService::new(tower::service_fn(|request: Request<Body>| async move {
        let key = request.uri().path().trim_start_matches('/').to_owned();
        let if_none_match = request.headers().get(header::IF_NONE_MATCH).cloned();
        Ok::<_, Infallible>(serve_embedded(&key, if_none_match.as_ref()))
    }))
}

/// Serves `key` from [`BundledAssets`], honouring `If-None-Match` against
/// the bundled file's content-hash `ETag` with a bodyless `304` (the same
/// revalidation contract a browser gets from `ServeDir` for a configured
/// `static_dir`, so switching between the two sources of `/static/` never
/// costs an operator's cache behaviour).
fn serve_embedded(key: &str, if_none_match: Option<&HeaderValue>) -> Response<Body> {
    let Some(file) = BundledAssets::get(key) else {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_FOUND;
        return response;
    };
    let etag = etag_for(file.metadata.sha256_hash());
    if if_none_match.is_some_and(|value| value == etag) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().insert(header::ETAG, etag);
        return response;
    }
    let mut response = Response::new(Body::from(file.data.into_owned()));
    let mime_type =
        HeaderValue::from_str(file.metadata.mimetype()).unwrap_or(HeaderValue::from_static("application/octet-stream"));
    response.headers_mut().insert(header::CONTENT_TYPE, mime_type);
    response.headers_mut().insert(header::ETAG, etag);
    response
}

#[cfg(test)]
#[path = "static_assets_test.rs"]
mod tests;
