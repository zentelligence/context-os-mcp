#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use contextos_server::{
    Config, ConfigEnvironment, ConfigLoadInput, ContextOsServer, DeregisterOutcome, DoctorReport,
    HostPathResolution, IndexReport, InterviewEnvironment, LogLevel, ModelReport, RegisteredServer,
    RegistrationStatus, SystemProcessDetector, TerminalInterviewer, Transport,
    default_claude_desktop_config_path, default_model_cache_dir, deregister,
    download_default_model, load_config_document, register, run_interview, status as host_status,
    write_config_document,
};
use directories::BaseDirs;
use rmcp::{ServiceExt, transport::stdio};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "contextos", version, about = "ContextOS MCP server")]
struct Cli {
    /// Load configuration from this TOML file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Add an allowed vault directory. May repeat.
    #[arg(long = "vault", global = true)]
    vaults: Vec<PathBuf>,

    /// Override the configured runtime log level.
    #[arg(long, value_enum, global = true)]
    log_level: Option<CliLogLevel>,

    /// Enable the HTTP transport for this run, optionally overriding the
    /// configured bind address (for example `--http 127.0.0.1:9000`). With
    /// no address, the configured or default bind is used unchanged.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "", value_name = "ADDR")]
    http: Option<String>,

    /// Register the ephemeris tools (`ephemeris_moon_phase`,
    /// `ephemeris_solar_events`, and the rest) for this run, overriding
    /// `[server] astro` in config. Enable only; there is no `--no-astro` to
    /// force it off from an already-enabling config.
    #[arg(long, global = true)]
    astro: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Clone, Debug, Subcommand)]
