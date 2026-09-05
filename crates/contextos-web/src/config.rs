//! `web.toml` loading and read-only `config.toml` vault-list loading.
//!
//! `contextos-web` never owns or writes `config.toml`: [`load_vault_set`]
//! parses only the `[[vault]]` blocks it needs (`path`, `name`, `managed`),
//! ignoring every other section (`limits`, `index_md`, `oplog`, `git`,
//! `search`, and so on) that belongs to `contextos-mcp`'s own schema, so it
//! deliberately does not `deny_unknown_fields` the way `contextos-mcp`'s own
//! `Config` does. It reuses `contextos-core`'s `VaultRoot`/`VaultSet`
//! directly rather than depending on `contextos-mcp`'s `Config` type, so
//! the two binaries share the validated domain type without sharing a
//! crate boundary.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use contextos_core::{PathError, VaultRoot, VaultRootInput, VaultSet};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// `web.toml`'s full schema.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default)]
    pub server: WebServerConfig,
    #[serde(default, rename = "mcp_server")]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl WebConfig {
    /// Re-checks every cross-field invariant [`load_web_config`] enforces on
    /// load. `pub(crate)` so [`crate::config_writer::WebConfigDocument`]
    /// can run the identical validator against an edited document before
    /// persisting it, rather than duplicating these rules.
    pub(crate) fn validate(&self) -> Result<(), WebConfigError> {
        validate_loopback_bind(&self.server.bind)?;
        let mut seen = std::collections::HashSet::new();
        for server in &self.mcp_servers {
            if !seen.insert(server.name().to_ascii_lowercase()) {
                return Err(WebConfigError::DuplicateMcpServerName {
                    name: server.name().to_owned(),
                });
            }
        }
        self.server.validate_logging()?;
        Ok(())
    }
}

/// Rejects any bind address that does not resolve to a loopback interface.
///
/// `contextos-web` ships with no authentication mechanism at all, so a
/// non-loopback bind would expose every proxied MCP tool (and the
/// static asset directory) to any host reachable on that interface. There is
/// deliberately no token-bypass the way `contextos-mcp`'s own
/// `validate_bind` allows for its HTTP transport (`architecture.md` §7.6):
/// no bearer-token mechanism exists in `contextos-web` to bypass with.
fn validate_loopback_bind(bind: &str) -> Result<(), WebConfigError> {
    let address = bind
        .parse::<SocketAddr>()
        .map_err(|_source| WebConfigError::InvalidBindAddress { bind: bind.to_owned() })?;
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(WebConfigError::NonLoopbackBind { bind: bind.to_owned() })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    /// An operator-configured override/addition to the crate's own bundled
    /// `static/` assets (embedded in the binary, `static_assets` module);
    /// optional, and `None` by default, since the bundled set already
    /// makes `/static/` fully servable with no configuration at all
    /// (FR-250).
    #[serde(default)]
    pub static_dir: Option<PathBuf>,
    #[serde(default)]
    pub log_level: WebLogLevel,
    #[serde(default)]
    pub log_file: String,
    /// Rotates `log_file` once it reaches this size, in megabytes. `None`
    /// (the default) never rotates on size. Meaningless (and rejected by
    /// [`WebServerConfig::validate_logging`]) while `log_file` is empty:
    /// there is no file to rotate when logging goes to stderr.
    #[serde(default)]
    pub log_max_size_mb: Option<u64>,
    /// Rotates `log_file` on every UTC calendar-day boundary. `false` (the
    /// default) never rotates on time. Combinable with `log_max_size_mb`:
    /// whichever condition is reached first triggers a rotation. Rejected
    /// by [`WebServerConfig::validate_logging`] while `log_file` is empty.
    #[serde(default)]
    pub log_rotate_daily: bool,
    /// Deletes a rotated log file once it is older than this many days.
    /// Independent of `log_retention_files`: when both are set, a rotated
    /// file is deleted once it violates either bound. `None` never prunes
    /// by age. Rejected while `log_file` is empty or neither rotation
    /// trigger is set (retention with nothing that ever rotates has no
    /// effect, which is more likely a misconfiguration than a deliberate
    /// no-op).
    #[serde(default)]
    pub log_retention_days: Option<u32>,
    /// Keeps only the `N` most recently rotated log files, deleting older
    /// ones. Same validation and combination rules as `log_retention_days`.
    #[serde(default)]
    pub log_retention_files: Option<u32>,
    /// Opaque: `web-architecture.md` §2 explicitly defers this table's
    /// schema to `web-rendering.md`, so this phase neither enumerates nor
    /// rejects its keys.
    #[serde(default)]
    pub ui: toml::Table,
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            static_dir: None,
            log_level: WebLogLevel::default(),
            log_file: String::new(),
            log_max_size_mb: None,
            log_rotate_daily: false,
            log_retention_days: None,
            log_retention_files: None,
            ui: toml::Table::default(),
        }
    }
}

