use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::service;

#[tokio::test]
async fn serves_a_file_that_exists_under_the_static_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("hello.txt"), b"hello")?;
    let router = Router::new().nest_service("/static", service(dir.path()));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/hello.txt")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_traversal_attempt_never_escapes_the_static_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let static_dir = root.path().join("static");
    std::fs::create_dir_all(&static_dir)?;
    std::fs::write(
        root.path().join("secret.txt"),
        b"outside the static directory",
    )?;
    let router = Router::new().nest_service("/static", service(&static_dir));

    let response = router
        .oneshot(
            Request::builder()
                .uri("/static/../secret.txt")
                .body(Body::empty())?,
        )
        .await?;

    assert_ne!(response.status(), StatusCode::OK);
    Ok(())
}
