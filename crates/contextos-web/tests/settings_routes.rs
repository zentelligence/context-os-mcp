//! `/settings/` contract tests (Phase 17, `delivery-plan.md`'s gate): real
//! `contextos-mcp` sessions against real temporary vault fixtures,
//! exercising the one gate item that genuinely needs a live MCP session
//! rather than a unit test double: the registered-app-dependency check
//! that blocks removing an `[[mcp_server]]` entry a registered app's
//! manifest still names. No mocking, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// `web.toml` naming two real, independently connectable `contextos-mcp`
/// stdio sessions against the identical `config_path`: `contextos` (this
/// crate's own convention: the first entry is always the local
/// `contextos-mcp` instance) and `extra`, standing in for a second server a
/// registered app's manifest might depend on.
fn write_two_server_web_toml(
    dir: &Path,
    contextos_command: &str,
    config_path: &Path,
) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join("web.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let config_path_value = format!("{config_path:?}");
    std::fs::write(
        &path,
        format!(
            "[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\ncommand = {contextos_command:?}\nargs = [\"--config\", {config_path_value}, \"--stdio\"]\n\n\
             [[mcp_server]]\nname = \"extra\"\ntransport = \"stdio\"\ncommand = {contextos_command:?}\nargs = [\"--config\", {config_path_value}, \"--stdio\"]\n"
        ),
    )?;
    Ok(path)
}

async fn router_with_two_servers(vault_dir: &Path, dir: &Path) -> Result<(Router, std::path::PathBuf), BoxError> {
    let config_path = support::write_vault_config(dir, vault_dir)?;
    let contextos_binary = support::contextos_mcp_binary()?;
    let web_config_path = write_two_server_web_toml(dir, &contextos_binary.to_string_lossy(), &config_path)?;
    let contextos_entry = support::real_contextos_entry("contextos", &config_path)?;
    let extra_entry = contextos_web::McpServerConfig::Stdio {
        name: "extra".to_owned(),
        command: contextos_binary.to_string_lossy().into_owned(),
        args: vec!["--config".to_owned(), config_path.to_string_lossy().into_owned()],
    };
    let clients = Arc::new(McpClientSet::connect(&[contextos_entry, extra_entry]).await?);
    let router = contextos_web::build_router(clients, Some(dir), &web_config_path, "contextos".to_owned());
    Ok((router, web_config_path))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn write_app_depending_on_extra(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        "registry/apps/needs-extra/manifest.toml",
        r#"
            name = "Needs Extra"
            kind = "spa"
            entry = "index.html"
            target = "_blank"
            mcp_servers = ["extra"]
        "#,
    )?;
    write(vault_dir, "registry/apps/needs-extra/index.html", "<html></html>")
}

