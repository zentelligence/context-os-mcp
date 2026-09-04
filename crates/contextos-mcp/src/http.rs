//! Authenticated streamable HTTP transport.
//!
//! This module builds an [`axum::Router`] that mounts the rmcp streamable
//! HTTP service behind two request-level guards: bearer authentication and a
//! request body cap. The router nests exactly one `ContextOsServer`
//! instance (the same one the stdio transport serves) so indexes, Git
//! state, and per-root write locks stay unified across transports.
//!
//! The vendored `rmcp = "=2.2.0"` streamable-HTTP server negotiates protocol
//! revisions up to and including `2026-07-28` (see
//! [`rmcp::model::ProtocolVersion::V_2026_07_28`]), which is the minimum
//! protocol revision this server supports, even though the crate's own
//! `LATEST` constant predates it.
//!
//! The server runs the streamable HTTP transport in **stateless,
//! JSON-response mode** (`stateful_mode: false`, `json_response: true`).
//! Session state in the MCP sense would be redundant here: every tool call
//! already goes through the one shared `ContextOsServer`, whose Arc-backed
//! indexes, Git state, and write coordinators are the only state that
//! matters. Stateless mode also keeps every HTTP exchange a plain
//! request/response with a JSON body, which is both simpler to test through
//! `tower::ServiceExt::oneshot` and an explicitly documented option of the
//! MCP Streamable HTTP specification (it avoids SSE framing overhead for
//! simple request/response tool calls).
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ContextOsServer, HttpConfig};

/// Path the MCP streamable HTTP service is mounted under.
pub const MOUNT_PATH: &str = "/mcp";

/// Validates an HTTP transport bind and token combination without touching
/// the network.
///
/// This is the "validate" half of a deliberate validate-then-bind seam: it
/// only inspects the configured bind address and token, so tests can assert
/// the refusal without ever asking the operating system to listen on a
/// non-loopback address. [`build_router`] and the caller's own
/// `TcpListener::bind` are the "bind" half.
///
/// # Errors
///
/// Returns [`HttpTransportError::InvalidBindAddress`] when `bind` is not a
/// parseable `host:port` socket address, or
/// [`HttpTransportError::NonLoopbackBindWithoutToken`] when `bind` resolves
/// to a non-loopback address and `token` is empty.
pub fn validate_bind(bind: &str, token: &str) -> Result<(), HttpTransportError> {
    let address =
        bind.parse::<SocketAddr>()
            .map_err(|_source| HttpTransportError::InvalidBindAddress {
                bind: bind.to_owned(),
            })?;
    if !address.ip().is_loopback() && token.is_empty() {
        return Err(HttpTransportError::NonLoopbackBindWithoutToken {
            bind: bind.to_owned(),
        });
    }
    Ok(())
}

/// Builds the authenticated streamable HTTP router for one shared
/// `ContextOsServer` instance.
///
/// The returned router enforces, in order, for every request:
/// 1. Bearer authentication (skipped only when no token is configured).
/// 2. The configured request body cap (`max_body_kb`), before the nested MCP
///    service reads any of the body.
///
/// There is deliberately no CORS layer: browser cross-origin requests are
/// denied by the absence of any `Access-Control-Allow-Origin` handling.
///
/// # Errors
///
/// Returns [`HttpTransportError::BodyLimitOverflow`] when `max_body_kb`
/// overflows a byte count on this platform.
pub fn build_router(
    server: ContextOsServer,
    config: &HttpConfig,
) -> Result<Router, HttpTransportError> {
    let max_body_bytes = usize::try_from(
        config
            .max_body_kb
            .checked_mul(1024)
            .ok_or(HttpTransportError::BodyLimitOverflow)?,
    )
    .map_err(|_source| HttpTransportError::BodyLimitOverflow)?;

    let guard = Arc::new(HttpGuard {
        auth: AuthGuard::from_token(&config.token),
        max_body_bytes,
    });

    let session_manager = Arc::new(LocalSessionManager::default());
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        session_manager,
        StreamableHttpServerConfig::default()
            .with_stateful_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None),
    );

    Ok(Router::new()
        .nest_service(MOUNT_PATH, service)
        .layer(axum::middleware::from_fn(
            move |request: Request, next: Next| {
                let guard = Arc::clone(&guard);
                async move { enforce_http_contract(&guard, request, next).await }
            },
        )))
}

