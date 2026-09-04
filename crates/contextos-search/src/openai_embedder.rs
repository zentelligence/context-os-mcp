//! `OpenAiCompatible`: an [`EmbedsText`] provider speaking the `OpenAI`
//! `/v1/embeddings` request shape. Always compiled, with no Cargo feature
//! gate, so switching between the local and remote embedding provider is a
//! configuration change rather than a rebuild.
//!
//! Failures here are typed [`SearchError`] values the caller degrades on;
//! this provider never touches the write pipeline (architecture.md
//! mutation boundary), and never runs during construction beyond reading
//! the configured API key's environment variable.

use std::io::Read as _;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};

use crate::{Chunk, EmbedsText, SearchError};

/// Maximum bytes read from one embedding response body, enforced
/// regardless of any `Content-Length` header the provider claims
/// (security.md network-boundary rule): a provider that lies about size,
/// or streams an unbounded body, cannot exhaust memory.
const MAX_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Default per-request timeout, covering connect through to the last
/// response byte. [`OpenAiCompatible::with_timeout`] overrides this,
/// primarily so tests can exercise timeout behaviour without a long
/// wall-clock wait.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Construction input for [`OpenAiCompatible`], mirroring
/// `contextos_mcp::config::EmbeddingConfig`'s `openai-compatible`
/// fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenAiCompatibleConfig {
    /// The provider's `/v1/embeddings`-shaped endpoint URL.
    pub endpoint: String,
    /// The model name sent in each request body.
    pub model: String,
    /// Name of the environment variable holding the API key; never the key
    /// value itself.
    pub api_key_env: String,
}

/// Newtype wrapping the resolved API key so any accidental future `Debug`
/// derive on a containing struct still redacts it, in addition to
/// [`OpenAiCompatible`]'s own hand-written `Debug` impl below (defence in
/// depth per security.md: "test redaction ... including error paths").
#[derive(Clone)]
struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey(\"<redacted>\")")
    }
}

/// An embedding provider speaking the `OpenAI` `/v1/embeddings` request
/// shape.
pub struct OpenAiCompatible {
    endpoint: String,
    model: String,
    api_key: ApiKey,
    client: Client,
    timeout: Duration,
    dimension: OnceLock<usize>,
}

impl std::fmt::Debug for OpenAiCompatible {
    /// Reports the endpoint and model but never the key: `client` and
    /// `dimension` are omitted (not merely redacted) via
    /// `finish_non_exhaustive`, so this never depends on `reqwest::Client`'s
    /// own `Debug` impl not leaking anything sensitive in the future.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompatible")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &self.api_key)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatible {
    /// Constructs the provider with an explicit request timeout.
    ///
    /// [`TryFrom<OpenAiCompatibleConfig>`] uses [`DEFAULT_TIMEOUT`] for
    /// production configuration; this entry point exists so tests can drive
    /// timeout behaviour without a long wall-clock wait.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingApiKeyMissing`] when the environment
    /// variable named by `config.api_key_env` is not set, and
    /// [`SearchError::EmbeddingTransport`] when the underlying HTTP client
    /// cannot be built. No network request is made by this constructor.
    pub fn with_timeout(
        config: OpenAiCompatibleConfig,
        timeout: Duration,
    ) -> Result<Self, SearchError> {
        Self::reject_insecure_endpoint(&config.endpoint)?;
        let key = std::env::var(&config.api_key_env).map_err(|_source| {
            SearchError::EmbeddingApiKeyMissing {
                variable: config.api_key_env.clone(),
            }
        })?;
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|source| SearchError::EmbeddingTransport {
                endpoint: config.endpoint.clone(),
                reason: format!("HTTP client could not be built: {source}"),
            })?;
        Ok(Self {
            endpoint: config.endpoint,
            model: config.model,
            api_key: ApiKey(key),
            client,
            timeout,
            dimension: OnceLock::new(),
        })
    }

    /// Rejects a configured endpoint that would send the `Authorization`
    /// header in cleartext: `https` is required unless the host is a
    /// loopback address, so a local test stub or a genuinely local provider
    /// still works, but a plain `http://` typo for a real provider is a
    /// startup-time error rather than a silent cleartext key leak
    /// (security.md network-boundary rule).
    fn reject_insecure_endpoint(endpoint: &str) -> Result<(), SearchError> {
        let parsed =
            reqwest::Url::parse(endpoint).map_err(|source| SearchError::EmbeddingConfig {
                reason: format!("endpoint '{endpoint}' is not a valid URL: {source}"),
            })?;
        if parsed.scheme() == "https" {
            return Ok(());
        }
        let is_loopback = match parsed.host_str() {
            Some("localhost") => true,
            Some(host) => host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback()),
            None => false,
        };
        if is_loopback {
            return Ok(());
        }
        Err(SearchError::EmbeddingInsecureEndpoint {
            endpoint: endpoint.to_owned(),
        })
    }

    fn map_transport_error(&self, source: &reqwest::Error) -> SearchError {
        if source.is_timeout() {
            let timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX);
            SearchError::EmbeddingTimeout {
                endpoint: self.endpoint.clone(),
                timeout_ms,
            }
        } else {
            SearchError::EmbeddingTransport {
                endpoint: self.endpoint.clone(),
                reason: source.to_string(),
            }
        }
    }

    /// Reads `response`'s body, rejecting it outright once it exceeds
    /// [`MAX_RESPONSE_BODY_BYTES`] rather than buffering an unbounded
    /// stream.
    fn read_bounded_body(&self, response: Response) -> Result<Vec<u8>, SearchError> {
        let cap = u64::try_from(MAX_RESPONSE_BODY_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
        let mut limited = response.take(cap);
        let mut buffer = Vec::new();
        limited
            .read_to_end(&mut buffer)
            .map_err(|source| SearchError::EmbeddingTransport {
                endpoint: self.endpoint.clone(),
                reason: format!("response body could not be read: {source}"),
            })?;
        if buffer.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(SearchError::EmbeddingResponseTooLarge {
                endpoint: self.endpoint.clone(),
                limit_bytes: MAX_RESPONSE_BODY_BYTES,
            });
        }
        Ok(buffer)
    }
}

