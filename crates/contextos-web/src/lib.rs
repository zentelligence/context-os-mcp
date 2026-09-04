#![forbid(unsafe_code)]

pub mod app;
pub mod apps;
pub mod atomic_write;
pub mod config;
pub mod config_writer;
pub mod mcp_client;
pub mod proxy;
pub mod rendering;
pub mod routes;
pub mod service;
pub mod static_assets;

pub use app::{build_router, connect};
pub use apps::{AppKind, AppStatus, AppTarget, RegisteredApp, discover_apps};
pub use config::{
    McpServerConfig, WebConfig, WebConfigError, WebLogLevel, WebServerConfig, load_vault_set, load_web_config,
};
pub use config_writer::{WebConfigDocument, WebConfigWriterError};
pub use mcp_client::{McpCallError, McpClient, McpClientSet, McpConnectError};