/// Typed startup and configuration failures for the HTTP transport.
#[derive(Debug, Error)]
pub enum HttpTransportError {
    /// A non-loopback bind was requested without any bearer token
    /// configured. Refusing to start is the safe default: an unauthenticated
    /// MCP server reachable beyond the local host would let any peer on that
    /// interface read and write the vault.
    #[error(
        "refusing to start the HTTP transport: bind address {bind} is not loopback and no bearer token is configured; set [server.http].token in the configuration file, export CONTEXTOS_MCP_TOKEN, or bind to a loopback address such as 127.0.0.1"
    )]
    NonLoopbackBindWithoutToken { bind: String },
    /// The configured bind string is not a parseable `host:port` socket
    /// address.
    #[error(
        "HTTP bind address is invalid; expected a host:port socket address such as 127.0.0.1:7331, got {bind}"
    )]
    InvalidBindAddress { bind: String },
    /// `max_body_kb` overflows a byte count on this platform.
    #[error("configured server.http.max_body_kb overflows a byte count")]
    BodyLimitOverflow,
}

/// Bundles the request-level guards a built router closes over.
struct HttpGuard {
    auth: AuthGuard,
    max_body_bytes: usize,
}

async fn enforce_http_contract(guard: &HttpGuard, request: Request, next: Next) -> Response {
    if let Some(rejection) = guard.auth.reject(request.headers()) {
        return rejection;
    }
    match cap_body(request, guard.max_body_bytes).await {
        Ok(request) => next.run(request).await,
        Err(rejection) => *rejection,
    }
}

/// Bearer-token gate compared by SHA-256 digest rather than raw bytes.
///
/// # Why hashing defeats timing probes without a constant-time-compare crate
///
/// The two digests below are compared with plain slice equality (`==`),
/// which is free to exit at the first differing byte and is therefore not a
/// constant-time comparison in the usual sense. That would matter if the
/// values being compared were the raw secret: an attacker measuring
/// per-byte timing could recover the token one byte at a time, because two
/// candidate tokens that share a prefix produce comparisons that share a
/// prefix too.
///
/// Hashing the token before comparing breaks exactly that correlation.
/// SHA-256 has the avalanche property: changing a single byte of the input
/// changes roughly half the output bits, unpredictably. So a candidate token
/// that differs from the correct one by only its last byte does not produce
/// a digest that shares a prefix with the correct digest; the two digests
/// are, for an attacker's purposes, unrelated. There is no partial credit
/// for a partially correct guess, so there is nothing for a timing side
/// channel on the digest comparison to reveal. This is why hashing first is
/// sufficient here without pulling in a constant-time-equality dependency
/// such as `subtle`: the property we need (resistance to incremental,
/// byte-at-a-time guessing) comes from breaking the input/output
/// correlation, not from making the final comparison itself take constant
/// time.
#[derive(Clone)]
struct AuthGuard {
    /// `None` when no token is configured, meaning every request is
    /// accepted; `Some(digest)` otherwise.
    digest: Option<[u8; 32]>,
}

impl AuthGuard {
    fn from_token(token: &str) -> Self {
        Self {
            digest: (!token.is_empty()).then(|| digest_of(token.as_bytes())),
        }
    }

    /// Returns the 401 response to send when `headers` do not carry a valid
    /// bearer token, or `None` when the request may proceed.
    fn reject(&self, headers: &HeaderMap) -> Option<Response> {
        let expected = self.digest?;
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_token);
        match presented {
            Some(token) if digest_of(token.as_bytes()) == expected => None,
            _ => {
                // Never log the presented token or the raw header value.
                tracing::warn!(
                    code = "auth/missing-token",
                    "rejected HTTP MCP request without a valid bearer token"
                );
                Some(unauthorized_response())
            }
        }
    }
}

