//! Provider-abstracted text embedding.
//!
//! `EmbedsText` is the port every embedding provider implements; concrete
//! providers live in sibling modules ([`crate::fake_embedder`],
//! [`crate::openai_embedder`], and, behind the default-on `semantic-local`
//! feature, [`crate::fastembed_embedder`]). This module also owns
//! [`EmbeddingProviderConfig`], a vault-agnostic mirror of
//! `contextos_server::config::EmbeddingConfig`'s `provider`, `model`,
//! `endpoint`, and `api_key_env` fields, and the `TryFrom` conversion that
//! turns it into a running provider. The conversion lives here (rather than
//! in `contextos-server`) because only this crate sees both the config
//! shape and the concrete provider types; `contextos-server` never depends
//! on a lower crate's absence, and library crates never depend on
//! `contextos-server` (architecture.md dependency direction).
//!
//! Every method here is synchronous, matching every other capability trait
//! in this crate ([`crate::IndexesText`], [`crate::LinkGraph`]): this crate
//! has no async runtime dependency at all. Blocking CPU work (local ONNX
//! inference) and blocking network I/O (the openai-compatible HTTP call)
//! are the composition root's responsibility to offload, exactly as
//! `contextos-server` already offloads every other blocking search and
//! filesystem call through `tokio::task::spawn_blocking` rather than this
//! crate reaching for an async trait or a bundled runtime.

use std::path::PathBuf;

use crate::{Chunk, SearchError};

/// Port for turning chunk text into embedding vectors.
///
/// Implementations are synchronous and safe to call from multiple threads;
/// the composition root decides how to keep this work off an async executor
/// thread.
pub trait EmbedsText: Send + Sync {
    /// Embeds one vector per input chunk, preserving input order.
    ///
    /// Returns an empty vector, performing no I/O, for an empty `chunks`
    /// slice. Otherwise every returned vector has the same length, and that
    /// length becomes visible through [`EmbedsText::dimension`] once this
    /// call returns successfully.
    ///
    /// # Errors
    ///
    /// Returns a typed [`SearchError`] when the provider cannot be reached
    /// or times out, when it returns a malformed, oversized, or
    /// inconsistently shaped response, or when a local model cannot be
    /// loaded. Never panics on provider misbehaviour.
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError>;

    /// Returns the fixed dimension of every vector this provider produces,
    /// once known.
    ///
    /// A provider whose model exposes its output dimension without
    /// inference (for example the local ONNX provider, which the addendum
    /// pins to a single known model) reports it immediately after
    /// construction. A provider that cannot know its dimension from
    /// configuration alone (an arbitrary openai-compatible endpoint)
    /// reports `None` until its first successful [`EmbedsText::embed`] call
    /// establishes it.
    fn dimension(&self) -> Option<usize>;
}

/// Vault-agnostic embedding provider selection configuration, mirroring
/// `contextos_server::config::EmbeddingConfig` (`provider`, `model`,
/// `endpoint`, `api_key_env`) so provider selection stays purely
/// configuration-driven: swapping `provider` between `local` and
/// `openai-compatible` in the vault's TOML changes which variant the server
/// constructs, with no code change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EmbeddingProviderConfig {
    /// Selects the local ONNX provider, reading the given injected model
    /// directory (`[vault.search.embedding] provider = "local"`).
    Local {
        /// Directory holding the model's ONNX and tokenizer files. Always
        /// supplied by the caller (platform app-data per the addendum);
        /// never derived from the current working directory or the
        /// operator's home directory.
        model_directory: PathBuf,
    },
    /// Selects the OpenAI-compatible HTTP provider
    /// (`provider = "openai-compatible"`).
    OpenAiCompatible {
        /// The provider's `/v1/embeddings`-shaped endpoint URL.
        endpoint: String,
        /// The model name sent in each request body.
        model: String,
        /// Name of the environment variable holding the API key; never the
        /// key value itself (security.md: configuration stores variable
        /// names, not secrets).
        api_key_env: String,
    },
}

impl TryFrom<EmbeddingProviderConfig> for Box<dyn EmbedsText> {
    type Error = SearchError;

    /// Constructs the configured provider.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::LocalEmbeddingDisabled`] when `Local` is
    /// selected in a build compiled without the `semantic-local` feature,
    /// [`SearchError::EmbeddingModelUnavailable`] when the local model
    /// directory is missing or invalid, and
    /// [`SearchError::EmbeddingApiKeyMissing`] when `OpenAiCompatible` is
    /// selected and its named environment variable is absent.
    fn try_from(value: EmbeddingProviderConfig) -> Result<Self, Self::Error> {
        match value {
            EmbeddingProviderConfig::Local { model_directory } => local_provider(model_directory),
            EmbeddingProviderConfig::OpenAiCompatible {
                endpoint,
                model,
                api_key_env,
            } => {
                let provider = crate::openai_embedder::OpenAiCompatible::try_from(
                    crate::openai_embedder::OpenAiCompatibleConfig {
                        endpoint,
                        model,
                        api_key_env,
                    },
                )?;
                Ok(Box::new(provider))
            }
        }
    }
}

/// Forwards to the boxed provider, so callers that only know the provider
/// through `TryFrom<EmbeddingProviderConfig>`'s `Box<dyn EmbedsText>` (every
/// composition-root caller, since provider selection is config-driven) can
/// still satisfy an `E: EmbedsText` bound directly, without a second,
/// non-trait-object code path.
impl EmbedsText for Box<dyn EmbedsText> {
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        (**self).embed(chunks)
    }

    fn dimension(&self) -> Option<usize> {
        (**self).dimension()
    }
}

#[cfg(feature = "semantic-local")]
fn local_provider(model_directory: PathBuf) -> Result<Box<dyn EmbedsText>, SearchError> {
    let provider = crate::fastembed_embedder::FastembedLocal::try_from(model_directory)?;
    Ok(Box::new(provider))
}

#[cfg(not(feature = "semantic-local"))]
fn local_provider(_model_directory: PathBuf) -> Result<Box<dyn EmbedsText>, SearchError> {
    Err(SearchError::LocalEmbeddingDisabled)
}
