//! `OpenAiCompatible` against a local, in-process HTTP stub.
//!
//! No test here touches a real network endpoint: `support::http_stub` is a
//! hand-rolled loopback server, and the "missing env var" test never starts
//! a stub at all, proving no request is sent.
//!
//! Every test that needs the API key's environment variable to actually be
//! present spawns this same test binary as a child process with that
//! variable set on the child only (`std::process::Command::env`), then
//! asserts the child's exit status. This mirrors
//! `contextos-fs/tests/mutate.rs`'s `..._child` convention and is required
//! here, not merely stylistic: `std::env::set_var`/`remove_var` are
//! `unsafe fn`, and this workspace forbids `unsafe_code` outright (see
//! `crates/contextos-search/src/lib.rs`), so no test in this crate may
//! mutate the current process's environment to fake a set variable.
mod support;

use std::process::Command;
use std::time::Duration;

use contextos_search::{Chunk, ChunkSource, EmbedsText, OpenAiCompatible, OpenAiCompatibleConfig, chunk_document};
use serde_json::Value;
use support::http_stub::{HttpStub, StubResponse};
use support::vault_note;

fn chunks_for(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<Vec<Chunk>, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(chunk_document(ChunkSource { path: &path, content }))
}

/// Re-runs `child_test_name` as a child process of this same test binary,
/// with `variable` set to `value` for that child only, and asserts it
/// passed. The child test itself no-ops when `variable` is absent, so it
/// also runs harmlessly under a normal `cargo test` invocation.
fn run_with_env_child(child_test_name: &str, variable: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", child_test_name])
        .env(variable, value)
        .status()?;
    if !status.success() {
        return Err(format!("child test {child_test_name} did not pass").into());
    }
    Ok(())
}

#[test]
fn missing_api_key_env_is_rejected_with_no_request_sent() -> Result<(), Box<dyn std::error::Error>> {
    let variable = "CONTEXTOS_TEST_MISSING_KEY_FR54";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} must not already be set"
    );

    let result = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: "http://127.0.0.1:1/v1/embeddings".to_owned(),
        model: "test-model".to_owned(),
        api_key_env: variable.to_owned(),
    });

    let Err(error) = result else {
        return Err("expected construction to fail when the api_key_env variable is unset".into());
    };
    assert_eq!(error.code(), "embedding/config");
    Ok(())
}

#[test]
fn plain_http_to_a_non_loopback_endpoint_is_rejected_with_no_request_sent() -> Result<(), Box<dyn std::error::Error>> {
    let variable = "CONTEXTOS_TEST_KEY_FR54_INSECURE";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} must not already be set"
    );

    let result = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: "http://embeddings.example.com/v1/embeddings".to_owned(),
        model: "test-model".to_owned(),
        api_key_env: variable.to_owned(),
    });

    let Err(error) = result else {
        return Err("expected construction to fail for a plain http:// endpoint that is not loopback".into());
    };
    assert_eq!(error.code(), "embedding/config");
    // The api_key_env variable is unset in this process, so if endpoint
    // rejection happened after the key lookup instead of before it, this
    // would fail with EmbeddingApiKeyMissing's message instead: asserting
    // the message pins which check ran first.
    assert!(error.to_string().contains("not https"));
    Ok(())
}

#[test]
fn https_endpoint_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let variable = "CONTEXTOS_TEST_KEY_FR54_HTTPS_MISSING";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} must not already be set"
    );

    let result = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: "https://embeddings.example.com/v1/embeddings".to_owned(),
        model: "test-model".to_owned(),
        api_key_env: variable.to_owned(),
    });

    // An https endpoint passes the scheme check, so construction fails on
    // the (still unset) api_key_env instead: proving https is accepted
    // rather than merely untested.
    let Err(error) = result else {
        return Err("expected construction to fail on the missing api_key_env".into());
    };
    assert!(error.to_string().contains("not set"));
    Ok(())
}

