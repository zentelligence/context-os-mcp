//! MCP client sessions to configured `[[mcp_server]]` entries (FR-204).
//!
//! `contextos-web` is a real MCP client, never an embedding of a service
//! crate's trait implementations (`D-W03`, `architecture.md` §3 of
//! `web-architecture.md`): every vault operation this crate performs is a
//! `call_tool` on a session built here.

use std::collections::HashMap;
use std::sync::Arc;

use rmcp::model::CallToolRequestParams;
use rmcp::service::{ClientInitializeError, RunningService};
use rmcp::transport::common::client_side_sse::NeverRetry;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use thiserror::Error;
use tokio::process::Command;

use crate::config::McpServerConfig;

/// A live MCP client session to one configured server.
pub struct McpClient {
    name: String,
    running: RunningService<RoleClient, ()>,
    /// The spawned child process's OS PID, present only for a `stdio`
    /// transport. Exposed for operational diagnostics and so a test can
    /// simulate a crashed `contextos-mcp` process (`NFR-W05`) without this
    /// module needing to invent its own process-supervision API surface.
    pid: Option<u32>,
}

impl McpClient {
    /// Spawns (stdio) or connects (HTTP) to `entry`, performing the MCP
    /// `initialize` handshake before returning. A failed spawn or handshake
    /// is returned as an error, never retried or silently degraded: FR-204
    /// requires this to be a startup error, not a lazily-discovered one.
    ///
    /// # Errors
    ///
    /// Returns [`McpConnectError::Spawn`] when the configured `command`
    /// cannot be started, [`McpConnectError::MissingTokenEnv`] when an HTTP
    /// entry names a `token_env` variable that is not set, or
    /// [`McpConnectError::Handshake`] when the process starts (or the HTTP
    /// endpoint connects) but the `initialize` handshake itself fails.
    pub async fn connect(entry: &McpServerConfig) -> Result<Self, McpConnectError> {
        match entry {
            McpServerConfig::Stdio {
                name,
                command,
                args,
            } => {
                let mut cmd = Command::new(command);
                cmd.args(args);
                let transport =
                    TokioChildProcess::new(cmd).map_err(|source| McpConnectError::Spawn {
                        server: name.clone(),
                        source,
                    })?;
                let pid = transport.id();
                let running =
                    ().serve(transport)
                        .await
                        .map_err(|source| McpConnectError::Handshake {
                            server: name.clone(),
                            source: Box::new(source),
                        })?;
                Ok(Self {
                    name: name.clone(),
                    running,
                    pid,
                })
            }
            McpServerConfig::Http {
                name,
                endpoint,
                token_env,
            } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.clone());
                // The default `ExponentialBackoff` retry policy applies to
                // the initial `initialize` request too, not just an
                // already-established session's reconnects: against a
                // genuinely unreachable endpoint it retries for minutes
                // rather than failing, which would turn FR-204's "a failed
                // handshake is a startup error, not a lazily-discovered
                // one" into an indefinite hang instead. A single attempt is
                // correct here: this is a startup connection, not a live
                // session worth reconnecting.
                config.retry_config = Arc::new(NeverRetry::default());
                if let Some(variable) = token_env {
                    let token = std::env::var(variable).map_err(|_source| {
                        McpConnectError::MissingTokenEnv {
                            server: name.clone(),
                            variable: variable.clone(),
                        }
                    })?;
                    config = config.auth_header(token);
                }
                let transport = StreamableHttpClientTransport::from_config(config);
                let running =
                    ().serve(transport)
                        .await
                        .map_err(|source| McpConnectError::Handshake {
                            server: name.clone(),
                            source: Box::new(source),
                        })?;
                Ok(Self {
                    name: name.clone(),
                    running,
                    pid: None,
                })
            }
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Calls `tool_name` on this session with `arguments`, relaying whatever
    /// the MCP server itself returns (including an MCP-level tool error
    /// result, `FR-213`) unmodified.
    ///
    /// # Errors
    ///
    /// Returns [`McpCallError::Unreachable`] when the transport itself
    /// fails (the server process died, the HTTP connection dropped): this
    /// is distinct from an MCP-level tool error, which is a successful
    /// `Ok(CallToolResult)` with `is_error: Some(true)`.
    pub async fn call_tool(
        &self,
        tool_name: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<rmcp::model::CallToolResult, McpCallError> {
        self.running
            .call_tool(CallToolRequestParams::new(tool_name).with_arguments(arguments))
            .await
            .map_err(|source| McpCallError::Unreachable {
                server: self.name.clone(),
                source: Box::new(source),
            })
    }
}

/// Every configured `[[mcp_server]]` session, connected once at startup and
/// held for the process lifetime.
pub struct McpClientSet(HashMap<String, Arc<McpClient>>);

impl McpClientSet {
    /// Connects to every entry in `entries` in order, failing fast on the
    /// first connection or handshake failure (FR-204: a startup error, not
    /// a partially-live set).
    ///
    /// # Errors
    ///
    /// Propagates the first [`McpConnectError`] any entry's
    /// [`McpClient::connect`] returns.
    pub async fn connect(entries: &[McpServerConfig]) -> Result<Self, McpConnectError> {
        let mut clients = HashMap::with_capacity(entries.len());
        for entry in entries {
            let client = McpClient::connect(entry).await?;
            clients.insert(client.name().to_owned(), Arc::new(client));
        }
        Ok(Self(clients))
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.0.get(name)
    }
}

/// Failures constructing an MCP client session (`McpClient::connect`,
/// `McpClientSet::connect`): always a startup-time concern (FR-204).
#[derive(Debug, Error)]
pub enum McpConnectError {
    #[error("failed to spawn MCP server {server:?}")]
    Spawn {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP initialize handshake failed for server {server:?}")]
    Handshake {
        server: String,
        #[source]
        source: Box<ClientInitializeError>,
    },
    #[error("environment variable {variable} (server {server:?}'s token_env) is not set")]
    MissingTokenEnv { server: String, variable: String },
}

/// Failures making a `call_tool` request against an already-connected
/// session: always a request-time concern (`NFR-W05`), never a startup one.
/// Kept as its own type rather than sharing [`McpConnectError`] so the
/// proxy route's error handling stays exhaustive without a catch-all arm
/// for connect-only variants that can never occur here.
#[derive(Debug, Error)]
pub enum McpCallError {
    #[error("MCP server {server:?} is unreachable")]
    Unreachable {
        server: String,
        #[source]
        source: Box<ServiceError>,
    },
}

#[cfg(test)]
#[path = "mcp_client_test.rs"]
mod tests;
