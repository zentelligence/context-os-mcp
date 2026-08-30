use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifies an allowed vault root without exposing its filesystem path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct VaultRootId(u32);

impl TryFrom<usize> for VaultRootId {
    type Error = PathError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Ok(Self(
            u32::try_from(value).map_err(|_| PathError::TooManyRoots { count: value })?,
        ))
    }
}

impl TryFrom<VaultRootId> for usize {
    type Error = PathError;

    fn try_from(value: VaultRootId) -> Result<Self, Self::Error> {
        usize::try_from(value.0).map_err(|_| PathError::TooManyRoots { count: usize::MAX })
    }
}

/// An allowed, resolved filesystem root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRoot {
    resolved: Utf8PathBuf,
    managed: bool,
    name: String,
}

impl TryFrom<PathBuf> for VaultRoot {
    type Error = PathError;

    fn try_from(value: PathBuf) -> Result<Self, Self::Error> {
        Self::try_from(VaultRootInput {
            path: value,
            managed: true,
            name: None,
        })
    }
}

/// Trusted construction input for an allowed vault root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultRootInput {
    pub path: PathBuf,
    pub managed: bool,
    /// Explicit vault name; `None` defaults to the resolved root
    /// directory's basename. Either the explicit value or the default must
    /// be a valid URI scheme token, checked once resolution is complete so
    /// the default reflects the actual directory on disk.
    pub name: Option<String>,
}

impl TryFrom<VaultRootInput> for VaultRoot {
    type Error = PathError;

    fn try_from(value: VaultRootInput) -> Result<Self, Self::Error> {
        let resolved =
            dunce::canonicalize(&value.path).map_err(|source| PathError::RootResolution {
                path: value.path.clone(),
                source,
            })?;
        if !resolved.is_dir() {
            return Err(PathError::RootNotDirectory { path: resolved });
        }
        let resolved =
            Utf8PathBuf::from_path_buf(resolved).map_err(|path| PathError::NonUtf8 { path })?;
        let name = match value.name {
            Some(name) => name,
            None => resolved
                .file_name()
                .ok_or_else(|| PathError::InvalidName {
                    path: value.path.clone(),
                    name: String::new(),
                })?
                .to_owned(),
        };
        if !is_valid_scheme_token(&name) {
            return Err(PathError::InvalidName {
                path: value.path.clone(),
                name,
            });
        }
        Ok(Self {
            resolved,
            managed: value.managed,
            name,
        })
    }
}

impl VaultRoot {
    /// Returns the resolved allowed directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.resolved.as_std_path()
    }

    /// Reports whether `ContextOS` substrate services are enabled for this root.
    #[must_use]
    pub const fn managed(&self) -> bool {
        self.managed
    }

    /// Returns this vault's configured or default-derived name, a
    /// valid URI scheme token used to address it as `{name}://{relative-path}`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Reports whether `name` is a valid URI scheme token per RFC 3986 §3.1:
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
fn is_valid_scheme_token(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

/// The complete set of roots from trusted startup configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSet {
    roots: Vec<VaultRoot>,
}

impl TryFrom<Vec<VaultRoot>> for VaultSet {
    type Error = PathError;

    fn try_from(value: Vec<VaultRoot>) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(PathError::NoRoots);
        }
        if u32::try_from(value.len()).is_err() {
            return Err(PathError::TooManyRoots { count: value.len() });
        }

        let mut unique_paths = HashSet::with_capacity(value.len());
        let mut unique_names = HashSet::with_capacity(value.len());
        for root in &value {
            if !unique_paths.insert(root.resolved.clone()) {
                return Err(PathError::DuplicateRoot {
                    path: root.resolved.clone().into_std_path_buf(),
                });
            }
            if !unique_names.insert(root.name.to_ascii_lowercase()) {
                return Err(PathError::DuplicateName {
                    name: root.name.clone(),
                });
            }
        }

        Ok(Self { roots: value })
    }
}

