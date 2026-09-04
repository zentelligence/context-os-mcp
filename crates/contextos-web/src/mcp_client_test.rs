use super::*;

#[tokio::test]
async fn connecting_to_a_nonexistent_command_fails_to_spawn() {
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: Vec::new(),
    };

    let result = McpClient::connect(&entry).await;

    assert!(matches!(result, Err(McpConnectError::Spawn { .. })));
}

#[cfg(unix)]
#[tokio::test]
async fn a_process_that_exits_without_ever_completing_the_handshake_is_a_startup_error()
-> Result<(), Box<dyn std::error::Error>> {
    // The process exits immediately without writing a response: the
    // transport observes a closed stream while still waiting for the
    // `initialize` response, so the handshake fails fast rather than
    // hanging, exercising FR-204's "startup error, not a lazily-discovered
    // one" without needing a real MCP server binary. Bounded by an explicit
    // timeout as a safety net, not as the thing under test: the assertion
    // on the inner `Result` is what proves this fails fast, not the bound.
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 0".to_owned()],
    };

    let Ok(result) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClient::connect(&entry),
    )
    .await
    else {
        return Err("connect() must fail fast, not hang, once the child process exits".into());
    };

    assert!(matches!(result, Err(McpConnectError::Handshake { .. })));
    Ok(())
}

#[tokio::test]
async fn an_http_entry_with_an_unset_token_env_fails_before_connecting() {
    // Deliberately not cleared via `std::env::remove_var`: this workspace
    // forbids `unsafe_code`, and mutating process environment variables is
    // `unsafe` from the 2024 edition on. A name this specific is not
    // expected to be set in any real environment this test runs in.
    let variable = "CONTEXTOS_WEB_TEST_UNSET_TOKEN_9F3C";
    assert!(
        std::env::var(variable).is_err(),
        "test precondition: {variable} must not be set in this environment"
    );
    let entry = McpServerConfig::Http {
        name: "some-other-server".to_owned(),
        endpoint: "http://127.0.0.1:1".to_owned(),
        token_env: Some(variable.to_owned()),
    };

    let result = McpClient::connect(&entry).await;

    assert!(matches!(
        result,
        Err(McpConnectError::MissingTokenEnv { .. })
    ));
}