enum CliCommand {
    /// Validate configuration, vault indexes, and local Git recovery state.
    Doctor {
        /// Resolve every currently auto-fixable finding (a stale or missing
        /// managed index, or an absent Git repository) instead of only
        /// reporting it. Mirrors the `doctor_resolve` MCP tool.
        #[arg(long)]
        resolve: bool,
        /// With --resolve, report what would be resolved without writing
        /// anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Rebuild every enabled vault search index (text and link graph).
    Index,
    /// Manage the shared local embedding model cache. Vault-independent:
    /// no `--config` or `--vault` is required.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Edit `config.toml` directly, or with no subcommand at all, run the
    /// interactive guided-setup interview. Vault-independent: works against
    /// a not-yet-valid or not-yet-existing configuration file, so it runs
    /// before the strict `Config` load the other subcommands require.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigAction {
    /// Manage this configuration's `[[vault]]` entries.
    Vault {
        #[command(subcommand)]
        action: ConfigVaultAction,
    },
    /// Register, deregister, or report this server's entry in an MCP
    /// host's own configuration file.
    Mcp {
        #[command(subcommand)]
        action: ConfigMcpAction,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigMcpAction {
    /// Add or update this server's entry in the host's `mcpServers` key.
    Register {
        #[arg(long, value_enum)]
        host: HostChoice,
        /// Skip discovery and edit this file directly. Required when
        /// discovery reports more than one candidate, or none.
        #[arg(long)]
        config_path: Option<PathBuf>,
        /// Proceed even though the host is detected running, logging a
        /// warning instead of refusing.
        #[arg(long)]
        force: bool,
    },
    /// Remove this server's entry from the host's `mcpServers` key.
    Deregister {
        #[arg(long, value_enum)]
        host: HostChoice,
        #[arg(long)]
        config_path: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Report whether this server is currently registered, without writing.
    Status {
        #[arg(long, value_enum)]
        host: HostChoice,
        #[arg(long)]
        config_path: Option<PathBuf>,
    },
}

/// Supported MCP hosts for `contextos config mcp`. Only Claude Desktop is
/// named anywhere in the governing specification today; a second variant
/// is additive when a second host is actually specified.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum HostChoice {
    #[value(name = "claude-desktop")]
    ClaudeDesktop,
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigVaultAction {
    /// Add a vault.
    Add {
        /// Explicit vault name, addressed as `name://relative-path`.
        name: String,
        /// Absolute path to an existing vault root directory.
        path: PathBuf,
        /// Mark this vault filesystem-only: mutating tools reject writes,
        /// and managed indexes, the oplog, and Git recovery are disabled.
        #[arg(long)]
        unmanaged: bool,
    },
    /// Remove a configured vault by name.
    Remove {
        /// The vault's configured or default-derived name.
        name: String,
    },
    /// List configured vaults.
    List,
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum ModelAction {
    /// Report the default local embedding model's cache status.
    List,
    /// Download the default local embedding model into the shared cache.
    Download,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<CliLogLevel> for LogLevel {
    fn from(value: CliLogLevel) -> Self {
        match value {
            CliLogLevel::Error => Self::Error,
            CliLogLevel::Warn => Self::Warn,
            CliLogLevel::Info => Self::Info,
            CliLogLevel::Debug => Self::Debug,
            CliLogLevel::Trace => Self::Trace,
        }
    }
}

// Best-effort startup diagnostic: unconditionally records this invocation's
// argv and environment to a fixed path next to the running executable,
// before anything fallible (CLI parsing, config loading) runs, so a launch
// that fails or is silently swallowed by the host still leaves behind what
// it was actually invoked with. Every failure inside is discarded rather
// than propagated, so this can never itself become the reason startup
// fails.
fn record_startup_diagnostic() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    let path = dir.join("contextos-startup-debug.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let _ = writeln!(
        file,
        "=== invocation at unix time {now} (pid {}) ===",
        std::process::id()
    );
    let _ = writeln!(file, "argv: {:?}", std::env::args().collect::<Vec<_>>());
    let mut vars: Vec<(String, String)> = std::env::vars().collect();
    vars.sort();
    let _ = writeln!(file, "env ({} vars):", vars.len());
    for (key, value) in vars {
        let value = if is_secret_env_var_name(&key) {
            "<redacted>"
        } else {
            value.as_str()
        };
        let _ = writeln!(file, "  {key}={value}");
    }
    let _ = writeln!(file);
}

/// Whether an environment variable's name suggests its value is a secret
/// (a token, key, or password) that this diagnostic must never write out in
/// full, regardless of which specific variable ends up carrying one.
fn is_secret_env_var_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["TOKEN", "SECRET", "KEY", "PASSWORD", "PASS"]
        .iter()
        .any(|marker| upper.contains(marker))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    record_startup_diagnostic();
    let mut cli = Cli::parse();
    // Taken once and re-bound after each independent-of-`Config` branch
    // below returns, rather than matched on `cli.command` directly:
    // `CliCommand` carries owned `String`/`PathBuf` fields (the `Config`
    // variant's vault name and path), so it is no longer `Copy`, unlike
    // before this subcommand existed.
    let command = cli.command.take();
    let command = match command {
        Some(CliCommand::Model { action }) => return run_model_command(action).await,
        Some(CliCommand::Config { action }) => {
            let config_path = resolve_config_toml_path(cli.config.clone())?;
            return dispatch_config_command(action, config_path).await;
        }
        other => other,
    };
    let mut config = Config::try_from(ConfigLoadInput {
        cli_config_path: cli.config,
        default_config_path: default_config_path(),
        cli_vaults: cli.vaults,
        cli_log_level: cli.log_level.map(LogLevel::from),
        environment: ConfigEnvironment {
            config_path: env::var_os("CONTEXTOS_MCP_CONFIG").map(PathBuf::from),
            token: env::var("CONTEXTOS_MCP_TOKEN").ok(),
            log_level: env::var("CONTEXTOS_MCP_LOG_LEVEL").ok(),
        },
    })?;
    initialise_tracing(config.server.log_level)?;
    // Emitted before any fallible construction below (config validation
    // already succeeded by this point, but `ContextOsServer::try_from` and
    // the CLI subcommand handlers have not run yet): the one log line
    // guaranteed to exist no matter what fails afterwards, so a process
    // that dies during construction still leaves behind confirmation it
    // was this build that ran, and which PID, rather than leaving an
    // operator unable to tell a startup failure apart from the binary
    // never having started at all.
    tracing::info!(
        name = "contextos",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        vaults = config.vaults.len(),
        "contextos starting"
    );
    if let Some(addr) = cli.http {
        apply_http_override(&mut config, addr);
    }
    if cli.astro {
        config.server.astro = true;
    }
    let command = match command {
        Some(CliCommand::Doctor { resolve, dry_run }) => {
            return run_doctor_command(config, resolve, dry_run).await;
        }
        other => other,
    };
    if matches!(command, Some(CliCommand::Index)) {
        let report = tokio::task::spawn_blocking(move || IndexReport::try_from(&config)).await??;
        let failed = report.has_failures();
        std::io::stdout()
            .lock()
            .write_all(report.to_string().as_bytes())?;
        if failed {
            return Err(
                std::io::Error::other("index rebuild found checks requiring action").into(),
            );
        }
        return Ok(());
    }

    let run_stdio = config.server.transports.contains(&Transport::Stdio);
    let run_http = config.server.transports.contains(&Transport::Http);
    let http_config = config.server.http.clone();
    let server = tokio::task::spawn_blocking(move || ContextOsServer::try_from(config)).await??;

    // One `CancellationToken` signals both transports to stop; dropping the
    // stdio `RunningService` on the losing branch of the `select!` below
    // closes stdio, and cancelling this token stops the HTTP listener from
    // accepting new connections while in-flight requests finish.
    let shutdown = CancellationToken::new();
    let semantic_drain_tasks = server.spawn_semantic_drain(&shutdown);

    let http_task = if run_http {
        contextos_server::validate_bind(&http_config.bind, &http_config.token)?;
        let listener = TcpListener::bind(&http_config.bind).await?;
        let bound = listener.local_addr()?;
        let router = contextos_server::build_router(server.clone(), &http_config)?;
        tracing::info!(transport = "http", bind = %bound, "starting ContextOS MCP HTTP server");
        let shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled().await })
                .await
        }))
    } else {
        None
    };

    let stdio_running = if run_stdio {
        tracing::info!(transport = "stdio", "starting ContextOS MCP server");
        Some(server.clone().serve(stdio()).await?)
    } else {
        None
    };

    match stdio_running {
        Some(running) => {
            tokio::select! {
                result = running.waiting() => { let _reason = result?; },
                result = shutdown_signal() => { result?; },
            }
        }
        None => shutdown_signal().await?,
    }
    shutdown.cancel();
    if let Some(task) = http_task {
        task.await??;
    }
    for task in semantic_drain_tasks {
        task.await?;
    }

    tokio::task::spawn_blocking(move || server.flush_substrates()).await??;
    Ok(())
}

