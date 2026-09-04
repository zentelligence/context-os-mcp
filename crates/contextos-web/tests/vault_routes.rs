//! Vault content route contract tests (Phase 15, `delivery-plan.md`'s
//! gate): real `contextos-mcp` sessions against real temporary vault
//! fixtures, exercising `GET`/`POST`/`PATCH`/`PUT`/`DELETE
//! /{{vault_name}}/{{relative-path}}` end to end. No mocking, per
//! `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        config_dir,
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

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

// ---------------------------------------------------------------------
// 404 (FR-225)
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_unconfigured_vault_name_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = get(&router, "/does-not-exist/some-note.md").await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_nonexistent_path_in_a_real_vault_is_a_404_with_no_listing_fallback()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/does-not-exist.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_ascii_lowercase().contains("index of"));
    Ok(())
}

// ---------------------------------------------------------------------
// Directory route (FR-224)
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_directory_route_renders_that_directorys_index_md() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Vault root\n\nWelcome.\n")?;
    write(
        vault_dir.path(),
        "notes/index.md",
        "# Notes directory\n\nSome operator prose.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Vault root"));
    assert!(body.contains("Welcome."));

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Notes directory"));
    assert!(body.contains("Some operator prose."));
    Ok(())
}

#[tokio::test]
async fn a_bare_path_resolving_to_a_directory_renders_the_same_as_its_trailing_slash_form()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "notes/index.md", "# Notes\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status_slash, body_slash) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    let (status_bare, body_bare) = get(&router, &format!("/{VAULT_NAME}/notes")).await?;
    assert_eq!(status_slash, StatusCode::OK);
    assert_eq!(status_bare, StatusCode::OK);
    assert_eq!(body_slash, body_bare);
    Ok(())
}

// ---------------------------------------------------------------------
// Markdown rendering: wikilinks, embeds, fences, callouts (FR-221, FR-240,
// FR-241)
// ---------------------------------------------------------------------

#[tokio::test]
async fn wikilink_resolution_covers_live_dead_and_doubly_nested_embed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "target-note.md",
        "# Target\n\nThis is the target note.\n",
    )?;
    write(
        vault_dir.path(),
        "third-file.md",
        "# Third\n\nThird-file content.\n",
    )?;
    write(
        vault_dir.path(),
        "embed-target.md",
        "# Embed target\n\n![[third-file]]\n",
    )?;
    write(
        vault_dir.path(),
        "example-note.md",
        "# Example\n\n\
         See [[target-note]] for a live link.\n\n\
         See [[does-not-exist]] for a dead link.\n\n\
         ![[embed-target]]\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/example-note.md")).await?;
    assert_eq!(status, StatusCode::OK);

    // Live link resolves to the target's own route.
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/target-note.md\"")));
    assert!(body.contains("class=\"wikilink\""));

    // Dead link renders as a visually distinct span, never a broken anchor.
    assert!(body.contains("wikilink dead"));
    assert!(body.contains("data-target=\"does-not-exist\""));

    // The embed inlines embed-target's own content...
    assert!(body.contains("Embed target"));
    // ...and the doubly-nested embed inside it (embed-target embeds
    // third-file) renders as a plain link at that second level, not a
    // second recursive inline.
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/third-file.md\"")));
    assert!(!body.contains("Third-file content."));
    Ok(())
}

#[tokio::test]
async fn triple_colon_fences_and_callouts_render_with_raw_source_available() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "note.md",
        "# Note\n\n\
         :::warning\nThis may break things.\n:::\n\n\
         :::mystery-fence\nUnrecognised body.\n:::\n\n\
         > [!tip] Handy hint\n> Do this instead.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/note.md")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("fence-recognised"));
    assert!(body.contains("This may break things."));
    assert!(body.contains("fence-unrecognised"));
    assert!(body.contains("mystery-fence"));
    assert!(body.contains("callout-tip"));
    assert!(body.contains("Handy hint"));

    let (status, raw) = get(&router, &format!("/{VAULT_NAME}/note.md?raw=1")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.contains(":::warning"));
    assert!(raw.contains("> [!tip]"));
    Ok(())
}

// ---------------------------------------------------------------------
// Mermaid rendering (FR-242)
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_valid_standalone_mermaid_file_renders_to_svg() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "diagram.mermaid", "graph TD\n  A --> B\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/diagram.mermaid")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<svg"));
    assert!(!body.contains("diagnostic-panel"));
    Ok(())
}

#[tokio::test]
async fn a_malformed_mermaid_diagram_renders_the_diagnostic_panel_not_a_server_error()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "bad.mermaid",
        "not valid mermaid syntax {{{\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/bad.mermaid")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("diagnostic-panel"));
    assert!(!body.contains("<svg"));
    Ok(())
}

// ---------------------------------------------------------------------
// Canvas rendering (FR-223, FR-243)
// ---------------------------------------------------------------------