#[test]
fn batched_request_shape_and_response_are_round_tripped() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "batched_request_shape_and_response_are_round_tripped_child",
        "CONTEXTOS_TEST_KEY_FR54_BATCH",
        "sk-test-secret-value",
    )
}

#[test]
fn batched_request_shape_and_response_are_round_tripped_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_BATCH").is_none() {
        return Ok(());
    }

    let vault = tempfile::tempdir()?;
    let content = "# One\n\nFirst section prose.\n\n# Two\n\nSecond section prose.\n";
    let chunks = chunks_for(&vault, "sections.md", content)?;
    assert!(chunks.len() >= 2, "fixture should yield multiple chunks");

    let response_body = serde_json::json!({
        "object": "list",
        "model": "test-model",
        "data": chunks
            .iter()
            .enumerate()
            .map(|(index, _)| serde_json::json!({
                "object": "embedding",
                "index": index,
                "embedding": vec![0.25_f32, 0.5, 0.75],
            }))
            .collect::<Vec<_>>(),
    });
    let stub = HttpStub::start(StubResponse::Immediate {
        status: 200,
        body: serde_json::to_vec(&response_body)?,
    })?;

    let provider = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: stub.endpoint("/v1/embeddings"),
        model: "test-model".to_owned(),
        api_key_env: "CONTEXTOS_TEST_KEY_FR54_BATCH".to_owned(),
    })?;

    let vectors = provider.embed(&chunks)?;
    let captured = stub.join()?;

    assert_eq!(vectors.len(), chunks.len());
    for vector in &vectors {
        assert_eq!(vector.len(), 3);
    }
    assert_eq!(provider.dimension(), Some(3));

    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/v1/embeddings");
    assert_eq!(captured.authorization.as_deref(), Some("Bearer sk-test-secret-value"));
    let sent: Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(sent["model"], "test-model");
    let sent_inputs = sent["input"].as_array().ok_or("expected input to be a JSON array")?;
    assert_eq!(sent_inputs.len(), chunks.len());
    for (sent_input, chunk) in sent_inputs.iter().zip(chunks.iter()) {
        assert_eq!(sent_input.as_str(), Some(chunk.text()));
    }
    Ok(())
}

#[test]
fn empty_input_sends_no_request() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "empty_input_sends_no_request_child",
        "CONTEXTOS_TEST_KEY_FR54_EMPTY",
        "sk-unused",
    )
}

#[test]
fn empty_input_sends_no_request_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_EMPTY").is_none() {
        return Ok(());
    }

    let provider = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: "http://127.0.0.1:1/v1/embeddings".to_owned(),
        model: "test-model".to_owned(),
        api_key_env: "CONTEXTOS_TEST_KEY_FR54_EMPTY".to_owned(),
    })?;

    let vectors = provider.embed(&[])?;

    assert!(vectors.is_empty());
    Ok(())
}

#[test]
fn timeout_is_reported_as_a_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "timeout_is_reported_as_a_typed_error_child",
        "CONTEXTOS_TEST_KEY_FR54_TIMEOUT",
        "sk-unused",
    )
}

#[test]
fn timeout_is_reported_as_a_typed_error_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_TIMEOUT").is_none() {
        return Ok(());
    }

    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "note.md", "Some prose to embed.\n")?;

    let stub = HttpStub::start(StubResponse::Delayed {
        delay: Duration::from_millis(400),
        status: 200,
        body: b"{}".to_vec(),
    })?;

    let provider = OpenAiCompatible::with_timeout(
        OpenAiCompatibleConfig {
            endpoint: stub.endpoint("/v1/embeddings"),
            model: "test-model".to_owned(),
            api_key_env: "CONTEXTOS_TEST_KEY_FR54_TIMEOUT".to_owned(),
        },
        Duration::from_millis(50),
    )?;

    let result = provider.embed(&chunks);
    let _ = stub.join();

    let Err(error) = result else {
        return Err("expected a timeout error".into());
    };
    assert_eq!(error.code(), "embedding/timeout");
    Ok(())
}

