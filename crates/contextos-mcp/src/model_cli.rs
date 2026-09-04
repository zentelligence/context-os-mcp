//! `contextos model` CLI support: reports on and fetches the shared local
//! embedding model for `[vault.search.embedding] provider = "local"`.
//!
//! Downloads go straight through the `hf-hub` crate into
//! `$HOME/.fastembed_cache`, the same cache directory and on-disk layout
//! fastembed's own (unused-here) `hf-hub` feature would produce. mempalace-rs
//! downloads the same model into the same directory, so a model fetched by
//! either application is reused by the other.
//!
//! This module never touches `contextos-search`: `FastembedLocal` still only
//! ever loads an already-populated, explicitly configured `model_directory`
//! (phase-5-decision-addendum.md A1). `contextos model download` is a
//! separate, explicitly user-invoked utility, never part of MCP tool
//! dispatch or the search runtime path.
//!
//! [`ModelReport::list`] and [`ModelReport::download`] take the cache
//! directory as a parameter rather than resolving it internally, so tests
//! never depend on the operator's actual home directory; [`default_model_cache_dir`]
//! is the sole home-directory-resolving entry point, called once by the CLI.

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};

use contextos_search::REQUIRED_MODEL_FILES;
use directories::BaseDirs;
use hf_hub::api::sync::ApiBuilder;
use hf_hub::{Cache, CacheRepo};
use thiserror::Error;

/// `HuggingFace` repository for `ContextOS`'s default local embedding
/// model: the repository fastembed's `EmbeddingModel::AllMiniLML6V2`
/// resolves to, and the model mempalace-rs downloads by default.
/// 384-dimensional ONNX sentence-transformers MiniLM-L6-v2.
pub const DEFAULT_MODEL_REPO: &str = "Qdrant/all-MiniLM-L6-v2-onnx";
pub const DEFAULT_MODEL_DIMENSION: usize = 384;

/// Directory name, relative to the operator's home directory, used as the
/// shared fastembed model cache -- mempalace-rs's convention, so a model
/// downloaded by either application is reused by the other.
const CACHE_DIR_NAME: &str = ".fastembed_cache";

/// Resolves the shared model cache directory: `$HOME/.fastembed_cache`.
///
/// # Errors
///
/// Returns [`ModelCliError::HomeDirectoryUnavailable`] when the operator's
/// home directory cannot be determined.
pub fn default_model_cache_dir() -> Result<PathBuf, ModelCliError> {
    let base_dirs = BaseDirs::new().ok_or(ModelCliError::HomeDirectoryUnavailable)?;
    Ok(base_dirs.home_dir().join(CACHE_DIR_NAME))
}

/// Returns the shared snapshot directory for the default model if every
/// required file is already present in `cache_dir`, without any network
/// access.
fn cached_directory(repo: &CacheRepo) -> Option<PathBuf> {
    let mut directory = None;
    for file in REQUIRED_MODEL_FILES {
        let path = repo.get(file)?;
        directory.get_or_insert_with(|| {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        });
    }
    directory
}

/// Resolves an operator's `[vault.search.embedding] model_directory` to the
/// flat directory `FastembedLocal` actually reads from.
///
/// `FastembedLocal` only ever reads a flat directory containing every
/// required file directly; it never calls this function or touches
/// `hf-hub` itself, so `phase-5-decision-addendum.md` A1's guarantee for
/// the search runtime path is unaffected. When `configured` already
/// contains every required file, it is returned unchanged -- today's
/// behaviour. Otherwise `configured` is tried as an hf-hub cache root (for
/// example the shared `$HOME/.fastembed_cache` `contextos model download`
/// populates): if the default model's snapshot is cached there, that
/// resolved directory is returned instead, so an operator can point
/// `model_directory` at the shared cache root directly rather than an
/// opaque revisioned subdirectory. Local filesystem reads only, never the
/// network.
#[must_use]
pub(crate) fn resolve_model_directory(configured: &Path) -> PathBuf {
    let is_flat_model_directory = REQUIRED_MODEL_FILES
        .iter()
        .all(|file| configured.join(file).is_file());
    if is_flat_model_directory {
        return configured.to_path_buf();
    }

    let repo = Cache::new(configured.to_path_buf()).model(DEFAULT_MODEL_REPO.to_owned());
    cached_directory(&repo).unwrap_or_else(|| configured.to_path_buf())
}