const VALID_CANVAS: &str = r#"{
  "nodes": [
    {"id": "n1", "type": "text", "x": 0, "y": 0, "width": 200, "height": 60, "text": "Node One"},
    {"id": "n2", "type": "text", "x": 300, "y": 0, "width": 200, "height": 60, "text": "Node Two"}
  ],
  "edges": [
    {"id": "e1", "fromNode": "n1", "toNode": "n2"}
  ]
}"#;

#[tokio::test]
async fn a_valid_canvas_file_renders_to_svg_using_its_own_positions() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "diagram.canvas", VALID_CANVAS)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/diagram.canvas")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<svg"));
    assert!(body.contains("data-id=\"n1\""));
    assert!(body.contains("x=\"0\""));
    assert!(body.contains("x=\"300\""));
    Ok(())
}

#[tokio::test]
async fn a_malformed_canvas_file_renders_the_diagnostic_panel_not_a_server_error()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "bad.canvas", "{ this is not valid json")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/bad.canvas")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("diagnostic-panel"));
    Ok(())
}

#[tokio::test]
async fn canvas_mutation_methods_all_return_405_read_only_in_v1() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "diagram.canvas", VALID_CANVAS)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for method in ["POST", "PATCH", "PUT", "DELETE"] {
        let (status, _) = request(
            &router,
            method,
            &format!("/{VAULT_NAME}/diagram.canvas"),
            "{}",
        )
        .await?;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "method {method}");
    }
    // The file was never touched by the read-only route.
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("diagram.canvas"))?,
        VALID_CANVAS
    );
    Ok(())
}

// ---------------------------------------------------------------------
// `.base` rendering and CRUD round trip (FR-222, FR-225a)
// ---------------------------------------------------------------------

const TASKS_BASE: &str = "views:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n      - status\n";

#[tokio::test]
async fn a_base_file_renders_its_matched_rows_as_a_card_grid() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "tasks.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks.base")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("task-one.md"));
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/task-one.md\"")));
    assert!(body.contains("active"));
    Ok(())
}

#[tokio::test]
async fn editing_a_row_dispatches_frontmatter_update_never_touching_the_base_file()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "tasks.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"target": "row", "note_path": "task-one.md", "patch": {"status": "done"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // The row's own note was updated...
    let note = std::fs::read_to_string(vault_dir.path().join("task-one.md"))?;
    assert!(note.contains("status: done"));
    // ...and the `.base` file's own definition was never written (FR-222:
    // the two mutation targets are never conflated).
    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert_eq!(base_source, TASKS_BASE);
    Ok(())
}

#[tokio::test]
async fn editing_the_view_definition_dispatches_base_apply_never_touching_a_matched_note()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "tasks.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"target": "definition", "operations": [{"op": "add_formula", "name": "count_it", "expression": "1"}]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // The `.base` file's own definition changed...
    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert_ne!(base_source, TASKS_BASE);
    assert!(base_source.contains("count_it"));
    // ...and the matched note was never written.
    let note = std::fs::read_to_string(vault_dir.path().join("task-one.md"))?;
    assert!(note.contains("status: active"));
    Ok(())
}

#[tokio::test]
async fn deleting_from_a_base_route_removes_a_definition_entry_never_a_note() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let base_with_formula = "formulas:\n  count_it: '1'\nviews:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n";
    write(vault_dir.path(), "tasks.base", base_with_formula)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "DELETE",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"operations": [{"op": "remove_formula", "name": "count_it"}]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert!(!base_source.contains("count_it"));
    // The note file still exists, untouched.
    assert!(vault_dir.path().join("task-one.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// Mutation-method dispatch on `.md` and generic files (FR-225a)
// ---------------------------------------------------------------------

#[tokio::test]
async fn patching_a_notes_frontmatter_dispatches_frontmatter_update() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "note.md",
        "---\nstatus: pending\n---\n# Note\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/note.md"),
        r#"{"patch": {"status": "active"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let content = std::fs::read_to_string(vault_dir.path().join("note.md"))?;
    assert!(content.contains("status: active"));
    Ok(())
}

#[tokio::test]
async fn posting_to_a_note_route_has_no_defined_target_and_is_405() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "note.md", "# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(&router, "POST", &format!("/{VAULT_NAME}/note.md"), "{}").await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn deleting_any_file_dispatches_fs_delete_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "note.md", "# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(&router, "DELETE", &format!("/{VAULT_NAME}/note.md"), "").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!vault_dir.path().join("note.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// Determinism (FR-244)
// ---------------------------------------------------------------------

#[tokio::test]
async fn rendering_the_same_note_twice_produces_byte_identical_html() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "target-note.md", "# Target\n\nContent.\n")?;
    write(
        vault_dir.path(),
        "note.md",
        "# Note\n\n\
         [[target-note]] and [[missing-note]].\n\n\
         :::note\nBody.\n:::\n\n\
         > [!info] Info\n> Detail.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, first) = get(&router, &format!("/{VAULT_NAME}/note.md")).await?;
    let (_, second) = get(&router, &format!("/{VAULT_NAME}/note.md")).await?;
    assert_eq!(first, second);
    Ok(())
}
