use std::cmp::Ordering;
use std::fs::{self, Metadata};
use std::io::Read;
use std::path::Path;

use contextos_core::VaultPath;
use globset::{GlobBuilder, GlobMatcher};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use walkdir::WalkDir;

use crate::{ContentHash, Filesystem, FsError};

/// Filesystem entry kind used by list, tree, and info results.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    #[serde(rename = "dir")]
    Directory,
}

/// One direct child of a listed directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<OffsetDateTime>,
}

/// Structured and host-readable directory result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DirectoryListing {
    pub entries: Vec<DirectoryEntry>,
    pub rendered: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListDirectoryRequest {
    pub path: VaultPath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortBy {
    Name,
    Size,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListDirectoryWithSizesRequest {
    pub path: VaultPath,
    pub sort_by: SortBy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryTreeRequest {
    pub path: VaultPath,
    pub exclude_patterns: Vec<String>,
    pub max_depth: usize,
}

/// Recursive directory node serialised with the MCP catalogue's `type` field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TreeNode {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Self>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFilesRequest {
    pub path: VaultPath,
    pub pattern: String,
    pub exclude_patterns: Vec<String>,
    pub max_results: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileInfoRequest {
    pub path: VaultPath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub created: Option<OffsetDateTime>,
    pub modified: Option<OffsetDateTime>,
    pub accessed: Option<OffsetDateTime>,
    pub readonly: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AllowedDirectory {
    /// This vault's configured or default-derived name, used to
    /// address it as `{name}://{relative-path}`.
    pub name: String,
    pub path: String,
    pub managed: bool,
}

impl Filesystem {
    /// Lists direct children sorted by name.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unauthorised, missing, non-directory, or
    /// unreadable paths.
    pub fn list_directory(
        &self,
        request: &ListDirectoryRequest,
    ) -> Result<DirectoryListing, FsError> {
        let path = self.authorise(&request.path)?;
        let entries = directory_entries(path, false)?;
        let hidden = self.hidden(&request.path)?;
        let excludes = exclusion_matcher(path, hidden, &[])?;
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| {
                !is_excluded(
                    &excludes,
                    Path::new(&entry.name),
                    entry.kind == EntryKind::Directory,
                )
            })
            .collect();
        Ok(DirectoryListing {
            rendered: render_entries(&entries, false),
            entries,
        })
    }

    /// Lists children with sizes and the requested ordering.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unauthorised, missing, non-directory, or
    /// unreadable paths.
    pub fn list_directory_with_sizes(
        &self,
        request: &ListDirectoryWithSizesRequest,
    ) -> Result<DirectoryListing, FsError> {
        let path = self.authorise(&request.path)?;
        let entries = directory_entries(path, true)?;
        let hidden = self.hidden(&request.path)?;
        let excludes = exclusion_matcher(path, hidden, &[])?;
        let mut entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| {
                !is_excluded(
                    &excludes,
                    Path::new(&entry.name),
                    entry.kind == EntryKind::Directory,
                )
            })
            .collect();
        if request.sort_by == SortBy::Size {
            entries.sort_by(|left, right| {
                left.size
                    .cmp(&right.size)
                    .then_with(|| left.name.cmp(&right.name))
            });
        }
        Ok(DirectoryListing {
            rendered: render_entries(&entries, true),
            entries,
        })
    }

    /// Builds a bounded, excluded recursive directory tree.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid patterns or inaccessible paths.
    pub fn directory_tree(&self, request: &DirectoryTreeRequest) -> Result<TreeNode, FsError> {
        let path = self.authorise(&request.path)?;
        ensure_directory(path)?;
        let hidden = self.hidden(&request.path)?;
        let excludes = exclusion_matcher(path, hidden, &request.exclude_patterns)?;
        tree_node(path, path, 0, request.max_depth, &excludes)
    }

    /// Finds paths by a case-insensitive glob while respecting exclusions.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid patterns or inaccessible paths.
    pub fn search_files(&self, request: &SearchFilesRequest) -> Result<Vec<String>, FsError> {
        let path = self.authorise(&request.path)?;
        ensure_directory(path)?;
        let matcher = GlobBuilder::new(&request.pattern)
            .case_insensitive(true)
            .build()
            .map_err(|source| FsError::InvalidGlob {
                pattern: request.pattern.clone(),
                source,
            })?
            .compile_matcher();
        let hidden = self.hidden(&request.path)?;
        let excludes = exclusion_matcher(path, hidden, &request.exclude_patterns)?;
        search_walk(path, request.max_results, &matcher, &excludes)
    }

    /// Returns metadata and a content hash for bounded regular files.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unauthorised, missing, or unreadable path.
    pub fn file_info(&self, request: &FileInfoRequest) -> Result<FileInfo, FsError> {
        let path = self.authorise(&request.path)?;
        let file_metadata = metadata(path)?;
        let kind = if file_metadata.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        let limits = self.limits(&request.path)?;
        let content_hash =
            if file_metadata.is_file() && file_metadata.len() <= limits.max_read_bytes {
                Some(hash_file(path)?)
            } else {
                None
            };
        Ok(FileInfo {
            path: request.path.relative().to_string_lossy().into_owned(),
            kind,
            size: file_metadata.len(),
            created: file_metadata.created().ok().map(OffsetDateTime::from),
            modified: file_metadata.modified().ok().map(OffsetDateTime::from),
            accessed: file_metadata.accessed().ok().map(OffsetDateTime::from),
            readonly: file_metadata.permissions().readonly(),
            content_hash,
        })
    }

    /// Lists the trusted roots used to validate all filesystem operations.
    #[must_use]
    pub fn list_allowed_directories(&self) -> Vec<AllowedDirectory> {
        (&self.roots)
            .into_iter()
            .map(|root| AllowedDirectory {
                name: root.name().to_owned(),
                path: root.path().to_string_lossy().into_owned(),
                managed: root.managed(),
            })
            .collect()
    }

    pub(crate) fn authorise<'a>(&self, vault_path: &'a VaultPath) -> Result<&'a Path, FsError> {
        let path: &Path = vault_path.into();
        if self.roots.authorises(vault_path) {
            Ok(path)
        } else {
            Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            })
        }
    }
}