async fn delete(router: &Router, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

async fn put(router: &Router, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

async fn patch(router: &Router, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// A single real, connectable `contextos-mcp` stdio session named
/// `"contextos"`, for tests that only need `/settings/` and a vault content
/// route, not the two-server dependency-check scenario.
async fn router_with_one_server(vault_dir: &Path, dir: &Path) -> Result<(Router, std::path::PathBuf), BoxError> {
    let config_path = support::write_vault_config(dir, vault_dir)?;
    let contextos_binary = support::contextos_mcp_binary()?;
    let web_config_path = dir.join("web.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let config_path_value = format!("{config_path:?}");
    let command_value = contextos_binary.to_string_lossy();
    std::fs::write(
        &web_config_path,
        format!(
            "[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\ncommand = {command_value:?}\nargs = [\"--config\", {config_path_value}, \"--stdio\"]\n"
        ),
    )?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(clients, Some(dir), &web_config_path, "contextos".to_owned());
    Ok((router, web_config_path))
}

#[tokio::test]
async fn removing_an_mcp_server_a_registered_apps_manifest_still_names_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app_depending_on_extra(vault_dir.path())?;
    let (router, web_config_path) = router_with_two_servers(vault_dir.path(), dir.path()).await?;
    let before = std::fs::read_to_string(&web_config_path)?;

    let (status, body) = delete(&router, r#"{"name":"extra"}"#).await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("settings/mcp-server-in-use"), "{body}");
    assert!(body.contains("needs-extra"), "{body}");
    assert!(body.contains(VAULT_NAME), "{body}");
    assert_eq!(
        std::fs::read_to_string(&web_config_path)?,
        before,
        "web.toml must be left byte-for-byte unchanged on a rejected removal"
    );
    Ok(())
}

#[tokio::test]
async fn removing_an_mcp_server_no_app_depends_on_succeeds() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // No app registered at all: nothing depends on "extra".
    let (router, web_config_path) = router_with_two_servers(vault_dir.path(), dir.path()).await?;

    let (status, body) = delete(&router, r#"{"name":"extra"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("extra"), "{body}");
    let rendered = std::fs::read_to_string(&web_config_path)?;
    assert!(!rendered.contains("name = \"extra\""));
    Ok(())
}

#[tokio::test]
async fn removing_an_mcp_server_an_app_depends_on_through_a_different_vault_is_still_rejected() -> Result<(), BoxError>
{
    let vault_a = tempfile::tempdir()?;
    let vault_b = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_a.path(), "index.md", "# Vault A\n")?;
    write(vault_b.path(), "index.md", "# Vault B\n")?;
    write_app_depending_on_extra(vault_b.path())?;

    let config_path = dir.path().join("config.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let content = format!(
        "[[vault]]\npath = {:?}\nname = \"vault-a\"\n[vault.search]\ntext = false\ngraph = false\n\n\
         [[vault]]\npath = {:?}\nname = \"vault-b\"\n[vault.search]\ntext = false\ngraph = false\n",
        vault_a.path(),
        vault_b.path()
    );
    std::fs::write(&config_path, content)?;
    let contextos_binary = support::contextos_mcp_binary()?;
    let web_config_path = write_two_server_web_toml(dir.path(), &contextos_binary.to_string_lossy(), &config_path)?;
    let contextos_entry = support::real_contextos_entry("contextos", &config_path)?;
    let extra_entry = contextos_web::McpServerConfig::Stdio {
        name: "extra".to_owned(),
        command: contextos_binary.to_string_lossy().into_owned(),
        args: vec!["--config".to_owned(), config_path.to_string_lossy().into_owned()],
    };
    let clients = Arc::new(McpClientSet::connect(&[contextos_entry, extra_entry]).await?);
    let router = contextos_web::build_router(clients, Some(dir.path()), &web_config_path, "contextos".to_owned());

    let (status, body) = delete(&router, r#"{"name":"extra"}"#).await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("vault-b"), "{body}");
    Ok(())
}

#[tokio::test]
async fn put_renaming_an_entry_with_no_dependent_apps_succeeds() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, web_config_path) = router_with_two_servers(vault_dir.path(), dir.path()).await?;
    let contextos_binary = support::contextos_mcp_binary()?;

    let (status, body) = put(
        &router,
        &format!(
            r#"{{"current_name":"extra","transport":"stdio","name":"renamed","command":{:?},"args":[]}}"#,
            contextos_binary.to_string_lossy()
        ),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("renamed"));
    let rendered = std::fs::read_to_string(&web_config_path)?;
    assert!(rendered.contains("name = \"renamed\""));
    assert!(!rendered.contains("name = \"extra\""));
    Ok(())
}

#[tokio::test]
async fn put_renaming_an_entry_a_registered_app_depends_on_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app_depending_on_extra(vault_dir.path())?;
    let (router, web_config_path) = router_with_two_servers(vault_dir.path(), dir.path()).await?;
    let before = std::fs::read_to_string(&web_config_path)?;
    let contextos_binary = support::contextos_mcp_binary()?;

    let (status, body) = put(
        &router,
        &format!(
            r#"{{"current_name":"extra","transport":"stdio","name":"renamed","command":{:?},"args":[]}}"#,
            contextos_binary.to_string_lossy()
        ),
    )
    .await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("settings/mcp-server-in-use"), "{body}");
    assert_eq!(std::fs::read_to_string(&web_config_path)?, before);
    Ok(())
}

/// End-to-end proof that saving an appearance value through `/settings/`
/// actually changes what a subsequent page render looks like, not just
/// what `web.toml` holds: a `PATCH ... target: "ui"` setting `theme` is
/// reflected in `<html data-theme="...">` on the *next* full-page render of
/// an unrelated vault content route, without a server restart (`current_
/// appearance` reads `web.toml` fresh per request, `config.rs`).
#[tokio::test]
async fn an_appearance_save_takes_effect_on_the_next_vault_page_render() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path) = router_with_one_server(vault_dir.path(), dir.path()).await?;

    let (status, before_body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{before_body}");
    assert!(!before_body.contains("data-theme"), "{before_body}");

    let (status, patch_body) = patch(
        &router,
        r#"{"target":"ui","patch":{"theme":"dark","font":"serif","size":"large"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{patch_body}");

    let (status, after_body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{after_body}");
    assert!(after_body.contains("<html lang=\"en\" data-theme=\"dark\" data-font=\"serif\" data-size=\"large\">"));
    Ok(())
}

#[tokio::test]
async fn settings_renders_the_same_with_or_without_a_trailing_slash() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path) = router_with_one_server(vault_dir.path(), dir.path()).await?;

    let (status_slash, body_slash) = get(&router, "/settings/").await?;
    let (status_bare, body_bare) = get(&router, "/settings").await?;
    assert_eq!(status_slash, StatusCode::OK);
    assert_eq!(status_bare, StatusCode::OK);
    assert_eq!(body_slash, body_bare);
    Ok(())
}

#[tokio::test]
async fn settings_keeps_the_vault_browser_and_apps_nav_links_clickable() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path) = router_with_one_server(vault_dir.path(), dir.path()).await?;

    let (status, body) = get(&router, "/settings/").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!("<a href=\"/{VAULT_NAME}/\"")));
    assert!(body.contains(&format!("<a href=\"/{VAULT_NAME}/apps/\"")));
    assert!(!body.contains("<span class=\"nav-dir\">"));
    Ok(())
}