/// Runs `contextos doctor`, or `contextos doctor --resolve`, which reuses
/// the exact same resolution path as the `doctor_resolve` MCP tool
/// (`contextos_server::resolve_for_cli`) so CLI and MCP stay behaviourally
/// identical for the auto-fixable set.
async fn run_doctor_command(
    config: Config,
    resolve: bool,
    dry_run: bool,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if resolve {
        let config_for_resolve = config.clone();
        let outcomes =
            tokio::task::spawn_blocking(move || -> Result<_, Box<dyn Error + Send + Sync>> {
                let server = ContextOsServer::try_from(config_for_resolve)?;
                Ok(contextos_server::resolve_for_cli(&server, dry_run)?)
            })
            .await??;
        let mut stdout = std::io::stdout().lock();
        for outcome in &outcomes {
            let verb = if outcome.resolved {
                "Resolved"
            } else {
                "Would resolve"
            };
            writeln!(
                stdout,
                "{verb}: {} ({}): {}",
                outcome.subject, outcome.vault, outcome.message
            )?;
        }
        let report = tokio::task::spawn_blocking(move || DoctorReport::try_from(&config)).await??;
        let failed = report.has_failures();
        stdout.write_all(report.to_string().as_bytes())?;
        if failed {
            return Err(std::io::Error::other("doctor found checks requiring action").into());
        }
        return Ok(());
    }
    let report = tokio::task::spawn_blocking(move || DoctorReport::try_from(&config)).await??;
    let failed = report.has_failures();
    std::io::stdout()
        .lock()
        .write_all(report.to_string().as_bytes())?;
    if failed {
        return Err(std::io::Error::other("doctor found checks requiring action").into());
    }
    Ok(())
}

/// Runs a `contextos model` subcommand. Vault-independent: fetches or
/// reports on the shared local embedding model cache without loading any
/// `Config`, so it works with no `--config` or `--vault` supplied.
async fn run_model_command(action: ModelAction) -> Result<(), Box<dyn Error + Send + Sync>> {
    let report = tokio::task::spawn_blocking(move || {
        let cache_dir = default_model_cache_dir()?;
        match action {
            ModelAction::List => Ok(ModelReport::list(&cache_dir)),
            ModelAction::Download => ModelReport::download(&cache_dir),
        }
    })
    .await??;
    std::io::stdout()
        .lock()
        .write_all(report.to_string().as_bytes())?;
    Ok(())
}