fn digest_of(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Extracts the token from an `Authorization: Bearer <token>` header value.
/// The scheme name is matched case-insensitively per RFC 7235. Any other
/// scheme, or a header with no scheme separator, yields `None`.
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

#[derive(Serialize)]
struct AuthErrorBody {
    code: &'static str,
    message: &'static str,
    remediation: &'static str,
}

const AUTH_ERROR_BODY: AuthErrorBody = AuthErrorBody {
    code: "auth/missing-token",
    message: "Authentication failed: the request did not carry a valid bearer token.",
    remediation: "Send 'Authorization: Bearer <token>' using the token configured at server.http.token or the CONTEXTOS_MCP_TOKEN environment variable.",
};

fn unauthorized_response() -> Response {
    let payload = serde_json::to_vec(&AUTH_ERROR_BODY).unwrap_or_default();
    let mut response = Response::new(Body::from(payload));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response.into_response()
}

fn payload_too_large_response() -> Response {
    let mut response = Response::new(Body::from(
        "request body exceeds the configured server.http.max_body_kb limit",
    ));
    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.into_response()
}

/// Caps the request body at `max_bytes`, rejecting oversize requests before
/// the nested MCP service ever reads the body.
///
/// A `Content-Length` over the limit is rejected without reading anything.
/// Otherwise the body is read up to `max_bytes` (via
/// [`axum::body::to_bytes`], which stops as soon as the limit is exceeded
/// rather than buffering an unbounded amount of data) as defence in depth
/// against a missing or understated `Content-Length`, such as chunked
/// transfer encoding.
async fn cap_body(request: Request, max_bytes: usize) -> Result<Request, Box<Response>> {
    if content_length_exceeds(request.headers(), max_bytes) {
        return Err(Box::new(payload_too_large_response()));
    }
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, max_bytes)
        .await
        .map_err(|_source| Box::new(payload_too_large_response()))?;
    Ok(Request::from_parts(parts, Body::from(bytes)))
}

fn content_length_exceeds(headers: &HeaderMap, max_bytes: usize) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_bytes)
}

#[cfg(test)]
mod tests {
    use super::{AuthGuard, HttpTransportError, bearer_token, validate_bind};

    #[test]
    fn bearer_token_matches_scheme_case_insensitively() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("BEARER   abc  "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("abc"), None);
    }

    #[test]
    fn auth_guard_without_a_token_accepts_every_request() {
        let guard = AuthGuard::from_token("");
        let headers = axum::http::HeaderMap::new();
        assert!(guard.reject(&headers).is_none());
    }

    #[test]
    fn auth_guard_rejects_a_wrong_token() {
        let guard = AuthGuard::from_token("correct-token");
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer wrong-token"),
        );
        assert!(guard.reject(&headers).is_some());
    }

    #[test]
    fn auth_guard_accepts_the_configured_token() {
        let guard = AuthGuard::from_token("correct-token");
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer correct-token"),
        );
        assert!(guard.reject(&headers).is_none());
    }

    #[test]
    fn validate_bind_accepts_loopback_without_a_token() -> Result<(), Box<dyn std::error::Error>> {
        validate_bind("127.0.0.1:7331", "")?;
        Ok(())
    }

    #[test]
    fn validate_bind_refuses_non_loopback_without_a_token() {
        let result = validate_bind("0.0.0.0:7331", "");
        assert!(matches!(
            result,
            Err(HttpTransportError::NonLoopbackBindWithoutToken { .. })
        ));
    }

    #[test]
    fn validate_bind_accepts_non_loopback_with_a_token() -> Result<(), Box<dyn std::error::Error>> {
        validate_bind("0.0.0.0:7331", "configured-token")?;
        Ok(())
    }

    #[test]
    fn validate_bind_rejects_an_unparseable_bind() {
        let result = validate_bind("not-a-socket-address", "");
        assert!(matches!(
            result,
            Err(HttpTransportError::InvalidBindAddress { .. })
        ));
    }
}
