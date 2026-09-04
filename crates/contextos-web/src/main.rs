#![forbid(unsafe_code)]

use std::error::Error;
use std::path::PathBuf;

use clap::Parser;
use contextos_web::{build_router, connect, load_web_config};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "contextos-web",
    version,
    about = "ContextOS web UI and MCP proxy"
)]
struct Cli {
    /// Load web-server configuration from this TOML file (FR-203).
    #[arg(long, default_value = "web.toml")]
    web_config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    let config = load_web_config(&cli.web_config)?;
    initialise_tracing(config.server.log_level);

    tracing::info!(
        name = "contextos-web",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        mcp_servers = config.mcp_servers.len(),
        "contextos-web starting"
    );

    // FR-204's handshake gate: every configured MCP session must complete
    // its `initialize` handshake before any HTTP request is served. A
    // failure here is the startup error the caller sees; no listener is
    // ever bound on a partially-connected client set.
    let clients = connect(&config).await?;

    let primary_server = config
        .mcp_servers
        .first()
        .map(|entry| entry.name().to_owned())
        .unwrap_or_default();

    let listener = TcpListener::bind(&config.server.bind).await?;
    let bound = listener.local_addr()?;
    let router = build_router(
        clients,
        &config.server.static_dir,
        &cli.web_config,
        primary_server,
    );
    tracing::info!(bind = %bound, "contextos-web listening");
    axum::serve(listener, router).await?;
    Ok(())
}

fn initialise_tracing(level: contextos_web::WebLogLevel) {
    let filter = match level {
        contextos_web::WebLogLevel::Error => "error",
        contextos_web::WebLogLevel::Warn => "warn",
        contextos_web::WebLogLevel::Info => "info",
        contextos_web::WebLogLevel::Debug => "debug",
        contextos_web::WebLogLevel::Trace => "trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