#[test]
fn oversized_response_body_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "oversized_response_body_is_rejected_child",
        "CONTEXTOS_TEST_KEY_FR54_OVERSIZE",
        "sk-unused",
    )
}

#[test]
fn oversized_response_body_is_rejected_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_OVERSIZE").is_none() {
        return Ok(());
    }

    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "note.md", "Some prose to embed.\n")?;

    // One byte over the provider's response size limit (8 MiB), so the
    // limit at the network boundary rejects it regardless of the declared
    // `Content-Length` the stub sends.
    let oversized_body = vec![b' '; (8 * 1024 * 1024) + 1];
    let stub = HttpStub::start(StubResponse::Immediate {
        status: 200,
        body: oversized_body,
    })?;

    let provider = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: stub.endpoint("/v1/embeddings"),
        model: "test-model".to_owned(),
        api_key_env: "CONTEXTOS_TEST_KEY_FR54_OVERSIZE".to_owned(),
    })?;

    let result = provider.embed(&chunks);
    let _ = stub.join();

    let Err(error) = result else {
        return Err("expected an oversized-response error".into());
    };
    assert_eq!(error.code(), "embedding/response-too-large");
    Ok(())
}

#[test]
fn non_2xx_status_is_mapped_to_a_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "non_2xx_status_is_mapped_to_a_typed_error_child",
        "CONTEXTOS_TEST_KEY_FR54_STATUS",
        "sk-unused",
    )
}

#[test]
fn non_2xx_status_is_mapped_to_a_typed_error_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_STATUS").is_none() {
        return Ok(());
    }

    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "note.md", "Some prose to embed.\n")?;

    let stub = HttpStub::start(StubResponse::Immediate {
        status: 401,
        body: br#"{"error":{"message":"invalid api key"}}"#.to_vec(),
    })?;

    let provider = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: stub.endpoint("/v1/embeddings"),
        model: "test-model".to_owned(),
        api_key_env: "CONTEXTOS_TEST_KEY_FR54_STATUS".to_owned(),
    })?;

    let result = provider.embed(&chunks);
    let _ = stub.join();

    let Err(error) = result else {
        return Err("expected a provider-status error".into());
    };
    assert_eq!(error.code(), "embedding/provider-error");
    assert!(error.to_string().contains("401"));
    Ok(())
}

#[test]
fn api_key_never_appears_in_debug_or_error_output() -> Result<(), Box<dyn std::error::Error>> {
    run_with_env_child(
        "api_key_never_appears_in_debug_or_error_output_child",
        "CONTEXTOS_TEST_KEY_FR54_REDACT",
        "sk-super-secret-do-not-leak",
    )
}

#[test]
fn api_key_never_appears_in_debug_or_error_output_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CONTEXTOS_TEST_KEY_FR54_REDACT").is_none() {
        return Ok(());
    }

    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "note.md", "Some prose to embed.\n")?;

    let stub = HttpStub::start(StubResponse::Immediate {
        status: 401,
        body: br#"{"error":{"message":"invalid api key"}}"#.to_vec(),
    })?;

    let provider = OpenAiCompatible::try_from(OpenAiCompatibleConfig {
        endpoint: stub.endpoint("/v1/embeddings"),
        model: "test-model".to_owned(),
        api_key_env: "CONTEXTOS_TEST_KEY_FR54_REDACT".to_owned(),
    })?;

    let debug_output = format!("{provider:?}");
    let result = provider.embed(&chunks);
    let _ = stub.join();
    let Err(error) = result else {
        return Err("expected a provider-status error".into());
    };
    let error_display = error.to_string();
    let error_debug = format!("{error:?}");

    assert!(!debug_output.contains("sk-super-secret-do-not-leak"));
    assert!(!error_display.contains("sk-super-secret-do-not-leak"));
    assert!(!error_debug.contains("sk-super-secret-do-not-leak"));
    Ok(())
}
