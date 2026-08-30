//! Text-search synchronisation.
//!
//! `TextSearchService` keeps one vault root's text index aligned with its
//! markdown files. `update` applies `OperationEvent`s from the write pipeline
//! immediately, while `refresh` reconciles filesystem changes that bypassed
//! the pipeline entirely (external editors, synced files, or restored
//! backups) against the stored index state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use contextos_core::{
    OpKind, OperationEvent, OperationWarning, PathError, UpdatesSearch, VaultPath, VaultPathInput,
    VaultRoot, VaultRootId, VaultSet,
};
use serde::Serialize;
use time::OffsetDateTime;

use crate::document::relative_display;
use crate::text::{IndexEntry, IndexesText};
use crate::{DocumentSource, IndexedDocument, SearchError};

/// Construction input for one vault root's `TextSearchService`.
pub struct TextSyncConfig<I> {
    /// Identity of the vault root this service synchronises.
    pub root_id: VaultRootId,
    /// Resolved filesystem path of the vault root.
    pub root: PathBuf,
    /// Forward-slash relative path prefixes excluded from synchronisation.
    pub excludes: Vec<String>,
    /// The text index kept in sync.
    pub index: I,
}

/// Keeps one vault root's text index aligned with its markdown files.
///
/// `update` (an `UpdatesSearch` implementation) applies `OperationEvent`s from
/// the write pipeline immediately. `refresh` reconciles filesystem changes
/// made outside the pipeline against the stored index state.
pub struct TextSearchService<I> {
    root_id: VaultRootId,
    root: PathBuf,
    excludes: Vec<String>,
    index: I,
}

impl<I> From<TextSyncConfig<I>> for TextSearchService<I> {
    fn from(value: TextSyncConfig<I>) -> Self {
        Self {
            root_id: value.root_id,
            root: value.root,
            excludes: value.excludes,
            index: value.index,
        }
    }
}

impl<I> TextSearchService<I> {
    /// Returns the underlying text index.
    #[must_use]
    pub const fn index(&self) -> &I {
        &self.index
    }
}

/// Scan and reconciliation counts for one `refresh` pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct FreshnessReport {
    /// Markdown files successfully visited under the vault root.
    pub scanned: usize,
    /// Files upserted because they were missing or stale in the index.
    pub reindexed: usize,
    /// Stored entries removed because their file no longer exists.
    pub removed: usize,
}

impl<I: IndexesText> TextSearchService<I> {
    /// Reconciles the text index against the current filesystem state,
    /// detecting external changes that bypassed the write pipeline (external
    /// editors, synced files, or restored backups).
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index or vault root cannot be read,
    /// or when the index cannot be written.
    pub fn refresh(&self) -> Result<FreshnessReport, SearchError> {
        let mut stored: BTreeMap<String, IndexEntry> = self
            .index
            .entries()?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();

        let files = collect_markdown_files(&self.root, &self.excludes)?;
        let roots = resolve_single_root_set(&self.root)?;

        let mut report = FreshnessReport::default();
        let mut pending = Vec::new();

        for (relative, absolute) in files {
            let existing = stored.remove(&relative);

            let Ok(metadata) = fs::metadata(&absolute) else {
                continue;
            };
            let Ok(system_modified) = metadata.modified() else {
                continue;
            };
            let modified = OffsetDateTime::from(system_modified);

            if existing
                .as_ref()
                .is_some_and(|entry| entry.modified == modified)
            {
                report.scanned = report.scanned.saturating_add(1);
                continue;
            }

            let Ok(content) = fs::read_to_string(&absolute) else {
                continue;
            };
            report.scanned = report.scanned.saturating_add(1);

            let Ok(path) = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &relative,
            }) else {
                continue;
            };
            let document = IndexedDocument::from(DocumentSource {
                path: &path,
                content: &content,
                modified,
            });
            let hash: &str = document.content_hash().into();
            let unchanged = existing.is_some_and(|entry| entry.content_hash == hash);
            pending.push(document);
            if !unchanged {
                report.reindexed = report.reindexed.saturating_add(1);
            }
        }

        if !pending.is_empty() {
            self.index.index(&pending)?;
        }

        for path in stored.into_keys() {
            self.index.remove(&path)?;
            report.removed = report.removed.saturating_add(1);
        }

        Ok(report)
    }

    /// Removes the document stored for `path`, ignoring any scope filter.
    fn remove_path(&self, path: &VaultPath) -> Result<(), SearchError> {
        self.index.remove(&relative_display(path))
    }

    /// Indexes `path` from its current filesystem content, or removes it from
    /// the index when the file is missing or no longer a regular file.
    fn sync_path(&self, path: &VaultPath) -> Result<(), SearchError> {
        let relative = relative_display(path);
        if !in_scope(&self.excludes, &relative) {
            return Ok(());
        }

        let absolute = self.root.join(&relative);
        let is_file = fs::metadata(&absolute).is_ok_and(|metadata| metadata.is_file());
        if !is_file {
            return self.index.remove(&relative);
        }

        let content =
            fs::read_to_string(&absolute).map_err(|source| SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })?;
        let metadata = fs::metadata(&absolute).map_err(|source| SearchError::DocumentRead {
            path: relative.clone(),
            source,
        })?;
        let modified = metadata
            .modified()
            .map(OffsetDateTime::from)
            .map_err(|source| SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })?;

        let document = IndexedDocument::from(DocumentSource {
            path,
            content: &content,
            modified,
        });
        self.index.index(&[document])
    }
}

