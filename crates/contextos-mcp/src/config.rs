use std::fs;
use std::path::PathBuf;

use contextos_core::{PathError, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::default_hidden_patterns;
use contextos_search::GraphBackend;
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default, rename = "vault")]
    pub vaults: Vec<VaultConfig>,
}

impl TryFrom<&str> for Config {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(value).map_err(|source| ConfigError::Toml {
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }
}

impl TryFrom<Vec<PathBuf>> for Config {
    type Error = ConfigError;

    fn try_from(value: Vec<PathBuf>) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(ConfigError::NoVaults);
        }
        Ok(Self {
            server: ServerConfig::default(),
            vaults: value.into_iter().map(VaultConfig::from).collect(),
        })
    }
}

/// Environment-provided configuration values captured at process startup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigEnvironment {
    pub config_path: Option<PathBuf>,
    pub token: Option<String>,
    pub log_level: Option<String>,
}

/// Trusted startup sources in documented precedence order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigLoadInput {
    pub cli_config_path: Option<PathBuf>,
    pub default_config_path: Option<PathBuf>,
    pub cli_vaults: Vec<PathBuf>,
    pub cli_log_level: Option<LogLevel>,
    pub environment: ConfigEnvironment,
}

impl TryFrom<ConfigLoadInput> for Config {
    type Error = ConfigError;

    fn try_from(value: ConfigLoadInput) -> Result<Self, Self::Error> {
        let config_path = value
            .cli_config_path
            .or(value.environment.config_path.clone())
            .or(value.default_config_path);
        let mut config = match config_path {
            Some(path) => match fs::read_to_string(&path) {
                Ok(source) => toml::from_str(&source).map_err(|source| ConfigError::Toml {
                    source: Box::new(source),
                })?,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Self::default(),
                Err(source) => return Err(ConfigError::Read { path, source }),
            },
            None => Self::default(),
        };

        if let Some(token) = value.environment.token {
            config.server.http.token = token;
        }
        if let Some(log_level) = value.environment.log_level {
            config.server.log_level = LogLevel::try_from(log_level.as_str())?;
        }
        if let Some(log_level) = value.cli_log_level {
            config.server.log_level = log_level;
        }
        if !value.cli_vaults.is_empty() {
            config.vaults = value.cli_vaults.into_iter().map(VaultConfig::from).collect();
        }

        config.validate()?;
        Ok(config)
    }
}

impl Config {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.vaults.is_empty() {
            return Err(ConfigError::NoVaults);
        }
        if self.server.transports.is_empty() {
            return Err(ConfigError::NoTransports);
        }
        for vault in &self.vaults {
            if vault.limits.max_read_mb == 0 {
                return Err(ConfigError::InvalidLimit {
                    field: "vault.limits.max_read_mb",
                });
            }
            if vault.limits.max_batch_files == 0 {
                return Err(ConfigError::InvalidLimit {
                    field: "vault.limits.max_batch_files",
                });
            }
            if !is_portable_relative_path(&vault.oplog.path) {
                return Err(ConfigError::InvalidRelativePath {
                    field: "vault.oplog.path",
                    path: vault.oplog.path.clone(),
                });
            }
            if let Some(path) = vault
                .git
                .restore_exclude
                .iter()
                .find(|path| !is_portable_relative_path(path))
            {
                return Err(ConfigError::InvalidRelativePath {
                    field: "vault.git.restore_exclude",
                    path: path.clone(),
                });
            }
        }
        Ok(())
    }
}

fn is_portable_relative_path(path: &std::path::Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, std::path::Component::Normal(_))
                && !component.as_os_str().to_string_lossy().contains(['\\', ':'])
        })
}

impl TryFrom<&Config> for VaultSet {
    type Error = ConfigError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let roots = value
            .vaults
            .iter()
            .map(|vault| {
                VaultRoot::try_from(VaultRootInput {
                    path: vault.path.clone(),
                    managed: vault.managed,
                    name: vault.name.clone(),
                })
                .map_err(|source| ConfigError::VaultPath {
                    path: vault.path.clone(),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        VaultSet::try_from(roots).map_err(|source| ConfigError::VaultSet { source })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_transports")]
    pub transports: Vec<Transport>,
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default)]
    pub log_file: String,
    #[serde(default)]
    pub http: HttpConfig,
    /// Size (in KB) at or above which a text-reading tool result attaches
    /// a `resource_link` content block alongside a bounded inline preview,
    /// so a resource-aware host fetches the full content via
    /// `resources/read` instead of exhausting its own inline-result
    /// budget. Below this size, behaviour is unchanged.
    #[serde(default = "default_resource_link_threshold_kb")]
    pub resource_link_threshold_kb: u64,
    /// Registers the `ephemeris_*` tools into this instance's advertised
    /// catalogue. `contextos-ephemeris` and
    /// every ephemeris tool handler are always compiled in regardless of
    /// this flag; it controls runtime visibility only, and can be set here
    /// or overridden per run with `--astro`. Off by default: a niche,
    /// personal-practice capability, kept out of a standard tool list
    /// unless explicitly opted into, the same reasoning `SearchConfig`'s
    /// `semantic` field already applies to a different opt-in capability.
    #[serde(default)]
    pub astro: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transports: default_transports(),
            log_level: LogLevel::default(),
            log_file: String::new(),
            http: HttpConfig::default(),
            resource_link_threshold_kb: default_resource_link_threshold_kb(),
            astro: false,
        }
    }
}

