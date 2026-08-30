//! A deterministic, dependency-free [`EmbedsText`] fake for unit and
//! integration layers above the embedding providers.
//!
//! Real providers are either slow (local ONNX inference), require a real
//! model on disk, or need network access (`OpenAiCompatible`). Anything
//! that only needs *a* provider (chunk-to-vector plumbing, a vector store,
//! `query_semantic`) can use [`FakeEmbedder`] instead: same input text
//! always produces the same vector, in the same process or a fresh one,
//! with no I/O of any kind.

use sha2::{Digest, Sha256};

use crate::{Chunk, EmbedsText, SearchError};

/// Deterministic hash-based pseudo-embedder.
///
/// Each output vector component is derived from a SHA-256 digest of the
/// chunk's text and the component's index, so identical text always yields
/// an identical vector (across calls, and across process runs) without
/// storing anything or touching the filesystem or network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FakeEmbedder {
    dimension: usize,
}

impl FakeEmbedder {
    /// Constructs a fake that produces vectors of the given `dimension`.
    #[must_use]
    pub const fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

impl Default for FakeEmbedder {
    /// Defaults to a 16-dimensional vector: large enough to make an
    /// accidental collision between two distinct texts implausible in
    /// tests, small enough to keep fixtures readable.
    fn default() -> Self {
        Self::new(16)
    }
}

impl EmbedsText for FakeEmbedder {
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        Ok(chunks
            .iter()
            .map(|chunk| hash_vector(chunk.text(), self.dimension))
            .collect())
    }

    fn dimension(&self) -> Option<usize> {
        Some(self.dimension)
    }
}

/// Derives a deterministic `dimension`-length vector from `text`.
///
/// Component `index` is `hash_component(text, index)`; every component is
/// independent, so the function needs no running hash state and produces
/// the same result regardless of call order.
fn hash_vector(text: &str, dimension: usize) -> Vec<f32> {
    (0..dimension)
        .map(|index| hash_component(text, index))
        .collect()
}

/// Derives one deterministic component in the closed range `[-1.0, 1.0]`
/// from `text` and `index`.
///
/// Only widening numeric conversions are used (`u16` to `f32` is always
/// exact), so this never triggers a precision-loss lint and never needs an
/// `as` cast.
fn hash_component(text: &str, index: usize) -> f32 {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.update(index.to_le_bytes());
    let digest = hasher.finalize();
    let bytes: [u8; 2] = digest[0..2].try_into().unwrap_or([0; 2]);
    let value = u16::from_le_bytes(bytes);
    (f32::from(value) / f32::from(u16::MAX)).mul_add(2.0, -1.0)
}