impl VaultSet {
    /// Returns the number of configured roots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Reports whether no roots are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Iterates over configured roots in stable configuration order.
    pub fn iter(&self) -> std::slice::Iter<'_, VaultRoot> {
        self.roots.iter()
    }

    /// Returns the configured root identified by `id`. Used to resolve a
    /// `VaultPath`'s owning root back to its name in order to build a
    /// `{name}://{relative-path}` URI.
    #[must_use]
    pub fn root(&self, id: VaultRootId) -> Option<&VaultRoot> {
        let index = usize::try_from(id).ok()?;
        self.roots.get(index)
    }

    /// Finds a configured root by its vault name, compared
    /// case-insensitively (URI scheme comparison is case-insensitive per
    /// RFC 3986 §3.1, and the `url` crate normalises a parsed scheme to
    /// lowercase).
    #[must_use]
    pub fn root_by_name(&self, name: &str) -> Option<(VaultRootId, &VaultRoot)> {
        self.roots
            .iter()
            .position(|root| root.name.eq_ignore_ascii_case(name))
            .and_then(|index| {
                let id = VaultRootId::try_from(index).ok()?;
                self.roots.get(index).map(|root| (id, root))
            })
    }

    /// Confirms that a validated path belongs to this configured root set.
    #[must_use]
    pub fn authorises(&self, path: &VaultPath) -> bool {
        let Ok(index) = usize::try_from(path.root.0) else {
            return false;
        };
        let Some(root) = self.roots.get(index) else {
            return false;
        };
        path.absolute.starts_with(&root.resolved)
            && path.absolute == root.resolved.join(&path.relative)
    }
}

impl<'a> IntoIterator for &'a VaultSet {
    type Item = &'a VaultRoot;
    type IntoIter = std::slice::Iter<'a, VaultRoot>;

    fn into_iter(self) -> Self::IntoIter {
        self.roots.iter()
    }
}

/// Borrowed boundary input for validating an untrusted tool path.
#[derive(Clone, Copy, Debug)]
pub struct VaultPathInput<'a> {
    pub roots: &'a VaultSet,
    pub raw: &'a str,
}

/// A path proven to resolve within one configured vault root.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct VaultPath {
    root: VaultRootId,
    relative: Utf8PathBuf,
    absolute: Utf8PathBuf,
}

impl VaultPath {
    /// Returns the root identity selected during validation.
    #[must_use]
    pub const fn root_id(&self) -> VaultRootId {
        self.root
    }

    /// Returns the path relative to its selected vault root.
    #[must_use]
    pub fn relative(&self) -> &Path {
        self.relative.as_std_path()
    }

    /// Resolves a vault-selector parameter: a parameter whose
    /// whole purpose is choosing a vault, such as the Git tools' `vault`
    /// field or `vault_index_rebuild`'s/`doctor_resolve`'s `path` field
    /// when used to mean "this whole vault" rather than a subtree. When
    /// `raw` exactly matches (case-insensitively) a configured vault's
    /// name, it selects that vault's root directly, equivalent to
    /// `{raw}://.`; otherwise `raw` is resolved via the normal
    /// `path`-parameter rules, unchanged.
    ///
    /// Deliberately a separate entry point from
    /// `TryFrom<VaultPathInput>`, not a branch folded into it: a bare-name
    /// shortcut is unambiguous for a parameter whose only job is picking a
    /// vault, but would be a genuine footgun on an ordinary `path`
    /// parameter, where a relative path or filename could legitimately
    /// share a configured vault's name. Callers opt in per parameter
    /// rather than this being silently universal.
    ///
    /// # Errors
    ///
    /// Returns the same typed path-resolution errors `TryFrom<VaultPathInput>`
    /// does.
    pub fn try_from_vault_selector(roots: &VaultSet, raw: &str) -> Result<Self, PathError> {
        if roots.root_by_name(raw).is_some() {
            let prefixed = format!("{raw}://.");
            return Self::try_from(VaultPathInput {
                roots,
                raw: &prefixed,
            });
        }
        Self::try_from(VaultPathInput { roots, raw })
    }
}

impl<'a> From<&'a VaultPath> for &'a Path {
    fn from(value: &'a VaultPath) -> Self {
        value.absolute.as_std_path()
    }
}

impl From<VaultPath> for PathBuf {
    fn from(value: VaultPath) -> Self {
        value.absolute.into_std_path_buf()
    }
}