fn default_transports() -> Vec<Transport> {
    vec![Transport::Stdio]
}

const fn default_resource_link_threshold_kb() -> u64 {
    5
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl TryFrom<&str> for LogLevel {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, ConfigError> {
        match value {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(ConfigError::InvalidEnvironmentValue {
                variable: "CONTEXTOS_MCP_LOG_LEVEL",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_http_bind")]
    pub bind: String,
    #[serde(default)]
    pub token: String,
    #[serde(default = "default_max_body_kb")]
    pub max_body_kb: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_http_bind(),
            token: String::new(),
            max_body_kb: default_max_body_kb(),
        }
    }
}

fn default_http_bind() -> String {
    "127.0.0.1:7331".to_owned()
}

const fn default_max_body_kb() -> u64 {
    2048
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    pub path: PathBuf,
    /// Explicit vault name; defaults to the resolved root directory's
    /// basename when unset. Used to address this vault as
    /// `{name}://{relative-path}` instead of an absolute filesystem path,
    /// on every path-accepting tool parameter, on the existing vault
    /// selector parameters, and as the sole scheme `resources/list`,
    /// `resources/read`, and `fs_attach_file` use. Either the explicit
    /// value or the default must be a valid URI scheme token and unique
    /// across the configured vaults, checked at startup with no silent
    /// sanitisation.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub managed: bool,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub index_md: IndexMdConfig,
    #[serde(default)]
    pub oplog: OplogConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub search: SearchConfig,
    /// Path patterns hidden from every enumeration surface for this vault:
    /// `resources/list`, `fs_list_directory`,
    /// `fs_list_directory_with_sizes`, `fs_directory_tree`, and
    /// `fs_search_files`. Separate from `search`'s indexing scope and from
    /// `index_md.exclude` (which governs which directories need a
    /// generated `index.md`, not visibility). Governs omission from
    /// listings only; a direct, explicit-path read is never restricted by
    /// this.
    #[serde(default = "default_hidden_patterns")]
    pub hidden: Vec<String>,
    /// Glob patterns naming which files `resources/list` itself enumerates
    /// for this vault: an allowlist, the opposite of `hidden`'s denylist,
    /// scoped to this one surface only, not
    /// `fs_list_directory`/`fs_directory_tree`/`fs_search_files`. Default
    /// (empty): `resources/list` reports nothing for this vault,
    /// deliberately, since an unconfigured allowlist has no basis for
    /// guessing what is worth surfacing, and dumping every file has little
    /// discovery value once a vault holds thousands of them, the exact
    /// complaint this field exists to fix. Every pattern is still subject
    /// to `hidden`. `resources/read`, `resources/templates/list`, and
    /// every direct-path tool are entirely unaffected: this narrows
    /// autonomous enumeration only, never direct access.
    #[serde(default)]
    pub resources_list_include: Vec<String>,
    /// Override location for this vault's derived state (text index, link
    /// graph cache, and vector store). Default (when unset, `None`): a
    /// platform app-data directory keyed to this vault, so index
    /// segments, lock files, and the vector store never sit inside a
    /// directory a third-party sync tool (for example Obsidian Sync) can
    /// observe or replicate. A relative path here resolves against the
    /// vault root, opting back into pre-existing in-vault `.contextos/`
    /// behaviour; an absolute path is used exactly as given. In-vault
    /// storage is not recommended for a vault under live multi-machine
    /// sync.
    #[serde(default)]
    pub state_directory: Option<PathBuf>,
}

impl From<PathBuf> for VaultConfig {
    fn from(value: PathBuf) -> Self {
        Self {
            path: value,
            name: None,
            managed: true,
            limits: LimitsConfig::default(),
            index_md: IndexMdConfig::default(),
            oplog: OplogConfig::default(),
            git: GitConfig::default(),
            search: SearchConfig::default(),
            hidden: default_hidden_patterns(),
            resources_list_include: Vec::new(),
            state_directory: None,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_read_mb")]
    pub max_read_mb: u64,
    #[serde(default = "default_max_batch_files")]
    pub max_batch_files: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_read_mb: default_max_read_mb(),
            max_batch_files: default_max_batch_files(),
        }
    }
}

const fn default_max_read_mb() -> u64 {
    5
}

const fn default_max_batch_files() -> usize {
    50
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndexMdConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_content_excludes")]
    pub exclude: Vec<String>,
}

impl Default for IndexMdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            exclude: default_content_excludes(),
        }
    }
}

