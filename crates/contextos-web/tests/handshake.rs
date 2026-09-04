//! FR-204: `contextos-web` refuses to serve any HTTP request until every
//! configured `[[mcp_server]]`'s `initialize` handshake has completed; a
//! deliberately misconfigured entry is a startup error, never a
//! request-time surprise.

mod support;

use contextos_web::config::McpServerConfig;
use contextos_web::mcp_client::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn a_misconfigured_command_fails_the_whole_connect_call_before_any_request_is_possible()
-> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let good = support::real_contextos_entry("contextos", &config_path)?;
    let bad = McpServerConfig::Stdio {
        name: "broken".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: Vec::new(),
    };

    // The good entry is listed first: if `connect` degraded to
    // "connect what you can, report the rest", this would be the case most
    // likely to hide it (a partially-populated, seemingly-usable set).
    let result = McpClientSet::connect(&[good, bad]).await;

    assert!(
        result.is_err(),
        "a misconfigured entry must fail the whole connect call, not just be reported \
         alongside a partially-live set"
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_http_endpoint_fails_the_connect_call() -> Result<(), BoxError> {
    // Bind an ephemeral loopback port, then drop the listener before
    // connecting: the OS refuses the next connection to that exact port
    // immediately (a real, deterministic "nothing is listening here"),
    // unlike a fixed low port number, which some sandboxed network
    // environments silently drop instead of refusing, turning the
    // connection attempt into an OS-level timeout of a minute or more
    // rather than the fast failure this test exists to prove.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let entry = McpServerConfig::Http {
        name: "unreachable".to_owned(),
        endpoint: format!("http://127.0.0.1:{port}"),
        token_env: None,
    };

    let Ok(result) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        McpClientSet::connect(&[entry]),
    )
    .await
    else {
        return Err("an unreachable endpoint must fail fast, not hang the whole startup".into());
    };

    assert!(result.is_err());
    Ok(())
}