impl WebServerConfig {
    /// Rejects a logging configuration that can never take effect: any
    /// rotation or retention setting while `log_file` is empty (logging
    /// goes to stderr, which cannot be rotated or pruned), a zero-megabyte
    /// size threshold (never satisfiable, since a freshly rotated file
    /// starts at zero bytes), a zero-day or zero-file retention bound (same
    /// "never satisfiable" reasoning), or a retention setting with neither
    /// rotation trigger enabled (nothing would ever produce a rotated file
    /// for retention to prune, so the setting could only be a
    /// misconfiguration, never a deliberate no-op).
    fn validate_logging(&self) -> Result<(), WebConfigError> {
        let rotates = self.log_max_size_mb.is_some() || self.log_rotate_daily;
        let retains = self.log_retention_days.is_some() || self.log_retention_files.is_some();
        if self.log_file.is_empty() && (rotates || retains) {
            return Err(WebConfigError::LogRotationWithoutFile);
        }
        if self.log_max_size_mb == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_max_size_mb must be at least 1".to_owned(),
            });
        }
        if self.log_retention_days == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_days must be at least 1".to_owned(),
            });
        }
        if self.log_retention_files == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_files must be at least 1".to_owned(),
            });
        }
        if retains && !rotates {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_days/log_retention_files has no effect without \
                         log_max_size_mb or log_rotate_daily"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

fn default_bind() -> String {
    "127.0.0.1:7332".to_owned()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebLogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

/// One `[[mcp_server]]` entry. Internally tagged on `transport` so
/// a stdio and an HTTP entry can carry their own, non-overlapping required
/// fields (`command`/`args` versus `endpoint`/`token_env`) rather than
/// making every field optional on a single flat shape.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "transport", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpServerConfig {
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        name: String,
        endpoint: String,
        #[serde(default)]
        token_env: Option<String>,
    },
}

impl McpServerConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::Http { name, .. } => name,
        }
    }
}

/// Loads and validates `web.toml` from `path`.
///
/// # Errors
///
/// Returns [`WebConfigError::Read`] when the file cannot be read,
/// [`WebConfigError::Toml`] when it does not parse against
/// [`WebConfig`]'s schema, or one of the validation variants
/// (`NonLoopbackBind`, `InvalidBindAddress`, `DuplicateMcpServerName`) when
/// it parses but violates a documented invariant.
pub fn load_web_config(path: &Path) -> Result<WebConfig, WebConfigError> {
    let source = fs::read_to_string(path).map_err(|source| WebConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    let config: WebConfig = toml::from_str(&source).map_err(|source| WebConfigError::Toml {
        source: Box::new(source),
    })?;
    config.validate()?;
    Ok(config)
}

/// The default `system_name` (`page_shell.html`'s header title) when
/// `[server.ui]` sets none: the shell must always show some title, so this
/// is the resolved value, not merely a placeholder string shown in a form.
pub const DEFAULT_SYSTEM_NAME: &str = "Command Centre";

/// The four `[server.ui]` keys `/settings/`'s Appearance pane,
/// `contextos-web.css`, and `page_shell.html`'s header give real,
/// operator-visible effect (colour theme, font, base text size, and the
/// header's own title); every other `[server.ui]` key stays persisted but
/// inert, matching `config.rs`'s own "a rendering/theme concern deferred
/// ... not enumerated here" stance for the table as a whole.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appearance {
    pub theme: Option<String>,
    pub font: Option<String>,
    pub size: Option<String>,
    /// The header's own title, next to the logo (`page_shell.html`).
    /// Unlike `theme`/`font`/`size` (each `None` when unset, so the
    /// corresponding CSS attribute selector is simply absent), the header
    /// always needs a title to show, so this is a resolved `String`, never
    /// unset: [`current_appearance`] and [`Appearance::default`] both fall
    /// back to [`DEFAULT_SYSTEM_NAME`].
    pub system_name: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: None,
            font: None,
            size: None,
            system_name: DEFAULT_SYSTEM_NAME.to_owned(),
        }
    }
}