/// Dispatches a `contextos config` invocation: a subcommand (`vault`/`mcp`)
/// straight to [`run_config_command`], or no subcommand at all to the
/// interactive guided-setup interview.
async fn dispatch_config_command(
    action: Option<ConfigAction>,
    config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match action {
        Some(action) => run_config_command(action, config_path).await,
        None => run_config_interview(config_path).await,
    }
}

/// Runs a `contextos config` subcommand: structure-preserving edits to
/// `config.toml` via `ConfigDocument` (`config_writer.rs`). Vault-
/// independent in the same sense `Model` is: it works against a not-yet-
/// valid or not-yet-existing configuration file, so it never goes through
/// `Config::try_from(ConfigLoadInput)`'s strict load.
async fn run_config_command(
    action: ConfigAction,
    config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match action {
        ConfigAction::Vault { action } => run_config_vault_command(action, config_path).await,
        ConfigAction::Mcp { action } => run_config_mcp_command(action, config_path).await,
    }
}

/// Runs `contextos config` with no subcommand: the interactive guided-setup
/// interview. Real terminal I/O and real network/host-detection
/// dependencies, unlike `run_interview` itself, which is exercised in tests
/// against a scripted `Interviewer` and fake dependencies instead.
async fn run_config_interview(config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    let command = std::env::current_exe()?
        .to_str()
        .ok_or_else(|| std::io::Error::other("this binary's own path is not valid UTF-8"))?
        .to_owned();
    let report = tokio::task::spawn_blocking(move || -> Result<_, Box<dyn Error + Send + Sync>> {
        let model_cache_dir = default_model_cache_dir()?;
        let detector = SystemProcessDetector;
        let wait_tick_calls = std::sync::atomic::AtomicU32::new(0);
        let wait_tick = move || {
            let call = wait_tick_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if call.is_multiple_of(5) {
                tracing::info!("still waiting for Claude Desktop to close");
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        };
        let environment = InterviewEnvironment {
            config_path,
            model_cache_dir,
            download_model: &download_default_model,
            resolve_host_path: &default_claude_desktop_config_path,
            process_detector: &detector,
            wait_tick: &wait_tick,
            server_command: command,
        };
        let mut interviewer = TerminalInterviewer;
        Ok(run_interview(&mut interviewer, &environment)?)
    })
    .await??;
    std::io::stdout()
        .lock()
        .write_all(report.to_string().as_bytes())?;
    Ok(())
}

/// Resolves the host config-file path for a `contextos config mcp`
/// subcommand: the explicit `--config-path` override when given, otherwise
/// platform discovery, rejecting `Ambiguous`/`NotFound` with a message
/// telling the operator to pass `--config-path` explicitly.
fn resolve_host_config_path(
    host: HostChoice,
    config_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if let Some(path) = config_path {
        return Ok(path);
    }
    let HostChoice::ClaudeDesktop = host;
    match default_claude_desktop_config_path()? {
        HostPathResolution::Found(path) => Ok(path),
        HostPathResolution::NotFound { reason } => Err(std::io::Error::other(format!(
            "could not locate Claude Desktop's configuration file ({reason}); pass \
             --config-path explicitly"
        ))
        .into()),
        HostPathResolution::Ambiguous { candidates } => Err(std::io::Error::other(format!(
            "more than one Claude Desktop configuration file candidate was found ({}); pass \
             --config-path explicitly to pick one",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

async fn run_config_mcp_command(
    action: ConfigMcpAction,
    server_config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stdout = std::io::stdout().lock();
    match action {
        ConfigMcpAction::Register {
            host,
            config_path,
            force,
        } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let command = std::env::current_exe()?
                .to_str()
                .ok_or_else(|| std::io::Error::other("this binary's own path is not valid UTF-8"))?
                .to_owned();
            let server_config_path_str = server_config_path
                .to_str()
                .ok_or_else(|| std::io::Error::other("configuration path is not valid UTF-8"))?
                .to_owned();
            let entry = RegisteredServer {
                command,
                args: vec!["--config".to_owned(), server_config_path_str],
            };
            let detector = SystemProcessDetector;
            let host_path_for_task = host_path.clone();
            tokio::task::spawn_blocking(move || {
                register(&host_path_for_task, &entry, &detector, force)
            })
            .await??;
            if force {
                tracing::warn!(
                    host_path = %host_path.display(),
                    "registered contextos with --force while the host may still be running"
                );
            }
            writeln!(stdout, "Registered contextos with {}.", host_path.display())?;
        }
        ConfigMcpAction::Deregister {
            host,
            config_path,
            force,
        } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let detector = SystemProcessDetector;
            let host_path_for_task = host_path.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                deregister(&host_path_for_task, &detector, force)
            })
            .await??;
            if force {
                tracing::warn!(
                    host_path = %host_path.display(),
                    "deregistered contextos with --force while the host may still be running"
                );
            }
            match outcome {
                DeregisterOutcome::Removed => {
                    writeln!(
                        stdout,
                        "Deregistered contextos from {}.",
                        host_path.display()
                    )?;
                }
                DeregisterOutcome::NotRegistered => {
                    writeln!(
                        stdout,
                        "contextos was not registered with {}.",
                        host_path.display()
                    )?;
                }
            }
        }
        ConfigMcpAction::Status { host, config_path } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let host_path_for_task = host_path.clone();
            let status =
                tokio::task::spawn_blocking(move || host_status(&host_path_for_task)).await??;
            match status {
                RegistrationStatus::Registered(entry) => {
                    writeln!(
                        stdout,
                        "contextos is registered with {}: {} {}",
                        host_path.display(),
                        entry.command,
                        entry.args.join(" ")
                    )?;
                }
                RegistrationStatus::NotRegistered => {
                    writeln!(
                        stdout,
                        "contextos is not registered with {}.",
                        host_path.display()
                    )?;
                }
            }
        }
    }
    Ok(())
}

async fn run_config_vault_command(
    action: ConfigVaultAction,
    config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stdout = std::io::stdout().lock();
    match action {
        ConfigVaultAction::List => {
            let read_path = config_path.clone();
            let vaults = tokio::task::spawn_blocking(move || {
                Ok::<_, Box<dyn Error + Send + Sync>>(load_config_document(&read_path)?.vaults())
            })
            .await??;
            if vaults.is_empty() {
                writeln!(stdout, "No vaults configured in {}.", config_path.display())?;
            } else {
                writeln!(stdout, "Vaults configured in {}:", config_path.display())?;
                for vault in vaults {
                    let managed = if vault.managed {
                        "managed"
                    } else {
                        "unmanaged"
                    };
                    writeln!(
                        stdout,
                        "  {} -> {} ({managed})",
                        vault.name,
                        vault.path.display()
                    )?;
                }
            }
        }
        ConfigVaultAction::Add {
            name,
            path,
            unmanaged,
        } => {
            let write_path = config_path.clone();
            let (display_name, display_path) = (name.clone(), path.clone());
            tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
                let mut document = load_config_document(&write_path)?;
                document.add_vault(&name, &path, !unmanaged)?;
                write_config_document(&write_path, &document)?;
                Ok(())
            })
            .await??;
            writeln!(
                stdout,
                "Added vault {display_name:?} ({}) to {}.",
                display_path.display(),
                config_path.display()
            )?;
        }
        ConfigVaultAction::Remove { name } => {
            let write_path = config_path.clone();
            let display_name = name.clone();
            tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
                let mut document = load_config_document(&write_path)?;
                document.remove_vault(&name)?;
                write_config_document(&write_path, &document)?;
                Ok(())
            })
            .await??;
            writeln!(
                stdout,
                "Removed vault {display_name:?} from {}.",
                config_path.display()
            )?;
        }
    }
    Ok(())
}

