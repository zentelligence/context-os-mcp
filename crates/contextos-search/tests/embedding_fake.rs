//! FR-54: `FakeEmbedder`, the deterministic in-repo stand-in for a real
//! embedding provider, used above this layer wherever a test only needs
//! *a* provider (chunk-to-vector plumbing, a vector store, `query_semantic`
//! in later stages).
mod support;

use contextos_search::{Chunk, ChunkSource, EmbedsText, FakeEmbedder, chunk_document};
use support::vault_note;

fn chunks_for(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<Vec<Chunk>, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(chunk_document(ChunkSource {
        path: &path,
        content,
    }))
}

#[test]
fn fr_54_fake_embedder_is_deterministic_across_calls() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(
        &vault,
        "note.md",
        "Some prose about ContextOS embeddings.\n",
    )?;
    let embedder = FakeEmbedder::default();

    let first = embedder.embed(&chunks)?;
    let second = embedder.embed(&chunks)?;

    assert_eq!(first, second);
    Ok(())
}

#[test]
fn fr_54_fake_embedder_is_deterministic_across_fresh_instances()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(
        &vault,
        "note.md",
        "Some prose about ContextOS embeddings.\n",
    )?;

    let from_first_instance = FakeEmbedder::default().embed(&chunks)?;
    let from_second_instance = FakeEmbedder::default().embed(&chunks)?;

    assert_eq!(from_first_instance, from_second_instance);
    Ok(())
}

#[test]
fn fr_54_fake_embedder_distinguishes_different_text() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let first_chunks = chunks_for(&vault, "one.md", "The quick brown fox.\n")?;
    let second_chunks = chunks_for(&vault, "two.md", "An entirely different sentence.\n")?;
    let embedder = FakeEmbedder::default();

    let first = embedder.embed(&first_chunks)?;
    let second = embedder.embed(&second_chunks)?;

    assert_ne!(first, second);
    Ok(())
}

#[test]
fn fr_54_fake_embedder_every_vector_matches_reported_dimension()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# One\n\nFirst section prose.\n\n# Two\n\nSecond section prose.\n";
    let chunks = chunks_for(&vault, "sections.md", content)?;
    assert!(chunks.len() >= 2, "fixture should yield multiple chunks");
    let embedder = FakeEmbedder::new(24);

    let vectors = embedder.embed(&chunks)?;

    assert_eq!(embedder.dimension(), Some(24));
    for vector in &vectors {
        assert_eq!(vector.len(), 24);
    }
    Ok(())
}

#[test]
fn fr_54_fake_embedder_empty_input_yields_empty_output_without_error()
-> Result<(), Box<dyn std::error::Error>> {
    let embedder = FakeEmbedder::default();

    let vectors = embedder.embed(&[])?;

    assert!(vectors.is_empty());
    Ok(())
}