impl TryFrom<VaultPathInput<'_>> for VaultPath {
    type Error = PathError;

    fn try_from(value: VaultPathInput<'_>) -> Result<Self, Self::Error> {
        if let Some((root_index, remainder)) = named_prefix(value.raw, value.roots)? {
            return Self::resolve_within_named_root(value.roots, root_index, remainder);
        }

        validate_lexical_input(value.raw)?;

        let supplied = Path::new(value.raw);
        let (candidate, lexical_root) = if supplied.is_absolute() {
            let lexical_root = value
                .roots
                .roots
                .iter()
                .position(|root| supplied.starts_with(root.resolved.as_std_path()));
            (supplied.to_path_buf(), lexical_root)
        } else if value.roots.roots.len() == 1 {
            (
                value.roots.roots[0].resolved.as_std_path().join(supplied),
                Some(0),
            )
        } else {
            return Err(PathError::AmbiguousRoot {
                path: supplied.to_path_buf(),
            });
        };

        Self::finish(value.roots, candidate, lexical_root, None)
    }
}

impl VaultPath {
    /// Resolves `remainder` (the part of a `{name}://{relative-path}` input
    /// after the prefix) against exactly the named root: an
    /// absolute `remainder` must still fall within that one root, never any
    /// other configured vault, so a name prefix never silently redirects.
    fn resolve_within_named_root(
        roots: &VaultSet,
        root_index: usize,
        remainder: &str,
    ) -> Result<Self, PathError> {
        if remainder.is_empty() {
            return Err(PathError::EmptyNamedPrefixRemainder {
                name: roots.roots[root_index].name().to_owned(),
            });
        }
        validate_lexical_input(remainder)?;

        let root = &roots.roots[root_index];
        let supplied = Path::new(remainder);
        let (candidate, lexical_root) = if supplied.is_absolute() {
            let lexical_root = supplied
                .starts_with(root.resolved.as_std_path())
                .then_some(root_index);
            (supplied.to_path_buf(), lexical_root)
        } else {
            (root.resolved.as_std_path().join(supplied), Some(root_index))
        };

        Self::finish(roots, candidate, lexical_root, Some(root_index))
    }

    /// Shared confinement, resolution, and construction, once a candidate
    /// absolute path and its lexical root guess (used only to distinguish a
    /// symlink escape from a path that was never inside any root) are known.
    /// `restrict_to`, when set, requires the resolved path fall within that
    /// one root specifically rather than any configured root (the
    /// `{name}://{relative-path}` named-prefix form).
    fn finish(
        roots: &VaultSet,
        candidate: PathBuf,
        lexical_root: Option<usize>,
        restrict_to: Option<usize>,
    ) -> Result<Self, PathError> {
        let resolved = resolve_with_missing_suffix(&candidate)?;
        let selected = roots
            .roots
            .iter()
            .enumerate()
            .filter(|(index, _)| restrict_to.is_none_or(|only| *index == only))
            .find(|(_, root)| resolved.starts_with(root.resolved.as_std_path()));

        let Some((root_index, root)) = selected else {
            return if lexical_root.is_some() {
                Err(PathError::SymlinkEscape { path: candidate })
            } else {
                Err(PathError::OutsideRoot { path: candidate })
            };
        };

        let relative = resolved
            .strip_prefix(root.resolved.as_std_path())
            .map_err(|source| PathError::PrefixMismatch {
                path: resolved.clone(),
                source,
            })?
            .to_path_buf();
        let relative =
            Utf8PathBuf::from_path_buf(relative).map_err(|path| PathError::NonUtf8 { path })?;
        let absolute =
            Utf8PathBuf::from_path_buf(resolved).map_err(|path| PathError::NonUtf8 { path })?;
        let root = VaultRootId::try_from(root_index)?;

        Ok(Self {
            root,
            relative,
            absolute,
        })
    }
}

/// Detects a `{name}://{relative-path}` prefix and looks up the
/// named root, deliberately without running [`validate_lexical_input`]
/// against the raw input first: that check's alternate-data-stream rule
/// would otherwise reject any `:` outside a Windows drive-letter position,
/// misclassifying every valid name prefix before this ever ran. Returns the
/// matched root's index and the still-unvalidated remainder after `://`;
/// `Ok(None)` when the input does not have this shape at all, so normal
/// absolute/relative resolution proceeds unchanged.
fn named_prefix<'a>(raw: &'a str, roots: &VaultSet) -> Result<Option<(usize, &'a str)>, PathError> {
    let Some((candidate_name, remainder)) = raw.split_once("://") else {
        return Ok(None);
    };
    if !is_valid_scheme_token(candidate_name) {
        return Ok(None);
    }
    match roots.root_by_name(candidate_name) {
        Some((id, _)) => Ok(Some((usize::try_from(id)?, remainder))),
        None => Err(PathError::UnknownVaultName {
            name: candidate_name.to_owned(),
        }),
    }
}

