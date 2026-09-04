//! `FastembedLocal` construction, gated behind the default-on
//! `semantic-local` Cargo feature.
//!
//! Every test in this file that does not carry `#[ignore]` runs without any
//! real ONNX model or tokenizer present, and never downloads anything:
//! `FastembedLocal` only ever calls fastembed's user-defined model
//! constructor (`TextEmbedding::try_new_from_user_defined`), never its
//! `hf-hub`-backed auto-download constructor, and that feature is not even
//! compiled into this build (see `src/fastembed_embedder.rs`).
//!
//! `fr_54_local_model_directory_produces_consistent_embeddings` is the one
//! exception: it requires a real, valid model directory (ONNX file plus the
//! four tokenizer files) and is `#[ignore]`d so `cargo test` never attempts
//! it, or any download, by default. Point
//! `CONTEXTOS_TEST_FASTEMBED_MODEL_DIR` at such a directory and run it
//! explicitly:
//!
//! ```sh
//! CONTEXTOS_TEST_FASTEMBED_MODEL_DIR=/path/to/model cargo test \
//!     -p contextos-search --test embedding_fastembed -- --ignored
//! ```
//!
//! Populating that directory is later pre-fetch tooling's job (phase-5-plan
//! Stage 0/A2); this test only proves `FastembedLocal` can consume it once
//! it exists.
#![cfg(feature = "semantic-local")]

mod support;

use contextos_search::{Chunk, ChunkSource, EmbedsText, FastembedLocal, chunk_document};
use support::vault_note;

fn chunks_for(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<Vec<Chunk>, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(chunk_document(ChunkSource { path: &path, content }))
}

#[test]
fn missing_model_directory_is_a_typed_error_with_no_download() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let missing = vault.path().join("does-not-exist");

    let result = FastembedLocal::try_from(missing);

    let Err(error) = result else {
        return Err("expected construction to fail for a missing model directory".into());
    };
    assert_eq!(error.code(), "embedding/local-unavailable");
    Ok(())
}

#[test]
fn model_directory_missing_one_required_file_is_a_typed_error() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    // Every required file except `model.onnx`.
    std::fs::write(directory.path().join("tokenizer.json"), b"{}")?;
    std::fs::write(directory.path().join("config.json"), b"{}")?;
    std::fs::write(directory.path().join("special_tokens_map.json"), b"{}")?;
    std::fs::write(directory.path().join("tokenizer_config.json"), b"{}")?;

    let result = FastembedLocal::try_from(directory.path().to_path_buf());

    let Err(error) = result else {
        return Err("expected construction to fail when model.onnx is missing".into());
    };
    assert_eq!(error.code(), "embedding/local-unavailable");
    Ok(())
}

#[test]
fn malformed_model_files_are_a_typed_error_not_a_panic() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    // All five required files present, but every one holds garbage bytes
    // rather than a real ONNX model or tokenizer: fastembed must reject
    // this as invalid, not download a replacement or panic.
    std::fs::write(directory.path().join("model.onnx"), b"not a real onnx model")?;
    std::fs::write(directory.path().join("tokenizer.json"), b"not real json")?;
    std::fs::write(directory.path().join("config.json"), b"{}")?;
    std::fs::write(directory.path().join("special_tokens_map.json"), b"{}")?;
    std::fs::write(directory.path().join("tokenizer_config.json"), b"{}")?;

    let result = FastembedLocal::try_from(directory.path().to_path_buf());

    let Err(error) = result else {
        return Err("expected construction to fail for malformed model files".into());
    };
    assert_eq!(error.code(), "embedding/local-unavailable");
    Ok(())
}

#[test]
#[ignore = "requires a real ONNX model directory; see this file's module documentation"]
fn local_model_directory_produces_consistent_embeddings() -> Result<(), Box<dyn std::error::Error>> {
    let Some(model_directory) = std::env::var_os("CONTEXTOS_TEST_FASTEMBED_MODEL_DIR") else {
        return Err("set CONTEXTOS_TEST_FASTEMBED_MODEL_DIR to a real model directory to run this test".into());
    };

    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "note.md", "ContextOS embeds vault prose locally.\n")?;

    let provider = FastembedLocal::try_from(std::path::PathBuf::from(model_directory))?;
    let dimension = provider
        .dimension()
        .ok_or("expected the local provider to know its dimension immediately")?;

    let first = provider.embed(&chunks)?;
    let second = provider.embed(&chunks)?;

    assert_eq!(first, second, "same input should embed identically");
    for vector in &first {
        assert_eq!(vector.len(), dimension);
    }
    Ok(())
}
