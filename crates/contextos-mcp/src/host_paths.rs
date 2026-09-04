//! Resolves Claude Desktop's `claude_desktop_config.json` location for
//! `contextos config mcp`.
//!
//! Every resolver here takes its platform root directories as parameters
//! rather than reading the environment or the operator's home directory
//! directly, mirroring `model_cli.rs`'s existing "take the directory as a
//! parameter; one `default_*` wrapper is the sole home-dir-resolving entry
//! point" precedent, so discovery stays unit-testable on any host: every
//! resolver reports `Found`, `NotFound`, or `Ambiguous` against injected
//! platform roots, with no reliance on the real filesystem.

use std::fs;
use std::path::{Path, PathBuf};

use directories::BaseDirs;
use thiserror::Error;

/// Where Claude Desktop's config file lives, relative to a macOS home
/// directory.
const MACOS_RELATIVE_PATH: &str = "Library/Application Support/Claude/claude_desktop_config.json";

/// The Windows MSIX per-publisher package-folder name prefix every genuine
/// Claude Desktop install uses.
const WINDOWS_PACKAGE_NAME_PREFIX: &str = "Claude_";

/// The subpath a candidate Windows package folder must contain, in addition
/// to the name prefix, before it is trusted: filters stale leftover
/// package folders a name-prefix match alone would false-positive on.
const WINDOWS_CONFIG_SUBPATH: &str = "LocalCache/Roaming/Claude/claude_desktop_config.json";

/// Where Claude Desktop's config file lives for a plain (non-MSIX) Windows
/// install, relative to `%APPDATA%` (`{FOLDERID_RoamingAppData}`). Operator
/// hardware observed this location in use alongside, and independently of,
/// the randomly-named MSIX package folder the `Packages` scan below finds,
/// so both must be checked before a Windows host is reported not found.
const WINDOWS_ROAMING_RELATIVE_PATH: &str = "Claude/claude_desktop_config.json";

/// The outcome of resolving Claude Desktop's config-file location: a single
/// trusted path, an explained absence, or more than one candidate that must
/// never be silently guessed between.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostPathResolution {
    Found(PathBuf),
    NotFound { reason: String },
    Ambiguous { candidates: Vec<PathBuf> },
}

/// Resolves Claude Desktop's config path under a macOS-style layout: a
/// fixed location under the home directory. `Found` only when the
/// directory it lives in exists (proof this is a real install, not a guess
/// at a path that has never existed), even though the config file itself
/// may not exist yet (`register` on a fresh install starts from empty).
#[must_use]
pub fn resolve_macos_config_path(home_dir: &Path) -> HostPathResolution {
    let path = home_dir.join(MACOS_RELATIVE_PATH);
    match path.parent() {
        Some(parent) if parent.is_dir() => HostPathResolution::Found(path),
        _ => HostPathResolution::NotFound {
            reason: format!(
                "no Claude Desktop install found at {} (its containing directory does not exist)",
                path.display()
            ),
        },
    }
}

/// Resolves Claude Desktop's config path under the Windows MSIX layout:
/// `packages_dir` is `%LOCALAPPDATA%\Packages`. A candidate package folder
/// must match both the `Claude_` name prefix and have the real
/// `LocalCache\Roaming\Claude` subpath present; zero or more than
/// one match after that filter is reported plainly, never guessed.
///
/// # Errors
///
/// Returns [`HostPathError::ReadPackagesDirectory`] when `packages_dir`
/// exists but cannot be enumerated (a permissions failure, for example);
/// a missing `packages_dir` itself is a normal [`HostPathResolution::NotFound`],
/// not an error.
pub fn resolve_windows_config_path(packages_dir: &Path) -> Result<HostPathResolution, HostPathError> {
    let entries = match fs::read_dir(packages_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostPathResolution::NotFound {
                reason: format!("no Windows Packages directory found at {}", packages_dir.display()),
            });
        }
        Err(source) => {
            return Err(HostPathError::ReadPackagesDirectory {
                path: packages_dir.to_path_buf(),
                source,
            });
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| HostPathError::ReadPackagesDirectory {
            path: packages_dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(WINDOWS_PACKAGE_NAME_PREFIX) {
            continue;
        }
        let candidate = entry.path().join(WINDOWS_CONFIG_SUBPATH);
        let Some(subpath_dir) = candidate.parent() else {
            continue;
        };
        if subpath_dir.is_dir() {
            candidates.push(candidate);
        }
    }

    match candidates.as_slice() {
        [] => Ok(HostPathResolution::NotFound {
            reason: format!(
                "no Claude Desktop package folder (matching \"{WINDOWS_PACKAGE_NAME_PREFIX}*\" \
                 with a real \"{WINDOWS_CONFIG_SUBPATH}\" subpath) found under {}",
                packages_dir.display()
            ),
        }),
        [only] => Ok(HostPathResolution::Found(only.clone())),
        _ => Ok(HostPathResolution::Ambiguous { candidates }),
    }
}

