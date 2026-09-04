//! The `/static/`-served JS client library wraps
//! `POST /mcp/{server_name}/{tool_name}` as `callTool(serverName, toolName,
//! args)` returning a Promise, with no LLM step between an app's own
//! JavaScript and the tool result.
//!
//! Verified by actually fetching the shipped script over HTTP from
//! `/static/` and running it under Node against a real, live
//! `contextos-web` router (itself backed by a real `contextos-mcp` stdio
//! session), not by inspecting the script's source text or loading it from
//! disk directly: this is the only way to prove the documented `callTool`
//! contract is genuinely callable JavaScript served the way it is
//! specified, not merely plausible-looking code sitting next
//! to the crate. Skipped, not failed, when no `node` binary is available
//! (this crate has no other reason to depend on a JavaScript runtime),
//! matching this project's existing precedent for environment-conditional
//! tests.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn node_is_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

async fn spawn_server(
    config_dir: &std::path::Path,
    vault_dir: &std::path::Path,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(contextos_web::mcp_client::McpClientSet::connect(&[entry]).await?);
    // No `static_dir` configured: `contextos-web-client.js` is served from
    // the crate's own embedded `static/` assets (FR-250), matching what a
    // zero-configuration install actually serves.
    let router = contextos_web::build_router(clients, None, &config_dir.join("web.toml"), "contextos".to_owned());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((addr, handle))
}

fn run_node_script(script: &str) -> Result<std::process::Output, BoxError> {
    Ok(std::process::Command::new("node").arg("-e").arg(script).output()?)
}

/// A Node script preamble common to both tests: fetches
/// `contextos-web-client.js` over HTTP from the real running server's own
/// `/static/` route, evaluates it (attaching `callTool` to `globalThis`,
/// exactly as a `<script>` tag would in a browser with no `window`), then
/// leaves `callTool` ready to invoke against `baseUrl`.
fn load_client_preamble(addr: SocketAddr) -> String {
    format!(
        "const baseUrl = 'http://{addr}';\n\
         fetch(baseUrl + '/static/contextos-web-client.js')\n\
         .then((response) => response.text())\n\
         .then((source) => {{\n\
         \x20\x20(0, eval)(source);\n\
         \x20\x20return globalThis.contextosWeb.callTool;\n\
         }})\n"
    )
}

#[tokio::test]
async fn call_tool_resolves_with_the_real_tool_result() -> Result<(), BoxError> {
    if !node_is_available() {
        eprintln!("skipping: no `node` binary on PATH");
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let (addr, _server) = spawn_server(dir.path(), &vault).await?;

    let script = format!(
        "{preamble}\
         .then((callTool) => callTool('contextos', 'vault_info', {{}}, {{ baseUrl }}))\n\
         .then((result) => {{ process.stdout.write(JSON.stringify(result)); }})\n\
         .catch((error) => {{ process.stderr.write(String(error)); process.exit(1); }});\n",
        preamble = load_client_preamble(addr)
    );
    // `Command::output()` blocks its thread; on the current-thread runtime
    // `#[tokio::test]` uses by default, that would starve the spawned
    // `axum::serve` task of any chance to accept Node's connection, hanging
    // both sides indefinitely. `spawn_blocking` keeps the server task
    // running while this call blocks a dedicated thread instead.
    let output = tokio::task::spawn_blocking(move || run_node_script(&script)).await??;

    assert!(
        output.status.success(),
        "node script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["structuredContent"]["vaults"][0]["name"], "contract-fixture");
    Ok(())
}

#[tokio::test]
async fn call_tool_rejects_on_an_unconfigured_server() -> Result<(), BoxError> {
    if !node_is_available() {
        eprintln!("skipping: no `node` binary on PATH");
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let (addr, _server) = spawn_server(dir.path(), &vault).await?;

    let script = format!(
        "{preamble}\
         .then((callTool) => callTool('does-not-exist', 'vault_info', {{}}, {{ baseUrl }}))\n\
         .then(() => {{ process.stderr.write('expected a rejection'); process.exit(1); }})\n\
         .catch((error) => {{\n\
         \x20\x20process.stdout.write(JSON.stringify({{ status: error.status, body: error.body }}));\n\
         }});\n",
        preamble = load_client_preamble(addr)
    );
    // `Command::output()` blocks its thread; on the current-thread runtime
    // `#[tokio::test]` uses by default, that would starve the spawned
    // `axum::serve` task of any chance to accept Node's connection, hanging
    // both sides indefinitely. `spawn_blocking` keeps the server task
    // running while this call blocks a dedicated thread instead.
    let output = tokio::task::spawn_blocking(move || run_node_script(&script)).await??;

    assert!(
        output.status.success(),
        "node script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["status"], 404);
    assert_eq!(result["body"]["error"], "mcp/server-not-configured");
    Ok(())
}