/// The result of a `contextos model` subcommand: a short, human-readable
/// report written verbatim to stdout by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelReport {
    lines: Vec<String>,
}

impl ModelReport {
    /// Reports the default model's cache status within `cache_dir`. Never
    /// touches the network.
    #[must_use]
    pub fn list(cache_dir: &Path) -> Self {
        let repo = Cache::new(cache_dir.to_path_buf()).model(DEFAULT_MODEL_REPO.to_owned());

        let mut lines = vec![
            format!(
                "Default local embedding model: {DEFAULT_MODEL_REPO} ({DEFAULT_MODEL_DIMENSION} dimensions)"
            ),
            format!("Cache directory: {}", cache_dir.display()),
        ];

        if let Some(directory) = cached_directory(&repo) {
            lines.push(format!("Status: downloaded ({})", directory.display()));
            lines.push(model_directory_hint(&directory));
        } else {
            lines.push("Status: not downloaded".to_owned());
            lines.push("Run `contextos model download` to fetch it.".to_owned());
        }

        Self { lines }
    }

    /// Downloads every required file for the default model into
    /// `cache_dir`, reusing anything already present. Performs blocking
    /// network I/O; callers run this inside `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns [`ModelCliError::NoRequiredFiles`] if the required file list
    /// is empty, or [`ModelCliError::Api`] when the download itself fails.
    pub fn download(cache_dir: &Path) -> Result<Self, ModelCliError> {
        let directory = download_default_model(cache_dir)?;

        Ok(Self {
            lines: vec![
                format!(
                    "Downloaded {DEFAULT_MODEL_REPO} ({DEFAULT_MODEL_DIMENSION} dimensions) to {}",
                    directory.display()
                ),
                model_directory_hint(&directory),
            ],
        })
    }
}

/// Downloads every required file for the default model into `cache_dir`,
/// reusing anything already present, and returns the resolved flat snapshot
/// directory `FastembedLocal` can read from directly. The shared
/// implementation behind both [`ModelReport::download`] (`contextos model
/// download`) and the `contextos config` interview wizard's "download the
/// local embedding model now" step, so both entry points fetch the
/// identical model the identical way. Performs blocking network I/O;
/// callers run this inside `spawn_blocking`.
///
/// # Errors
///
/// Returns [`ModelCliError::NoRequiredFiles`] if the required file list is
/// empty, or [`ModelCliError::Api`] when the download itself fails.
pub fn download_default_model(cache_dir: &Path) -> Result<PathBuf, ModelCliError> {
    let api = ApiBuilder::from_cache(Cache::new(cache_dir.to_path_buf()))
        .with_progress(true)
        .build()?;
    let repo = api.model(DEFAULT_MODEL_REPO.to_owned());

    let mut directory: Option<PathBuf> = None;
    for file in REQUIRED_MODEL_FILES {
        let path = repo.get(file)?;
        directory.get_or_insert_with(|| {
            path.parent()
                .map_or_else(|| path.clone(), Path::to_path_buf)
        });
    }
    directory.ok_or(ModelCliError::NoRequiredFiles)
}

fn model_directory_hint(directory: &Path) -> String {
    format!(
        "Use as [vault.search.embedding] model_directory = \"{}\"",
        directory.display()
    )
}

impl Display for ModelReport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "ContextOS MCP model")?;
        for line in &self.lines {
            writeln!(formatter, "{line}")?;
        }
        Ok(())
    }
}