fn metadata(path: &Path) -> Result<Metadata, FsError> {
    match path.metadata() {
        Ok(value) => Ok(value),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(FsError::NotFound {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(FsError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn ensure_directory(path: &Path) -> Result<(), FsError> {
    if metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(FsError::NotDirectory {
            path: path.to_path_buf(),
        })
    }
}

fn directory_entries(path: &Path, include_sizes: bool) -> Result<Vec<DirectoryEntry>, FsError> {
    ensure_directory(path)?;
    let reader = fs::read_dir(path).map_err(|source| FsError::ReadDirectory {
        path: path.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for item in reader {
        let item = item.map_err(|source| FsError::ReadDirectoryEntry {
            path: path.to_path_buf(),
            source,
        })?;
        let item_metadata = item.metadata().map_err(|source| FsError::ReadMetadata {
            path: item.path(),
            source,
        })?;
        entries.push(DirectoryEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            kind: if item_metadata.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: include_sizes.then_some(item_metadata.len()),
            modified: item_metadata.modified().ok().map(OffsetDateTime::from),
        });
    }
    entries.sort_by(|left, right| match (left.kind, right.kind) {
        (EntryKind::Directory, EntryKind::File) => Ordering::Less,
        (EntryKind::File, EntryKind::Directory) => Ordering::Greater,
        _ => left.name.cmp(&right.name),
    });
    Ok(entries)
}

fn render_entries(entries: &[DirectoryEntry], include_sizes: bool) -> String {
    entries
        .iter()
        .map(|entry| {
            let marker = if entry.kind == EntryKind::Directory {
                "[DIR]"
            } else {
                "[FILE]"
            };
            match (include_sizes, entry.size) {
                (true, Some(size)) => format!("{marker} {} ({size} B)", entry.name),
                _ => format!("{marker} {}", entry.name),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Vault-substrate patterns hidden from every enumeration surface by
/// default: a vault operator's own `hidden`
/// configuration is layered on top of, never instead of, this baseline.
/// Governs omission from listings only; a direct, explicit-path read is
/// never affected.
#[must_use]
pub fn default_hidden_patterns() -> Vec<String> {
    [".git/", ".git/**", ".contextos/", ".contextos/**"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn exclusion_matcher(
    root: &Path,
    hidden: &[String],
    patterns: &[String],
) -> Result<Gitignore, FsError> {
    let mut builder = GitignoreBuilder::new(root);
    for fixed in hidden {
        builder
            .add_line(None, fixed)
            .map_err(|source| FsError::InvalidExclude {
                pattern: fixed.clone(),
                source,
            })?;
    }
    for pattern in patterns {
        builder
            .add_line(None, pattern)
            .map_err(|source| FsError::InvalidExclude {
                pattern: pattern.clone(),
                source,
            })?;
    }
    builder.build().map_err(|source| FsError::InvalidExclude {
        pattern: patterns.join(", "),
        source,
    })
}

fn is_excluded(excludes: &Gitignore, relative: &Path, is_directory: bool) -> bool {
    let direct = excludes
        .matched_path_or_any_parents(relative, is_directory)
        .is_ignore();
    direct
        || (is_directory
            && excludes
                .matched_path_or_any_parents(relative.join("__contextos_descendant__"), false)
                .is_ignore())
}

fn tree_node(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    excludes: &Gitignore,
) -> Result<TreeNode, FsError> {
    let current_metadata = metadata(current)?;
    let kind = if current_metadata.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let name = current.file_name().map_or_else(
        || current.to_string_lossy().into_owned(),
        |value| value.to_string_lossy().into_owned(),
    );
    let children = if kind == EntryKind::Directory && depth < max_depth {
        let mut nodes = Vec::new();
        for entry in directory_entries(current, false)? {
            let child = current.join(&entry.name);
            let relative = child
                .strip_prefix(root)
                .map_err(|source| FsError::PrefixMismatch {
                    path: child.clone(),
                    source,
                })?;
            if !is_excluded(excludes, relative, entry.kind == EntryKind::Directory) {
                nodes.push(tree_node(root, &child, depth + 1, max_depth, excludes)?);
            }
        }
        Some(nodes)
    } else {
        None
    };
    Ok(TreeNode {
        name,
        kind,
        children,
    })
}

/// Renders a relative path using forward slashes regardless of platform,
/// matching this vault's documented relative-path convention: `to_string_
/// lossy` alone would leak the native separator (`\` on Windows), which a
/// caller comparing or storing these strings across platforms must not see.
fn forward_slash_display(relative: &Path) -> String {
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn search_walk(
    root: &Path,
    maximum: usize,
    matcher: &GlobMatcher,
    excludes: &Gitignore,
) -> Result<Vec<String>, FsError> {
    let mut found_paths = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().skip(1) {
        let entry = entry.map_err(|source| FsError::WalkDirectory {
            path: root.to_path_buf(),
            source,
        })?;
        let relative =
            entry
                .path()
                .strip_prefix(root)
                .map_err(|source| FsError::PrefixMismatch {
                    path: entry.path().to_path_buf(),
                    source,
                })?;
        if is_excluded(excludes, relative, entry.file_type().is_dir()) {
            continue;
        }
        if matcher.is_match(relative) {
            found_paths.push(forward_slash_display(relative));
        }
    }
    found_paths.sort_by_key(|value| value.to_lowercase());
    found_paths.truncate(maximum);
    Ok(found_paths)
}

pub(crate) fn hash_file(path: &Path) -> Result<ContentHash, FsError> {
    let mut file = fs::File::open(path).map_err(|source| FsError::OpenRead {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| FsError::ReadContent {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(ContentHash::from(<[u8; 32]>::from(hasher.finalize())))
}