fn validate_lexical_input(raw: &str) -> Result<(), PathError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(PathError::Invalid { path: raw.into() });
    }
    if Path::new(raw)
        .components()
        .any(|component| component == Component::ParentDir)
        || raw.split(['/', '\\']).any(|component| component == "..")
    {
        return Err(PathError::Traversal { path: raw.into() });
    }

    validate_windows_input(raw)?;
    Ok(())
}

fn validate_windows_input(raw: &str) -> Result<(), PathError> {
    if raw.starts_with(r"\\?\") || raw.starts_with(r"\\.\") {
        return Err(PathError::WindowsVerbatim { path: raw.into() });
    }

    let bytes = raw.as_bytes();
    #[cfg(not(windows))]
    if raw.starts_with(r"\\")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(PathError::UnsupportedWindowsPath { path: raw.into() });
    }
    let without_drive = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        &raw[2..]
    } else {
        raw
    };
    if without_drive.contains(':') {
        return Err(PathError::AlternateDataStream { path: raw.into() });
    }
    Ok(())
}

fn resolve_with_missing_suffix(candidate: &Path) -> Result<PathBuf, PathError> {
    let mut ancestor = candidate;
    let mut suffix = Vec::<OsString>::new();

    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    return Err(PathError::OutsideRoot {
                        path: candidate.to_path_buf(),
                    });
                };
                suffix.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    return Err(PathError::OutsideRoot {
                        path: candidate.to_path_buf(),
                    });
                };
                ancestor = parent;
            }
            Err(source) => {
                return Err(PathError::PathInspection {
                    path: ancestor.to_path_buf(),
                    source,
                });
            }
        }
    }

    let mut resolved =
        dunce::canonicalize(ancestor).map_err(|source| PathError::PathResolution {
            path: ancestor.to_path_buf(),
            source,
        })?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// Path validation failures with stable MCP-facing classifications.