/// Typed failures for `contextos model` subcommands.
#[derive(Debug, Error)]
pub enum ModelCliError {
    #[error("the operator's home directory could not be determined")]
    HomeDirectoryUnavailable,
    #[error("the default model has no required files configured")]
    NoRequiredFiles,
    #[error("model download failed: {0}")]
    Api(#[from] hf_hub::api::sync::ApiError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use contextos_search::REQUIRED_MODEL_FILES;

    /// Mirrors hf-hub's on-disk layout closely enough for `CacheRepo::get`
    /// to resolve: `<cache>/models--org--repo/snapshots/<rev>/<file>`, with
    /// `refs/main` pointing at `<rev>`.
    fn populated_snapshot_dir(cache_dir: &Path) -> Result<PathBuf, std::io::Error> {
        let repo_dir = cache_dir.join("models--Qdrant--all-MiniLM-L6-v2-onnx");
        let snapshot_dir = repo_dir.join("snapshots").join("test-revision");
        std::fs::create_dir_all(&snapshot_dir)?;
        let refs_dir = repo_dir.join("refs");
        std::fs::create_dir_all(&refs_dir)?;
        std::fs::write(refs_dir.join("main"), "test-revision")?;
        for file in REQUIRED_MODEL_FILES {
            std::fs::write(snapshot_dir.join(file), b"fixture")?;
        }
        Ok(snapshot_dir)
    }

    #[test]
    fn list_reports_not_downloaded_for_an_empty_cache() -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;

        let rendered = ModelReport::list(cache.path()).to_string();

        assert!(rendered.contains("Status: not downloaded"));
        assert!(rendered.contains(DEFAULT_MODEL_REPO));
        assert!(rendered.contains("contextos model download"));
        Ok(())
    }

    #[test]
    fn list_reports_downloaded_when_every_required_file_is_present()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let snapshot_dir = populated_snapshot_dir(cache.path())?;

        let rendered = ModelReport::list(cache.path()).to_string();

        assert!(rendered.contains("Status: downloaded"));
        assert!(rendered.contains(&snapshot_dir.display().to_string()));
        assert!(rendered.contains("model_directory"));
        Ok(())
    }

    #[test]
    fn list_reports_not_downloaded_when_a_required_file_is_missing()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let snapshot_dir = populated_snapshot_dir(cache.path())?;
        std::fs::remove_file(snapshot_dir.join("tokenizer.json"))?;

        let rendered = ModelReport::list(cache.path()).to_string();

        assert!(rendered.contains("Status: not downloaded"));
        Ok(())
    }

    #[test]
    fn default_model_cache_dir_is_named_fastembed_cache() -> Result<(), Box<dyn std::error::Error>>
    {
        let resolved = default_model_cache_dir()?;

        assert_eq!(
            resolved.file_name(),
            Some(std::ffi::OsStr::new(".fastembed_cache"))
        );
        Ok(())
    }

    #[test]
    fn resolve_model_directory_passes_through_an_already_flat_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let flat = tempfile::tempdir()?;
        for file in REQUIRED_MODEL_FILES {
            std::fs::write(flat.path().join(file), b"fixture")?;
        }

        let resolved = resolve_model_directory(flat.path());

        assert_eq!(resolved, flat.path());
        Ok(())
    }

    #[test]
    fn resolve_model_directory_resolves_a_shared_cache_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let snapshot_dir = populated_snapshot_dir(cache.path())?;

        let resolved = resolve_model_directory(cache.path());

        assert_eq!(resolved, snapshot_dir);
        Ok(())
    }

    #[test]
    fn resolve_model_directory_passes_through_an_unresolvable_directory_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let empty = tempfile::tempdir()?;

        let resolved = resolve_model_directory(empty.path());

        assert_eq!(resolved, empty.path());
        Ok(())
    }
}