/// Reads [`Appearance`] from `[server.ui]` fresh on every call (no
/// caching): a `/settings/` appearance save takes effect on the very next
/// page render. Never propagates a load failure to the caller (a
/// nav-shell-only concern, per
/// [`shell::build_nav`](crate::rendering::shell::build_nav)'s own "degrade
/// rather than fail the page" contract) or coerces a non-string value (an
/// operator-set number or table under one of these keys has no defined
/// meaning to render, so it is treated as unset rather than guessed at).
#[must_use]
pub fn current_appearance(path: &Path) -> Appearance {
    let Ok(config) = load_web_config(path) else {
        return Appearance::default();
    };
    let string_value = |key: &str| match config.server.ui.get(key) {
        Some(toml::Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    Appearance {
        theme: string_value("theme"),
        font: string_value("font"),
        size: string_value("size"),
        system_name: string_value("system_name").unwrap_or_else(|| DEFAULT_SYSTEM_NAME.to_owned()),
    }
}

/// The subset of `config.toml`'s schema `contextos-web` reads: each
/// `[[vault]]` block's `path`, `name`, and `managed`. Every other
/// field `contextos-mcp`'s own `Config` recognises is silently ignored
/// here, deliberately: this is a read-only, non-owning reader, not a
/// second implementation of `contextos-mcp`'s schema.
#[derive(Deserialize)]
struct VaultListConfig {
    #[serde(default, rename = "vault")]
    vaults: Vec<VaultListEntry>,
}

#[derive(Deserialize)]
struct VaultListEntry {
    path: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_managed")]
    managed: bool,
}

const fn default_managed() -> bool {
    true
}

/// Loads `config.toml`'s vault list from `path` and resolves it to a
/// [`VaultSet`], the same `contextos-core` type `contextos-mcp` resolves
/// from the identical file, so both binaries agree on which vaults exist,
/// their `name`, and their root without a second source of truth.
///
/// # Errors
///
/// Returns [`WebConfigError::Read`] when the file cannot be read,
/// [`WebConfigError::Toml`] when it does not parse, or
/// [`WebConfigError::VaultSet`] when the parsed vault list fails
/// `contextos-core`'s own root/name validation (a non-existent path, a
/// duplicate or invalid name, or no vault configured at all).
pub fn load_vault_set(path: &Path) -> Result<VaultSet, WebConfigError> {
    let source = fs::read_to_string(path).map_err(|source| WebConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    let parsed: VaultListConfig = toml::from_str(&source).map_err(|source| WebConfigError::Toml {
        source: Box::new(source),
    })?;
    let roots = parsed
        .vaults
        .into_iter()
        .map(|entry| {
            VaultRoot::try_from(VaultRootInput {
                path: entry.path.clone(),
                managed: entry.managed,
                name: entry.name,
            })
            .map_err(|source| WebConfigError::VaultPath {
                path: entry.path,
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    VaultSet::try_from(roots).map_err(|source| WebConfigError::VaultSet { source })
}

#[derive(Debug, Error)]
pub enum WebConfigError {
    #[error("web configuration file could not be read: {path}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("web configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error(
        "server.bind must be a loopback address in this release (no authentication mechanism \
         exists to protect a non-loopback bind); got {bind}"
    )]
    NonLoopbackBind { bind: String },
    #[error(
        "server.bind is invalid; expected a host:port socket address such as 127.0.0.1:7332, \
         got {bind}"
    )]
    InvalidBindAddress { bind: String },
    #[error("duplicate [[mcp_server]] name: {name}")]
    DuplicateMcpServerName { name: String },
    #[error("log rotation and retention settings have no effect while log_file is empty (logging goes to stderr)")]
    LogRotationWithoutFile,
    #[error("invalid log rotation/retention configuration: {detail}")]
    InvalidLogRotation { detail: String },
    #[error("vault path is invalid: {path}", path = path.display())]
    VaultPath {
        path: PathBuf,
        #[source]
        source: PathError,
    },
    #[error("configured vault roots are invalid")]
    VaultSet {
        #[source]
        source: PathError,
    },
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