/// Resolves the `config.toml` path for a `contextos config` subcommand,
/// matching `ConfigLoadInput`'s own precedence (`--config`, then
/// `CONTEXTOS_MCP_CONFIG`, then the default path) but standalone: this
/// subcommand edits the file directly and must run before
/// `Config::try_from` would reject a not-yet-valid or not-yet-existing one.
fn resolve_config_toml_path(
    cli_config_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    cli_config_path
        .or_else(|| env::var_os("CONTEXTOS_MCP_CONFIG").map(PathBuf::from))
        .or_else(default_config_path)
        .ok_or_else(|| {
            std::io::Error::other(
                "no configuration file path could be determined; pass --config or set \
                 CONTEXTOS_MCP_CONFIG",
            )
            .into()
        })
}

/// Applies the `--http [addr]` CLI override: enables the HTTP transport for
/// this run and, when `addr` is non-empty, overrides the configured bind.
fn apply_http_override(config: &mut Config, addr: String) {
    if !config.server.transports.contains(&Transport::Http) {
        config.server.transports.push(Transport::Http);
    }
    if !addr.is_empty() {
        config.server.http.bind = addr;
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

/// Fallback configuration file path used when neither `--config` nor
/// `CONTEXTOS_MCP_CONFIG` is set: `$HOME/.config/contextos/config.toml` on
/// every platform, so an operator can `contextos index` (or any other
/// subcommand) without ever passing a path, by simply placing their config
/// at this one well-known, documented location.
fn default_config_path() -> Option<PathBuf> {
    let base_dirs = BaseDirs::new()?;
    Some(
        base_dirs
            .home_dir()
            .join(".config")
            .join("contextos")
            .join("config.toml"),
    )
}

fn initialise_tracing(level: LogLevel) -> Result<(), Box<dyn Error + Send + Sync>> {
    let filter = match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use contextos_server::{ConfigEnvironment, ConfigLoadInput, Transport};

    use super::{
        Config, LogLevel, apply_http_override, default_config_path, is_secret_env_var_name,
    };

    #[test]
    fn secret_env_var_names_are_detected_case_insensitively() {
        for name in [
            "CONTEXTOS_MCP_TOKEN",
            "token",
            "MY_API_KEY",
            "SOME_SECRET",
            "DB_PASSWORD",
            "PASS",
        ] {
            assert!(is_secret_env_var_name(name), "expected {name:?} to match");
        }
    }

    #[test]
    fn ordinary_env_var_names_are_not_flagged_as_secret() {
        for name in ["PATH", "HOME", "CONTEXTOS_MCP_CONFIG", "RUST_LOG"] {
            assert!(
                !is_secret_env_var_name(name),
                "expected {name:?} not to match"
            );
        }
    }

    #[test]
    fn default_config_path_is_under_the_home_directory_config_folder()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let resolved = default_config_path().ok_or("home directory unavailable")?;

        assert_eq!(
            resolved.strip_prefix(
                directories::BaseDirs::new()
                    .ok_or("home directory unavailable")?
                    .home_dir()
            )?,
            std::path::Path::new(".config/contextos/config.toml")
        );
        Ok(())
    }

    #[test]
    fn http_cli_flag_enables_transport_and_optionally_overrides_bind()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let fixture = tempdir()?;
        let vault = fixture.path().join("vault");
        fs::create_dir_all(&vault)?;
        let mut config = Config::try_from(vec![vault])?;
        assert!(!config.server.transports.contains(&Transport::Http));

        apply_http_override(&mut config, String::new());

        assert!(config.server.transports.contains(&Transport::Http));
        assert_eq!(config.server.http.bind, "127.0.0.1:7331");

        apply_http_override(&mut config, "127.0.0.1:9000".to_owned());

        assert_eq!(
            config
                .server
                .transports
                .iter()
                .filter(|transport| **transport == Transport::Http)
                .count(),
            1
        );
        assert_eq!(config.server.http.bind, "127.0.0.1:9000");
        Ok(())
    }

    #[test]
    fn cli_vaults_replace_file_vaults_without_discarding_server_settings()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let fixture = tempdir()?;
        let file_vault = fixture.path().join("file-vault");
        let cli_vault = fixture.path().join("cli-vault");
        fs::create_dir_all(&file_vault)?;
        fs::create_dir_all(&cli_vault)?;
        let config_path = fixture.path().join("config.toml");
        fs::write(
            &config_path,
            format!("[server]\nlog_level = \"debug\"\n[[vault]]\npath = {file_vault:?}\n"),
        )?;

        let config = Config::try_from(ConfigLoadInput {
            cli_config_path: Some(config_path),
            cli_vaults: vec![cli_vault.clone()],
            environment: ConfigEnvironment::default(),
            ..ConfigLoadInput::default()
        })?;

        assert_eq!(config.server.log_level, LogLevel::Debug);
        assert_eq!(config.vaults.len(), 1);
        assert_eq!(config.vaults[0].path, cli_vault);
        Ok(())
    }
}
