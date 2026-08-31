#![forbid(unsafe_code)]

mod config;
mod config_io;
mod config_writer;
mod doctor;
mod host_paths;
mod host_registration;
mod http;
mod index_cli;
mod interview;
mod model_cli;
mod module;
mod resource_support;
mod resources;
mod semantic_drain;
mod server;
mod state_dir;
mod tool_error;
mod tools;

pub use config::{
    Config, ConfigEnvironment, ConfigError, ConfigLoadInput, EmbeddingConfig, EmbeddingProvider,
    GitConfig, GraphBackendConfig, HttpConfig, IndexMdConfig, LimitsConfig, LogLevel, OplogConfig,
    SearchConfig, ServerConfig, Transport, VaultConfig,
};
pub use config_io::{ConfigIoError, load_config_document, write_config_document};
pub use config_writer::{ConfigDocument, ConfigWriterError, ServerSettingsSummary, VaultSummary};
pub use doctor::{DoctorError, DoctorReport};
pub use host_paths::{
    HostPathError, HostPathResolution, default_claude_desktop_config_path,
    resolve_linux_config_path, resolve_macos_config_path, resolve_windows_config_path,
    resolve_windows_config_paths, resolve_windows_roaming_config_path,
};
pub use host_registration::{
    DeregisterOutcome, DetectsRunningProcesses, HostRegistrationError, RegisteredServer,
    RegistrationStatus, SystemProcessDetector, deregister, is_claude_desktop_running, register,
    status,
};
pub use http::{HttpTransportError, MOUNT_PATH as HTTP_MOUNT_PATH, build_router, validate_bind};
pub use index_cli::{IndexCliError, IndexReport};
pub use interview::{
    HostRegistrationOutcome, InterviewEnvironment, InterviewError, InterviewReport, Interviewer,
    TerminalInterviewer, run_interview,
};
pub use model_cli::{ModelCliError, ModelReport, default_model_cache_dir, download_default_model};
pub use module::{
    ModuleCall, ModuleContext, ModuleContextError, ModuleManifest, ModuleNamespace, ModuleRegistry,
    ModuleRegistryError, ServerModule, ServerModuleFuture,
};
pub use server::{ContextOsServer, ServerBuildConfig, ServerBuildError};
pub use state_dir::StateDirError;
pub use tools::doctor::{DoctorResolveOutcome, resolve_for_cli};