fn default_content_excludes() -> Vec<String> {
    [".contextos", ".git", ".obsidian", "memory/log", "memory/sessions"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OplogConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_oplog_path")]
    pub path: PathBuf,
}

impl Default for OplogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_oplog_path(),
        }
    }
}

fn default_oplog_path() -> PathBuf {
    PathBuf::from("memory/log")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_commit_debounce")]
    pub commit_debounce_s: u64,
    #[serde(default = "default_author_name")]
    pub author_name: String,
    #[serde(default = "default_author_email")]
    pub author_email: String,
    #[serde(default)]
    pub destructive_delete: bool,
    #[serde(default = "default_git_restore_excludes")]
    pub restore_exclude: Vec<PathBuf>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commit_debounce_s: default_commit_debounce(),
            author_name: default_author_name(),
            author_email: default_author_email(),
            destructive_delete: false,
            restore_exclude: default_git_restore_excludes(),
        }
    }
}

fn default_git_restore_excludes() -> Vec<PathBuf> {
    ["memory/log", "memory/sessions", "memory/coding"]
        .map(PathBuf::from)
        .to_vec()
}

const fn default_commit_debounce() -> u64 {
    30
}

fn default_author_name() -> String {
    "Context OS MCP".to_owned()
}

fn default_author_email() -> String {
    "mcp@contextos.local".to_owned()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    #[serde(default = "default_true")]
    pub text: bool,
    #[serde(default = "default_true")]
    pub graph: bool,
    /// The link graph's persistence backend when `graph` is enabled;
    /// ignored otherwise. Additive alongside `graph`
    /// rather than nested under it, so an existing config with `graph =
    /// true` and no `graph_backend` continues to parse unchanged.
    #[serde(default)]
    pub graph_backend: GraphBackendConfig,
    #[serde(default)]
    pub semantic: bool,
    /// Paths excluded from the search corpus (text, graph, and semantic
    /// indexing). Independent of `[vault.index_md] exclude`: that field
    /// scopes managed `index.md` generation and has no bearing on what a
    /// vault's search tools can find.
    #[serde(default = "default_content_excludes")]
    pub exclude: Vec<String>,
    /// Default budget, in seconds, for `query_index_rebuild`'s semantic
    /// phase when a call omits `budget_seconds`: the phase returns early
    /// with partial progress once this elapses, rather than blocking past
    /// a caller's own request timeout. A per-call `budget_seconds` always
    /// overrides this.
    #[serde(default = "default_rebuild_budget_seconds")]
    pub rebuild_budget_seconds: u64,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            text: true,
            graph: true,
            graph_backend: GraphBackendConfig::default(),
            semantic: false,
            exclude: default_content_excludes(),
            rebuild_budget_seconds: default_rebuild_budget_seconds(),
            embedding: EmbeddingConfig::default(),
        }
    }
}

const fn default_rebuild_budget_seconds() -> u64 {
    25
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub provider: EmbeddingProvider,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key_env: String,
    /// Directory holding the local ONNX provider's model and tokenizer
    /// files (platform app-data, never the vault, per
    /// `phase-5-decision-addendum.md` A2). Required when `provider =
    /// "local"` and `[vault.search] semantic = true`; unused otherwise.
    /// Populating this directory (the pre-fetch tool) is separate,
    /// follow-up work: this field only says where to look.
    #[serde(default)]
    pub model_directory: Option<PathBuf>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::Local,
            model: String::new(),
            endpoint: String::new(),
            api_key_env: String::new(),
            model_directory: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingProvider {
    #[default]
    Local,
    OpenaiCompatible,
}

/// TOML-facing counterpart of `contextos_search::GraphBackend`: a closed
/// three-value enum, rejected at config-parse time
/// for any other value, converted to the crate-internal type at the
/// composition-root boundary (`From<GraphBackendConfig> for GraphBackend`
/// below) rather than deriving `Deserialize` on the search crate's own
/// type, matching `EmbeddingProvider`'s precedent of keeping TOML-parsing
/// concerns in the composition root.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphBackendConfig {
    Serde,
    #[default]
    Fjall,
    Sqlite,
}

impl From<GraphBackendConfig> for GraphBackend {
    fn from(value: GraphBackendConfig) -> Self {
        match value {
            GraphBackendConfig::Serde => Self::Serde,
            GraphBackendConfig::Fjall => Self::Fjall,
            GraphBackendConfig::Sqlite => Self::Sqlite,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("configuration file could not be read: {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("environment variable {variable} has an invalid value: {value}")]
    InvalidEnvironmentValue { variable: &'static str, value: String },
    #[error("at least one allowed vault directory must be configured")]
    NoVaults,
    #[error("at least one transport (\"stdio\" or \"http\") must be configured")]
    NoTransports,
    #[error("configuration limit must be greater than zero: {field}")]
    InvalidLimit { field: &'static str },
    #[error("configuration path must be a portable relative path: {field} = {path}")]
    InvalidRelativePath { field: &'static str, path: PathBuf },
    #[error("vault path is invalid: {path}")]
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
