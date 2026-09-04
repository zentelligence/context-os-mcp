#![forbid(unsafe_code)]

use std::error::Error;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use contextos_web::service::{
    CommandRunner, ServiceSpec, ServiceStatus, SystemCommandRunner, UninstallOutcome, current_platform_backend,
};
use contextos_web::{build_router, connect, load_web_config};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "contextos-web",
    version = concat!("v", env!("CARGO_PKG_VERSION")),
    about = "ContextOS web UI"
)]
struct Cli {
    /// Load configuration from this TOML file. Also the path a `service
    /// install` embeds into the generated service definition's own
    /// `--config` argument.
    #[arg(long, default_value = "web.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
enum Command {
    /// Install, remove, or report on `contextos-web` running as an
    /// auto-starting, per-user background service (`systemd --user` on
    /// Linux, a `launchd` `LaunchAgent` on macOS, a Scheduled Task on
    /// Windows). None of the three needs elevation.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum ServiceAction {
    /// Install (or re-install) and start the service.
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Report whether the service is installed and running.
    Status,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();

    if let Some(Command::Service { action }) = cli.command {
        return run_service_command(action, cli.config).await;
    }

    let config = load_web_config(&cli.config)?;
    initialise_tracing(config.server.log_level);

    tracing::info!(
        name = "contextos-web",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        mcp_servers = config.mcp_servers.len(),
        "contextos-web starting"
    );

    // The handshake gate: every configured MCP session must complete
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
    let router = build_router(clients, &config.server.static_dir, &cli.config, primary_server);
    tracing::info!(bind = %bound, "contextos-web listening");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Installs, removes, or reports on the `contextos-web` background service.
/// Resolves the running binary's own path, the operator's home and
/// configuration directories, and `config_path` into a [`ServiceSpec`],
/// then dispatches to the [`ServiceBackend`] matching this process's own
/// platform. Runs the platform command(s) on a blocking thread (blocking
/// work never runs on an async executor thread), matching
/// `contextos-mcp`'s own `run_config_mcp_command` pattern for a CLI
/// subcommand backed by synchronous, potentially slow I/O.
///
/// [`ServiceBackend`]: contextos_web::service::ServiceBackend
async fn run_service_command(action: ServiceAction, config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    let binary_path = std::env::current_exe()?;
    let web_config_path = std::path::absolute(&config_path)?;
    let base_dirs = directories::BaseDirs::new().ok_or("could not determine the current user's home directory")?;
    let spec = ServiceSpec {
        binary_path,
        web_config_path,
        home_dir: base_dirs.home_dir().to_path_buf(),
        config_dir: base_dirs.config_dir().to_path_buf(),
    };

    tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        let backend = current_platform_backend()?;
        let runner = SystemCommandRunner;
        let runner: &dyn CommandRunner = &runner;
        let mut stdout = std::io::stdout().lock();
        match action {
            ServiceAction::Install => {
                backend.install(runner, &spec)?;
                writeln!(stdout, "Installed and started the contextos-web service.")?;
            }
            ServiceAction::Uninstall => match backend.uninstall(runner, &spec)? {
                UninstallOutcome::Removed => {
                    writeln!(stdout, "Removed the contextos-web service.")?;
                }
                UninstallOutcome::NotInstalled => {
                    writeln!(stdout, "The contextos-web service was not installed.")?;
                }
            },
            ServiceAction::Status => match backend.status(runner, &spec)? {
                ServiceStatus::Installed { running: true } => {
                    writeln!(stdout, "The contextos-web service is installed and running.")?;
                }
                ServiceStatus::Installed { running: false } => {
                    writeln!(stdout, "The contextos-web service is installed but not running.")?;
                }
                ServiceStatus::NotInstalled => {
                    writeln!(stdout, "The contextos-web service is not installed.")?;
                }
            },
        }
        Ok(())
    })
    .await??;
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