impl<I: IndexesText> UpdatesSearch for TextSearchService<I> {
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        match event.kind {
            OpKind::Delete => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.remove_path(path)?;
                    }
                }
            }
            OpKind::Move => {
                if let Some(from) = event.paths.first()
                    && from.root_id() == self.root_id
                {
                    self.remove_path(from)?;
                }
                if let Some(to) = event.paths.get(1)
                    && to.root_id() == self.root_id
                {
                    self.sync_path(to)?;
                }
            }
            OpKind::Create | OpKind::Modify | OpKind::Restore => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.sync_path(path)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Reports whether `relative` is an in-scope markdown path: it must end with
/// `.md` and must not fall under any configured exclude prefix.
fn in_scope(excludes: &[String], relative: &str) -> bool {
    is_markdown(relative) && !is_excluded(excludes, relative)
}

/// Reports whether `relative` names a markdown file by extension: this
/// crate's one definition of "markdown file" (case-insensitive `.md`),
/// reused by `contextos-server`'s MCP resources surface so that surface
/// never grows a second, potentially divergent definition.
#[must_use]
pub fn is_markdown(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Reports whether `relative` equals or falls under one of `excludes`, each a
/// forward-slash relative path prefix.
fn is_excluded(excludes: &[String], relative: &str) -> bool {
    excludes.iter().any(|prefix| {
        relative == prefix.as_str()
            || relative
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Builds a single-root `VaultSet` from the already-resolved vault root, used
/// to construct throwaway `VaultPath`s for files discovered by the walk.
fn resolve_single_root_set(root: &Path) -> Result<VaultSet, SearchError> {
    let vault_root =
        VaultRoot::try_from(root.to_path_buf()).map_err(|source| root_error(root, source))?;
    VaultSet::try_from(vec![vault_root]).map_err(|source| root_error(root, source))
}

fn root_error(root: &Path, source: PathError) -> SearchError {
    SearchError::IndexDirectory {
        path: root.display().to_string(),
        source: std::io::Error::other(source),
    }
}

/// Recursively walks `root` in deterministic sorted order, returning every
/// markdown file not under an exclude prefix as a (relative, absolute) pair.
fn collect_markdown_files(
    root: &Path,
    excludes: &[String],
) -> Result<Vec<(String, PathBuf)>, SearchError> {
    let mut files = Vec::new();
    walk_directory(root, root, excludes, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn walk_directory(
    root: &Path,
    directory: &Path,
    excludes: &[String],
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), SearchError> {
    let entries = fs::read_dir(directory).map_err(|source| SearchError::DocumentRead {
        path: directory.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SearchError::DocumentRead {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(root) else {
            continue;
        };
        let relative = forward_slash(relative_path);
        if is_excluded(excludes, &relative) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| SearchError::DocumentRead {
                path: path.display().to_string(),
                source,
            })?;
        if file_type.is_dir() {
            walk_directory(root, &path, excludes, files)?;
        } else if file_type.is_file() && is_markdown(&relative) {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Joins path components with `/`, matching `document::relative_display` but
/// for a plain filesystem path rather than a validated `VaultPath`.
fn forward_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}