#[derive(Debug, Error)]
pub enum PathError {
    #[error("no allowed vault roots are configured")]
    NoRoots,
    #[error("{count} vault roots exceed the supported identifier range")]
    TooManyRoots { count: usize },
    #[error("vault root is configured more than once: {path}")]
    DuplicateRoot { path: PathBuf },
    #[error("vault name is not a valid URI scheme token: {name:?} (root {path})")]
    InvalidName { path: PathBuf, name: String },
    #[error("vault name is configured more than once: {name}")]
    DuplicateName { name: String },
    #[error("vault root could not be resolved: {path}")]
    RootResolution {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("vault root is not a directory: {path}")]
    RootNotDirectory { path: PathBuf },
    #[error("path is not valid UTF-8: {path:?}")]
    NonUtf8 { path: PathBuf },
    #[error("path is empty or contains a null byte: {path:?}")]
    Invalid { path: PathBuf },
    #[error("empty path after vault-name prefix: \"{name}://\"")]
    EmptyNamedPrefixRemainder { name: String },
    #[error("parent traversal is not permitted: {path}")]
    Traversal { path: PathBuf },
    #[error("relative path is ambiguous with multiple vault roots: {path}")]
    AmbiguousRoot { path: PathBuf },
    #[error("no configured vault has the name: {name}")]
    UnknownVaultName { name: String },
    #[error("path resolves outside every allowed vault root: {path}")]
    OutsideRoot { path: PathBuf },
    #[error("symlink resolves outside every allowed vault root: {path}")]
    SymlinkEscape { path: PathBuf },
    #[error("Windows verbatim paths are not accepted from tool input: {path}")]
    WindowsVerbatim { path: PathBuf },
    #[error("Windows alternate data streams are not permitted: {path}")]
    AlternateDataStream { path: PathBuf },
    #[error("Windows drive or UNC paths are not valid on this host platform: {path}")]
    UnsupportedWindowsPath { path: PathBuf },
    #[error("path metadata could not be inspected: {path}")]
    PathInspection {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("path could not be resolved: {path}")]
    PathResolution {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("resolved path does not share the selected root prefix: {path}")]
    PrefixMismatch {
        path: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
}

impl PathError {
    /// Returns the offending path when this failure is path-specific.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::NoRoots
            | Self::TooManyRoots { .. }
            | Self::DuplicateName { .. }
            | Self::UnknownVaultName { .. }
            | Self::EmptyNamedPrefixRemainder { .. } => None,
            Self::DuplicateRoot { path }
            | Self::InvalidName { path, .. }
            | Self::RootResolution { path, .. }
            | Self::RootNotDirectory { path }
            | Self::NonUtf8 { path }
            | Self::Invalid { path }
            | Self::Traversal { path }
            | Self::AmbiguousRoot { path }
            | Self::OutsideRoot { path }
            | Self::SymlinkEscape { path }
            | Self::WindowsVerbatim { path }
            | Self::AlternateDataStream { path }
            | Self::UnsupportedWindowsPath { path }
            | Self::PathInspection { path, .. }
            | Self::PathResolution { path, .. }
            | Self::PrefixMismatch { path, .. } => Some(path),
        }
    }

    /// Stable machine-readable code for MCP error data.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Traversal { .. } | Self::OutsideRoot { .. } => "path/outside-root",
            Self::SymlinkEscape { .. } => "path/symlink-escape",
            Self::AmbiguousRoot { .. } => "path/ambiguous-root",
            Self::UnknownVaultName { .. } => "path/unknown-vault-name",
            Self::EmptyNamedPrefixRemainder { .. } => "path/empty-named-prefix",
            Self::InvalidName { .. } => "path/invalid-vault-name",
            Self::DuplicateName { .. } => "path/duplicate-vault-name",
            Self::WindowsVerbatim { .. }
            | Self::AlternateDataStream { .. }
            | Self::UnsupportedWindowsPath { .. } => "path/invalid-windows-path",
            Self::RootResolution { .. }
            | Self::RootNotDirectory { .. }
            | Self::NoRoots
            | Self::TooManyRoots { .. }
            | Self::DuplicateRoot { .. } => "path/invalid-root",
            Self::NonUtf8 { .. }
            | Self::Invalid { .. }
            | Self::PathInspection { .. }
            | Self::PathResolution { .. }
            | Self::PrefixMismatch { .. } => "path/invalid",
        }
    }

    /// Actionable remediation suitable for an MCP error response.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::AmbiguousRoot { .. } => {
                "Pass an absolute path, or prefix the path with a configured vault name \
                 (\"name://relative-path\"), when more than one vault root is configured."
            }
            Self::UnknownVaultName { .. } => {
                "Use a name from a configured vault (see vault_info or \
                 fs_list_allowed_directories), or omit the prefix to use the existing \
                 absolute-path or single-vault-relative resolution rules."
            }
            Self::EmptyNamedPrefixRemainder { .. } => {
                "Append a relative path after the \"name://\" prefix, or use \"name://.\" to \
                 select the whole named vault."
            }
            Self::NoRoots => {
                "Configure at least one allowed vault directory and restart the server."
            }
            Self::RootResolution { .. } | Self::RootNotDirectory { .. } => {
                "Configure an existing, readable directory as the vault root."
            }
            Self::SymlinkEscape { .. } => {
                "Use a path whose symlinks resolve within an allowed vault root."
            }
            Self::InvalidName { .. } => {
                "Configure a vault name starting with a letter, containing only letters, \
                 digits, '+', '-', or '.', or remove the override to use the default folder \
                 name if it does not qualify."
            }
            Self::DuplicateName { .. } => {
                "Configure a unique name for each vault, or remove an explicit override so \
                 the default no longer collides with another configured vault."
            }
            Self::Traversal { .. }
            | Self::OutsideRoot { .. }
            | Self::WindowsVerbatim { .. }
            | Self::AlternateDataStream { .. }
            | Self::UnsupportedWindowsPath { .. } => {
                "Use a normal path contained within an allowed vault root."
            }
            Self::TooManyRoots { .. }
            | Self::DuplicateRoot { .. }
            | Self::NonUtf8 { .. }
            | Self::Invalid { .. }
            | Self::PathInspection { .. }
            | Self::PathResolution { .. }
            | Self::PrefixMismatch { .. } => {
                "Use a valid UTF-8 path beneath a readable, uniquely configured vault root."
            }
        }
    }
}
