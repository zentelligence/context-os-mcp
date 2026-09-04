//! Golden-file determinism suite (Phase 15 gate, `web-rendering.md` §5):
//! one fixture per stage (wikilink, embed, triple-colon fence, callout,
//! `.base` view, `.canvas`, Mermaid diagram) renders twice per run,
//! asserting both run-to-run identity and a match against a checked-in
//! golden file.
//!
//! Set `UPDATE_GOLDEN=1` to (re)write the golden files from the current
//! renderer output, the standard bootstrap/refresh path for this kind of
//! suite: a missing golden file without that flag is a test failure, not a
//! silent pass, so an accidentally deleted golden is caught by CI.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

fn fixtures_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/rendering");
    path
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        config_dir,
        &config_dir.join("web.toml"),
        "contextos".to_owned(),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Asserts `actual` is both stable (the caller renders twice and compares
/// before calling this) and matches the checked-in golden file at
/// `fixtures/rendering/{{name}}.golden.html` (or `.svg`), bootstrapping it
/// under `UPDATE_GOLDEN=1`.
fn assert_matches_golden(name: &str, extension: &str, actual: &str) -> Result<(), BoxError> {
    let golden_path = fixtures_dir().join(format!("{name}.golden.{extension}"));
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&golden_path, actual)?;
        return Ok(());
    }
    let expected = std::fs::read_to_string(&golden_path).map_err(|source| {
        format!(
            "missing golden file {}: run with UPDATE_GOLDEN=1 to create it ({source})",
            golden_path.display()
        )
    })?;
    if expected != actual {
        return Err(format!(
            "rendered output for {name} no longer matches its golden file \
             ({}); if this is an intentional rendering change, rerun with \
             UPDATE_GOLDEN=1 to refresh it",
            golden_path.display()
        )
        .into());
    }
    Ok(())
}

async fn render_twice_and_check_golden(
    router: &Router,
    route: &str,
    name: &str,
    extension: &str,
) -> Result<(), BoxError> {
    let (status, first) = get(router, route).await?;
    assert_eq!(status, StatusCode::OK, "route {route}");
    let (status, second) = get(router, route).await?;
    assert_eq!(status, StatusCode::OK, "route {route}");
    assert_eq!(first, second, "non-deterministic render for {name}");
    assert_matches_golden(name, extension, &first)
}

#[tokio::test]
// A straight-line sequence of fixture writes and one golden-comparison
// call per content type (seven kinds, `web-rendering.md` §5); splitting it
// into helper functions would not reduce complexity, only indirection.
#[allow(clippy::too_many_lines)]
async fn golden_file_determinism_suite() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;

    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "target-note.md",
        "# Target note\n\nResolved target content.\n",
    )?;
    write(
        vault_dir.path(),
        "embed-target.md",
        "# Embed target\n\nEmbedded content body.\n",
    )?;
    write(
        vault_dir.path(),
        "wikilink-fixture.md",
        "# Wikilink fixture\n\nA [[target-note]] link.\n",
    )?;
    write(
        vault_dir.path(),
        "embed-fixture.md",
        "# Embed fixture\n\n![[embed-target]]\n",
    )?;
    write(
        vault_dir.path(),
        "fence-fixture.md",
        "# Fence fixture\n\n:::warning\nA recognised fence body.\n:::\n",
    )?;
    write(
        vault_dir.path(),
        "callout-fixture.md",
        "# Callout fixture\n\n> [!tip] Handy\n> A callout body.\n",
    )?;
    write(
        vault_dir.path(),
        "base-fixture.base",
        "views:\n  - type: table\n    name: \"All\"\n    order:\n      - file.path\n      - status\n",
    )?;
    write(
        vault_dir.path(),
        "base-row.md",
        "---\nstatus: active\n---\n# Base row\n",
    )?;
    write(
        vault_dir.path(),
        "canvas-fixture.canvas",
        "{\"nodes\":[{\"id\":\"n1\",\"type\":\"text\",\"x\":0,\"y\":0,\"width\":160,\"height\":60,\"text\":\"Node\"}],\"edges\":[]}\n",
    )?;
    write(
        vault_dir.path(),
        "mermaid-fixture.mermaid",
        "graph TD\n  A --> B\n",
    )?;

    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/wikilink-fixture.md"),
        "wikilink",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/embed-fixture.md"),
        "embed",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/fence-fixture.md"),
        "fence",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/callout-fixture.md"),
        "callout",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/base-fixture.base"),
        "base",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/canvas-fixture.canvas"),
        "canvas",
        "html",
    )
    .await?;
    render_twice_and_check_golden(
        &router,
        &format!("/{VAULT_NAME}/mermaid-fixture.mermaid"),
        "mermaid",
        "html",
    )
    .await?;

    Ok(())
}