impl TryFrom<OpenAiCompatibleConfig> for OpenAiCompatible {
    type Error = SearchError;

    /// Constructs the provider using [`DEFAULT_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingApiKeyMissing`] when the environment
    /// variable named by `value.api_key_env` is not set. No network request
    /// is made by this constructor.
    fn try_from(value: OpenAiCompatibleConfig) -> Result<Self, Self::Error> {
        Self::with_timeout(value, DEFAULT_TIMEOUT)
    }
}

impl EmbedsText for OpenAiCompatible {
    /// Sends every chunk's text as one batched `/v1/embeddings` request.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingTimeout`] when the request exceeds
    /// the configured timeout, [`SearchError::EmbeddingResponseTooLarge`]
    /// when the response exceeds [`MAX_RESPONSE_BODY_BYTES`],
    /// [`SearchError::EmbeddingProviderStatus`] for a non-2xx response, and
    /// [`SearchError::EmbeddingShapeMismatch`] when the response's vector
    /// count or dimension does not match the request.
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let inputs: Vec<&str> = chunks.iter().map(Chunk::text).collect();
        let request_body = EmbeddingRequestBody {
            model: &self.model,
            input: inputs,
        };
        let payload = serde_json::to_vec(&request_body).map_err(|source| {
            SearchError::EmbeddingTransport {
                endpoint: self.endpoint.clone(),
                reason: format!("request body could not be serialised: {source}"),
            }
        })?;

        let response = self
            .client
            .post(&self.endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key.0),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .map_err(|source| self.map_transport_error(&source))?;

        let status = response.status();
        let body = self.read_bounded_body(response)?;

        if !status.is_success() {
            let excerpt: String = String::from_utf8_lossy(&body).chars().take(200).collect();
            return Err(SearchError::EmbeddingProviderStatus {
                endpoint: self.endpoint.clone(),
                status: status.as_u16(),
                body_excerpt: excerpt,
            });
        }

        let parsed: EmbeddingResponseBody =
            serde_json::from_slice(&body).map_err(|source| SearchError::EmbeddingTransport {
                endpoint: self.endpoint.clone(),
                reason: format!("response body was not valid JSON: {source}"),
            })?;

        let mut entries = parsed.data;
        entries.sort_by_key(|entry| entry.index);
        let vectors: Vec<Vec<f32>> = entries.into_iter().map(|entry| entry.embedding).collect();

        if vectors.len() != chunks.len() {
            return Err(SearchError::EmbeddingShapeMismatch {
                reason: format!(
                    "provider returned {} vectors for {} chunks",
                    vectors.len(),
                    chunks.len()
                ),
            });
        }

        let Some(observed_dimension) = vectors.first().map(Vec::len) else {
            return Ok(vectors);
        };
        if vectors
            .iter()
            .any(|vector| vector.len() != observed_dimension)
        {
            return Err(SearchError::EmbeddingShapeMismatch {
                reason: "provider returned vectors of inconsistent length".to_owned(),
            });
        }
        if let Some(existing) = self.dimension.get() {
            if *existing != observed_dimension {
                return Err(SearchError::EmbeddingShapeMismatch {
                    reason: format!(
                        "provider dimension changed from {existing} to {observed_dimension}"
                    ),
                });
            }
        } else {
            let _ = self.dimension.set(observed_dimension);
        }

        Ok(vectors)
    }

    fn dimension(&self) -> Option<usize> {
        self.dimension.get().copied()
    }
}

#[derive(Serialize)]
struct EmbeddingRequestBody<'a> {
    model: &'a str,
    input: Vec<&'a str>,
}

/// The response body is third-party, external content (an embedding
/// provider's own reply), not configuration we control, so this
/// intentionally omits `deny_unknown_fields`: a provider adding fields such
/// as `usage` or `object` must not break parsing.
#[derive(Deserialize)]
struct EmbeddingResponseBody {
    data: Vec<EmbeddingResponseEntry>,
}

#[derive(Deserialize)]
struct EmbeddingResponseEntry {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}