/// Resolves Claude Desktop's config path under the plain (non-MSIX) Windows
/// layout: a fixed location under `%APPDATA%`, the same "containing
/// directory must exist" proof-of-a-real-install rule
/// [`resolve_macos_config_path`] uses.
#[must_use]
pub fn resolve_windows_roaming_config_path(roaming_dir: &Path) -> HostPathResolution {
    let path = roaming_dir.join(WINDOWS_ROAMING_RELATIVE_PATH);
    match path.parent() {
        Some(parent) if parent.is_dir() => HostPathResolution::Found(path),
        _ => HostPathResolution::NotFound {
            reason: format!(
                "no Claude Desktop install found at {} (its containing directory does not exist)",
                path.display()
            ),
        },
    }
}

/// Resolves Claude Desktop's config path on Windows by checking both known
/// layouts, the MSIX `packages_dir` scan and the plain `roaming_dir`
/// location, before reporting not found: operator hardware has been seen
/// running a plain install under `roaming_dir` with no MSIX package folder
/// present at all, so checking only one layout false-negatives on the
/// other. A candidate found under either layout is reported as normal;
/// candidates found under both are reported [`HostPathResolution::Ambiguous`]
/// rather than guessed between.
///
/// # Errors
///
/// Propagates [`HostPathError::ReadPackagesDirectory`] from the underlying
/// `packages_dir` scan (see [`resolve_windows_config_path`]).
pub fn resolve_windows_config_paths(
    packages_dir: &Path,
    roaming_dir: &Path,
) -> Result<HostPathResolution, HostPathError> {
    let mut candidates = Vec::new();
    match resolve_windows_config_path(packages_dir)? {
        HostPathResolution::Found(path) => candidates.push(path),
        HostPathResolution::Ambiguous { candidates: more } => candidates.extend(more),
        HostPathResolution::NotFound { .. } => {}
    }
    if let HostPathResolution::Found(path) = resolve_windows_roaming_config_path(roaming_dir) {
        candidates.push(path);
    }

    match candidates.as_slice() {
        [] => Ok(HostPathResolution::NotFound {
            reason: format!(
                "no Claude Desktop install found under the Windows Packages directory ({}) or \
                 the Roaming AppData directory ({})",
                packages_dir.display(),
                roaming_dir.display()
            ),
        }),
        [only] => Ok(HostPathResolution::Found(only.clone())),
        _ => Ok(HostPathResolution::Ambiguous { candidates }),
    }
}

/// Linux has no known Claude Desktop install location; always reported
/// plainly rather than guessed.
#[must_use]
pub fn resolve_linux_config_path() -> HostPathResolution {
    HostPathResolution::NotFound {
        reason: "no known Claude Desktop install location on Linux".to_owned(),
    }
}

/// Resolves Claude Desktop's config path for the platform this binary is
/// actually running on, using the operator's real home/local-app-data
/// directories. The sole home-directory-resolving entry point for this
/// module; every other function here takes its roots as parameters so
/// tests never depend on the operator's actual home directory.
///
/// # Errors
///
/// Returns [`HostPathError::HomeDirectoryUnavailable`] when the operator's
/// home directory cannot be determined, and propagates
/// [`HostPathError::ReadPackagesDirectory`] on Windows.
pub fn default_claude_desktop_config_path() -> Result<HostPathResolution, HostPathError> {
    let base_dirs = BaseDirs::new().ok_or(HostPathError::HomeDirectoryUnavailable)?;
    if cfg!(target_os = "macos") {
        Ok(resolve_macos_config_path(base_dirs.home_dir()))
    } else if cfg!(target_os = "windows") {
        resolve_windows_config_paths(&base_dirs.data_local_dir().join("Packages"), base_dirs.config_dir())
    } else {
        Ok(resolve_linux_config_path())
    }
}

/// Typed failures resolving Claude Desktop's config-file location.
#[derive(Debug, Error)]
pub enum HostPathError {
    #[error("could not determine the operator's home directory")]
    HomeDirectoryUnavailable,
    #[error("could not read the Windows Packages directory at {}: {source}", path.display())]
    ReadPackagesDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
#[path = "host_paths_test.rs"]
mod tests;
