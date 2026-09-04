use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::service;

fn bundled_static_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/static"))
}

#[tokio::test]
async fn serves_a_file_that_exists_under_a_configured_static_directory() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("hello.txt"), b"hello")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(Request::builder().uri("/static/hello.txt").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_traversal_attempt_never_escapes_a_configured_static_directory() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let static_dir = root.path().join("static");
    std::fs::create_dir_all(&static_dir)?;
    std::fs::write(root.path().join("secret.txt"), b"outside the static directory")?;
    let router = Router::new().nest_service("/static", service(Some(&static_dir)));

    let response = router
        .oneshot(Request::builder().uri("/static/../secret.txt").body(Body::empty())?)
        .await?;

    assert_ne!(response.status(), StatusCode::OK);
    Ok(())
}

/// `FR-250`: the crate's own bundled assets ship embedded in the binary, so
/// they are servable with no `static_dir` configured at all, matching the
/// v0.20.2 Windows install failure this behaviour fixes (a `cargo install`
/// layout never carries a `static/` directory next to the binary).
#[tokio::test]
async fn serves_a_bundled_asset_with_no_static_dir_configured() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let expected = std::fs::read(bundled_static_dir().join("contextos-web-client.js"))?;
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), expected.as_slice());
    Ok(())
}

/// A bundled asset carries a content-hash `ETag`, and a matching
/// `If-None-Match` on a later request gets a bodyless `304`, not a full
/// re-download: the same cache-revalidation contract a browser gets from
/// `ServeDir` for a configured `static_dir`.
#[tokio::test]
async fn a_bundled_asset_revalidates_via_etag() -> Result<(), Box<dyn std::error::Error>> {
    let router = Router::new().nest_service("/static", service(None));

    let first = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(axum::http::header::ETAG)
        .ok_or("expected an ETag header on the first response")?
        .clone();

    let second = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web-client.js")
                .header(axum::http::header::IF_NONE_MATCH, etag)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    let body = to_bytes(second.into_body(), usize::MAX).await?;
    assert!(body.is_empty());
    Ok(())
}

/// A path neither a configured `static_dir` nor the bundled set contains
/// is a plain `404`, not a panic or an empty-body success.
#[tokio::test]
async fn a_path_absent_from_both_the_static_dir_and_the_bundled_set_is_not_found()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/does-not-exist.txt")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// A configured `static_dir` is consulted first: an operator can override a
/// bundled asset (a custom `contextos-web.css`, say) without rebuilding.
#[tokio::test]
async fn a_configured_static_dir_overrides_a_bundled_asset_of_the_same_name() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("contextos-web.css"), b"body { color: red; }")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/contextos-web.css")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), b"body { color: red; }");
    Ok(())
}

/// A configured `static_dir` that does not itself carry a bundled asset
/// (`htmx.min.js`, say, an operator only overrode the crate's own CSS)
/// still serves that asset from the embedded fallback, not a `404`.
#[tokio::test]
async fn a_configured_static_dir_falls_back_to_a_bundled_asset_it_does_not_contain()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("contextos-web.css"), b"body { color: red; }")?;
    let router = Router::new().nest_service("/static", service(Some(dir.path())));

    let response = router
        .oneshot(Request::builder().uri("/static/htmx.min.js").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let expected = std::fs::read(bundled_static_dir().join("htmx.min.js"))?;
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), expected.as_slice());
    Ok(())
}
