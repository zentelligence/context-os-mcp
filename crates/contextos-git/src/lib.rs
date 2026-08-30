#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use contextos_core::{
    Clock, ContentHash, DeleteMode, DeleteMutation, OpKind, OperationEvent, OperationWarning,
    Origin, RestoreMutation, VaultPath, VaultPathInput, VaultRoot, VaultSet, VersionsVault,
    WriteMutation, WritesVault,
};
use git2::{
    DiffFormat, DiffOptions, IndexAddOption, IndexEntry, Repository, RepositoryInitOptions,
    Signature, Sort, Status, StatusOptions, Time,
};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use thiserror::Error;

const DERIVED_IGNORE: &str = ".contextos/";
const OBSIDIAN_WORKSPACE_IGNORE: &str = ".obsidian/workspace*";
const DIFF_TRUNCATION_NOTICE: &str = "\n[diff truncated]\n";
const PENDING_DIRECTORY: &str = "contextos-mcp/pending";

/// Trusted dependencies and identity for one local Git vault.
#[derive(Clone, Debug)]
pub struct Git2VaultConfig<C> {
    pub root: VaultRoot,
    pub roots: VaultSet,
    pub clock: C,
    pub author_name: String,
    pub author_email: String,
    pub allow_destructive_restore: bool,
    pub protected_restore_paths: Vec<PathBuf>,
}

/// Local libgit2 adapter scoped to one configured vault root.
#[derive(Clone, Debug)]
pub struct Git2Vault<C> {
    root: VaultRoot,
    roots: VaultSet,
    clock: C,
    author_name: String,
    author_email: String,
    allow_destructive_restore: bool,
    protected_restore_paths: Vec<PathBuf>,
    pending: Arc<Mutex<PendingBatch>>,
}

#[derive(Clone, Debug, Default)]
struct PendingBatch {
    paths: BTreeSet<PathBuf>,
    events: Vec<OperationEvent>,
    generation: u64,
    staged_at_unix: Option<i64>,
}

impl<C> TryFrom<Git2VaultConfig<C>> for Git2Vault<C> {
    type Error = GitError;

    fn try_from(value: Git2VaultConfig<C>) -> Result<Self, Self::Error> {
        if value.author_name.trim().is_empty() {
            return Err(GitError::InvalidAuthorName);
        }
        if value.author_email.trim().is_empty() || !value.author_email.contains('@') {
            return Err(GitError::InvalidAuthorEmail);
        }
        if !value.roots.iter().any(|root| root == &value.root) {
            return Err(GitError::RootNotConfigured);
        }
        if value.protected_restore_paths.iter().any(|path| {
            path.as_os_str().is_empty()
                || path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) {
            return Err(GitError::InvalidProtectedRestorePath);
        }
        Ok(Self {
            root: value.root,
            roots: value.roots,
            clock: value.clock,
            author_name: value.author_name,
            author_email: value.author_email,
            allow_destructive_restore: value.allow_destructive_restore,
            protected_restore_paths: value.protected_restore_paths,
            pending: Arc::new(Mutex::new(PendingBatch::default())),
        })
    }
}

/// Outcome of initialising local recovery history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitInitResult {
    pub initialised: bool,
    pub commit_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusResult {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    pub pending_paths: Vec<String>,
    pub staged_at_unix: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogRequest {
    pub path: Option<PathBuf>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogEntry {
    pub id: String,
    pub short_id: String,
    pub time: i64,
    pub message: String,
    pub files_changed: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffRequest {
    pub from: Option<String>,
    pub to: Option<String>,
    pub path: Option<PathBuf>,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffResult {
    pub content: String,
    pub truncated: bool,
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Reports whether this vault currently contains a Git repository.
    #[must_use]
    pub fn is_repository(&self) -> bool {
        Repository::open(self.root.path()).is_ok()
    }

    /// Reports repository and MCP-owned pending state.
    ///
    /// # Errors
    ///
    /// Returns a typed repository or pending-state error.
    pub fn status(&self) -> Result<GitStatusResult, GitError> {
        let repository = open_repository(self.root.path())?;
        let branch = repository
            .head()
            .map_err(GitError::Repository)?
            .shorthand()
            .unwrap_or("HEAD")
            .to_owned();
        let mut options = StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repository
            .statuses(Some(&mut options))
            .map_err(GitError::Repository)?;
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().map_err(GitError::Repository)?.to_owned();
            let status = entry.status();
            if status.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            ) {
                staged.push(path.clone());
            }
            if status.contains(Status::WT_NEW) {
                untracked.push(path.clone());
            }
            if status.intersects(
                Status::WT_MODIFIED
                    | Status::WT_DELETED
                    | Status::WT_RENAMED
                    | Status::WT_TYPECHANGE,
            ) {
                unstaged.push(path);
            }
        }
        let pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        let pending_paths = pending
            .paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let staged_at_unix = pending.staged_at_unix;
        Ok(GitStatusResult {
            branch,
            ahead: 0,
            behind: 0,
            staged,
            unstaged,
            untracked,
            pending_paths,
            staged_at_unix,
        })
    }

    /// Reads newest-first local history with an optional path filter.
    ///
    /// # Errors
    ///
    /// Returns a typed repository traversal or diff error.
    pub fn log(&self, request: &GitLogRequest) -> Result<Vec<GitLogEntry>, GitError> {
        if request.limit == 0 {
            return Err(GitError::InvalidLogLimit);
        }
        let repository = open_repository(self.root.path())?;
        let mut walk = repository.revwalk().map_err(GitError::Repository)?;
        walk.set_sorting(Sort::TIME).map_err(GitError::Repository)?;
        walk.push_head().map_err(GitError::Repository)?;
        let mut entries = Vec::new();
        for id in walk {
            let commit = repository
                .find_commit(id.map_err(GitError::Repository)?)
                .map_err(GitError::Repository)?;
            let tree = commit.tree().map_err(GitError::Repository)?;
            let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
            let diff = repository
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
                .map_err(GitError::Repository)?;
            let mut files = diff
                .deltas()
                .filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            files.sort();
            files.dedup();
            if request.path.as_ref().is_some_and(|path| {
                !files
                    .iter()
                    .any(|file| Path::new(file) == path || Path::new(file).starts_with(path))
            }) {
                continue;
            }
            let id = commit.id().to_string();
            entries.push(GitLogEntry {
                short_id: id.chars().take(7).collect(),
                id,
                time: commit.time().seconds(),
                message: commit.message().unwrap_or("").to_owned(),
                files_changed: files,
            });
            if entries.len() >= request.limit {
                break;
            }
        }
        Ok(entries)
    }

    /// Produces a byte-capped unified diff between refs or the working tree.
    ///
    /// # Errors
    ///
    /// Returns a typed reference, repository, or limit error.
    pub fn diff(&self, request: &GitDiffRequest) -> Result<GitDiffResult, GitError> {
        if request.max_bytes == 0 {
            return Err(GitError::InvalidDiffLimit);
        }
        let repository = open_repository(self.root.path())?;
        let from = request.from.as_deref().unwrap_or("HEAD");
        let from_tree = repository
            .revparse_single(from)
            .and_then(|object| object.peel_to_tree())
            .map_err(GitError::Repository)?;
        let mut options = DiffOptions::new();
        if let Some(path) = &request.path {
            options.pathspec(path);
        }
        let diff = if let Some(to) = &request.to {
            let to_tree = repository
                .revparse_single(to)
                .and_then(|object| object.peel_to_tree())
                .map_err(GitError::Repository)?;
            repository
                .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut options))
                .map_err(GitError::Repository)?
        } else {
            repository
                .diff_tree_to_workdir_with_index(Some(&from_tree), Some(&mut options))
                .map_err(GitError::Repository)?
        };
        let capture_limit = request.max_bytes.saturating_add(1);
        let mut bytes = Vec::new();
        let mut truncated = false;
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                let mut encoded = [0_u8; 4];
                let prefix = line.origin().encode_utf8(&mut encoded).as_bytes();
                let available = capture_limit.saturating_sub(bytes.len());
                let retained = available.min(prefix.len());
                bytes.extend_from_slice(&prefix[..retained]);
                truncated |= retained < prefix.len();
            }
            let available = capture_limit.saturating_sub(bytes.len());
            let retained = available.min(line.content().len());
            bytes.extend_from_slice(&line.content()[..retained]);
            truncated |= retained < line.content().len();
            true
        })
        .map_err(GitError::Repository)?;
        truncated |= bytes.len() > request.max_bytes;
        let content = if truncated && request.max_bytes >= DIFF_TRUNCATION_NOTICE.len() {
            let content_limit = request.max_bytes - DIFF_TRUNCATION_NOTICE.len();
            let mut content =
                String::from_utf8_lossy(&bytes[..bytes.len().min(content_limit)]).into_owned();
            while content.len() > content_limit {
                content.pop();
            }
            content.push_str(DIFF_TRUNCATION_NOTICE);
            content
        } else {
            let mut content =
                String::from_utf8_lossy(&bytes[..bytes.len().min(request.max_bytes)]).into_owned();
            while content.len() > request.max_bytes {
                content.pop();
            }
            content
        };
        Ok(GitDiffResult { content, truncated })
    }

    /// Commits staged state left by an interrupted previous server process.
    ///
    /// # Errors
    ///
    /// Returns a typed repository or commit error.
    pub fn recover_staged(&self) -> Result<GitCommitResult, GitError> {
        if !self.is_repository() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        let recorded = self.recorded_pending_paths()?;
        if recorded.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        let repository = open_repository(self.root.path())?;
        let mut index = repository.index().map_err(GitError::Repository)?;
        for path in &recorded {
            let absolute = self.root.path().join(path);
            if absolute.exists() {
                Self::stage_present_path(&mut index, path, &absolute)?;
            } else {
                index
                    .remove_all([path], None)
                    .map_err(GitError::Repository)?;
            }
        }
        index.write().map_err(GitError::Repository)?;
        let head_tree = repository
            .head()
            .and_then(|head| head.peel_to_tree())
            .map_err(GitError::Repository)?;
        let diff = repository
            .diff_tree_to_index(Some(&head_tree), Some(&index), None)
            .map_err(GitError::Repository)?;
        let staged = diff
            .deltas()
            .filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        let paths = recorded
            .intersection(&staged)
            .cloned()
            .collect::<BTreeSet<_>>();
        let stale = recorded.difference(&paths).cloned().collect::<Vec<_>>();
        self.clear_pending_paths(&stale)?;
        if paths.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        self.pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?
            .paths = paths;
        self.commit(Some("mcp: recovered staged changes"))
    }

    /// Returns the monotonic generation of the current pending batch.
    ///
    /// # Errors
    ///
    /// Returns an error when pending state is unavailable.
    pub fn pending_generation(&self) -> Result<u64, GitError> {
        Ok(self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?
            .generation)
    }

    /// Reports whether the current quiet-period batch will produce a commit.
    ///
    /// # Errors
    ///
    /// Returns an error when pending state is unavailable.
    pub fn pending_commit_count(&self) -> Result<usize, GitError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        Ok(usize::from(!pending.paths.is_empty()))
    }

    /// Commits only if no newer event has reset the debounce generation.
    ///
    /// # Errors
    ///
    /// Returns a typed pending-state or commit error.
    pub fn commit_if_generation(&self, generation: u64) -> Result<GitCommitResult, GitError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        if pending.generation != generation {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        self.commit_pending(&mut pending, None)
    }

    /// Initialises a repository, maintains ignore policy, and commits vault state.
    ///
    /// # Errors
    ///
    /// Returns a typed path, write, or libgit2 error when initialisation cannot
    /// establish a recoverable initial state.
    pub fn initialise<W>(&self, writer: &W) -> Result<GitInitResult, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        if self.root.path().join(".git").exists() {
            if let Some(event) = self.maintain_ignore_policy(writer)? {
                self.stage_event(&event)?;
            }
            let repository = open_repository(self.root.path())?;
            let commit_id = repository
                .head()
                .ok()
                .and_then(|head| head.target())
                .map(|id| id.to_string());
            if commit_id.is_none() {
                repository
                    .set_head("refs/heads/main")
                    .map_err(GitError::Repository)?;
                let mut index = repository.index().map_err(GitError::Repository)?;
                index
                    .add_all(["*"], IndexAddOption::DEFAULT, None)
                    .map_err(GitError::Repository)?;
                index.write().map_err(GitError::Repository)?;
                let tree_id = index.write_tree().map_err(GitError::Repository)?;
                let tree = repository
                    .find_tree(tree_id)
                    .map_err(GitError::Repository)?;
                let signature = self.signature()?;
                let id = repository
                    .commit(
                        Some("HEAD"),
                        &signature,
                        &signature,
                        "mcp: initialise ContextOS vault",
                        &tree,
                        &[],
                    )
                    .map_err(GitError::Repository)?;
                let recorded = self.recorded_pending_paths()?;
                self.clear_pending_paths(&recorded.into_iter().collect::<Vec<_>>())?;
                *self
                    .pending
                    .lock()
                    .map_err(|_| GitError::PendingStateUnavailable)? = PendingBatch::default();
                return Ok(GitInitResult {
                    initialised: true,
                    commit_id: Some(id.to_string()),
                });
            }
            return Ok(GitInitResult {
                initialised: false,
                commit_id,
            });
        }

        let _ignore_event = self.maintain_ignore_policy(writer)?;
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository =
            Repository::init_opts(self.root.path(), &options).map_err(GitError::Repository)?;
        let mut index = repository.index().map_err(GitError::Repository)?;
        index
            .add_all(["*"], IndexAddOption::DEFAULT, None)
            .map_err(GitError::Repository)?;
        index.write().map_err(GitError::Repository)?;
        let tree_id = index.write_tree().map_err(GitError::Repository)?;
        let tree = repository
            .find_tree(tree_id)
            .map_err(GitError::Repository)?;
        let signature = self.signature()?;
        let commit_id = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "mcp: initialise ContextOS vault",
                &tree,
                &[],
            )
            .map_err(GitError::Repository)?;

        Ok(GitInitResult {
            initialised: true,
            commit_id: Some(commit_id.to_string()),
        })
    }

    /// Commits only paths staged by this service, preserving operator staging.
    ///
    /// # Errors
    ///
    /// Returns a typed Git error when the owned staged snapshot cannot be
    /// materialised or committed.
    pub fn commit(&self, message: Option<&str>) -> Result<GitCommitResult, GitError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        self.commit_pending(&mut pending, message)
    }

    fn commit_pending(
        &self,
        pending: &mut PendingBatch,
        message: Option<&str>,
    ) -> Result<GitCommitResult, GitError> {
        if pending.paths.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }

        let repository = open_repository(self.root.path())?;
        let parent = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(GitError::Repository)?;
        let parent_tree = parent.tree().map_err(GitError::Repository)?;
        let staged = repository.index().map_err(GitError::Repository)?;
        let mut owned_index = git2::Index::new().map_err(GitError::Repository)?;
        owned_index
            .read_tree(&parent_tree)
            .map_err(GitError::Repository)?;

        for path in &pending.paths {
            owned_index
                .remove_all([path], None)
                .map_err(GitError::Repository)?;
            for entry in staged.iter().filter(|entry| entry_is_within(entry, path)) {
                owned_index.add(&entry).map_err(GitError::Repository)?;
            }
        }

        let tree_id = owned_index
            .write_tree_to(&repository)
            .map_err(GitError::Repository)?;
        let tree = repository
            .find_tree(tree_id)
            .map_err(GitError::Repository)?;
        let commit_message = message
            .filter(|candidate| !candidate.trim().is_empty())
            .map_or_else(|| generated_commit_message(&pending.events), str::to_owned);
        let signature = self.signature()?;
        let commit_id = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                &commit_message,
                &tree,
                &[&parent],
            )
            .map_err(GitError::Repository)?;
        let committed_paths = pending.paths.iter().cloned().collect::<Vec<_>>();
        if let Err(error) = self.clear_pending_paths(&committed_paths) {
            tracing::warn!(
                code = error.code(),
                error = %error,
                "committed Git pending-path metadata could not be cleared"
            );
        }
        pending.paths.clear();
        pending.events.clear();
        pending.staged_at_unix = None;

        Ok(GitCommitResult {
            commit_id: Some(commit_id.to_string()),
            committed_paths,
            message: Some(commit_message),
        })
    }

    /// Restores one historical UTF-8 file or directory through the shared pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed reference, object, encoding, read, or persistence error.
    pub fn restore<W>(
        &self,
        request: &GitRestoreRequest,
        writer: &W,
    ) -> Result<GitRestoreResult, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        let relative = self.relative_path(&request.path)?;
        if self.is_restore_protected(relative) {
            return Err(GitError::ProtectedRestorePath.into());
        }
        let repository = open_repository(self.root.path())?;
        let object = repository
            .revparse_single(&request.reference)
            .map_err(GitError::Repository)?;
        let commit = object.peel_to_commit().map_err(GitError::Repository)?;
        let tree = commit.tree().map_err(GitError::Repository)?;
        let historical = historical_files(&repository, &tree, relative)?
            .into_iter()
            .filter(|(path, _)| !self.is_restore_protected(path))
            .collect::<BTreeMap<_, _>>();
        let historical_paths = historical.keys().cloned().collect::<BTreeSet<_>>();

        let mut restore_plans = Vec::new();
        let mut diff = String::new();
        for (path, content) in historical {
            let vault_path = self.vault_path_for_relative(&path)?;
            let (current, _) = current_text_and_hash(&vault_path)?;
            if current == content {
                continue;
            }
            diff.push_str(
                &TextDiff::from_lines(&current, &content)
                    .unified_diff()
                    .header("working tree", &request.reference)
                    .to_string(),
            );
            restore_plans.push((vault_path, content));
        }
        let delete_paths = self.owned_deletions(relative, &historical_paths)?;
        if !delete_paths.is_empty() && !self.allow_destructive_restore {
            return Err(GitError::DestructiveRestoreDisabled.into());
        }
        for path in &delete_paths {
            let relative = path
                .relative()
                .to_str()
                .ok_or_else(|| GitError::NonUtf8Path {
                    path: path.relative().to_path_buf(),
                })?;
            diff.push_str("delete ");
            diff.push_str(relative);
            diff.push('\n');
        }
        if request.dry_run {
            return Ok(GitRestoreResult {
                diff,
                applied: false,
                events: Vec::new(),
                warnings: Vec::new(),
            });
        }

        let mut events = Vec::new();
        let mut warnings = Vec::new();
        for (path, content) in restore_plans {
            let (_, expected_hash) = current_text_and_hash(&path)?;
            let result = writer
                .restore(&RestoreMutation {
                    path,
                    content,
                    expected_hash,
                    origin: Origin::Tool("git_restore".to_owned()),
                })
                .map_err(GitWriteError::Write)?;
            events.extend(result.event);
            warnings.extend(result.warnings);
        }
        for path in delete_paths {
            let mode = if <&Path>::from(&path).is_dir() {
                DeleteMode::HardRecursive
            } else {
                DeleteMode::Hard
            };
            let result = writer
                .delete(&DeleteMutation {
                    path,
                    mode,
                    origin: Origin::Tool("git_restore".to_owned()),
                })
                .map_err(GitWriteError::Write)?;
            events.extend(result.event);
            warnings.extend(result.warnings);
        }
        Ok(GitRestoreResult {
            diff,
            applied: true,
            events,
            warnings,
        })
    }

    fn vault_path_for_relative(&self, relative: &Path) -> Result<VaultPath, GitError> {
        let absolute = self.root.path().join(relative);
        let raw = absolute.to_str().ok_or_else(|| GitError::NonUtf8Path {
            path: absolute.clone(),
        })?;
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(GitError::Path)
    }

    fn owned_deletions(
        &self,
        selected: &Path,
        historical: &BTreeSet<PathBuf>,
    ) -> Result<Vec<VaultPath>, GitError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        let mut relative_paths = BTreeSet::new();
        for path in &pending.paths {
            if self.is_restore_protected(path) {
                continue;
            }
            if !selected.as_os_str().is_empty() && !path.starts_with(selected) {
                continue;
            }
            if historical
                .iter()
                .any(|historical_path| historical_path == path || historical_path.starts_with(path))
            {
                continue;
            }
            if self.root.path().join(path).exists() {
                relative_paths.insert(path.clone());
            }
        }
        drop(pending);
        let mut paths = relative_paths
            .into_iter()
            .map(|path| self.vault_path_for_relative(&path))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort_by(|left, right| {
            right
                .relative()
                .components()
                .count()
                .cmp(&left.relative().components().count())
                .then_with(|| right.relative().cmp(left.relative()))
        });
        Ok(paths)
    }

    fn is_restore_protected(&self, path: &Path) -> bool {
        self.protected_restore_paths
            .iter()
            .any(|protected| path == protected || path.starts_with(protected))
    }

    fn stage_event(&self, event: &OperationEvent) -> Result<(), GitError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?;
        let repository = open_repository(self.root.path())?;
        let mut index = repository.index().map_err(GitError::Repository)?;

        for path in &event.paths {
            let relative = self.relative_path(path)?;
            self.record_pending_path(relative)?;
            let remove = event.kind == OpKind::Delete
                || (event.kind == OpKind::Move && !<&Path>::from(path).exists());
            if remove {
                index
                    .remove_all([relative], None)
                    .map_err(GitError::Repository)?;
            } else {
                Self::stage_present_path(&mut index, relative, <&Path>::from(path))?;
            }
            pending.paths.insert(relative.to_path_buf());
        }
        index.write().map_err(GitError::Repository)?;
        pending.events.push(event.clone());
        pending.generation = pending.generation.saturating_add(1);
        pending.staged_at_unix = Some(self.clock.now().unix_timestamp());
        Ok(())
    }

    fn record_pending_path(&self, path: &Path) -> Result<(), GitError> {
        let text = path.to_str().ok_or_else(|| GitError::NonUtf8Path {
            path: path.to_path_buf(),
        })?;
        let directory = self.pending_directory()?;
        std::fs::create_dir_all(&directory).map_err(GitError::PendingMetadata)?;
        let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
        let marker = directory.join(format!("{}.pending", <&str>::from(&digest)));
        if marker.exists() {
            return Ok(());
        }
        let temporary = marker.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            self.clock.now().unix_timestamp_nanos()
        ));
        let write_result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(GitError::PendingMetadata)?;
            file.write_all(text.as_bytes())
                .map_err(GitError::PendingMetadata)?;
            file.sync_all().map_err(GitError::PendingMetadata)?;
            std::fs::rename(&temporary, &marker).map_err(GitError::PendingMetadata)
        })();
        if write_result.is_err() {
            let _cleanup = std::fs::remove_file(&temporary);
        }
        write_result
    }

    fn recorded_pending_paths(&self) -> Result<BTreeSet<PathBuf>, GitError> {
        let directory = self.pending_directory()?;
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new());
            }
            Err(source) => return Err(GitError::PendingMetadata(source)),
        };
        let mut paths = BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(GitError::PendingMetadata)?;
            if entry.path().extension().and_then(std::ffi::OsStr::to_str) != Some("pending") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).map_err(GitError::PendingMetadata)?;
            let path = PathBuf::from(text);
            if path.as_os_str().is_empty() || path.starts_with(".git") {
                return Err(GitError::InvalidPendingMetadata);
            }
            let text = path.to_str().ok_or(GitError::InvalidPendingMetadata)?;
            let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
            let expected = format!("{}.pending", <&str>::from(&digest));
            if entry.file_name() != std::ffi::OsString::from(expected) {
                return Err(GitError::InvalidPendingMetadata);
            }
            let validated = self.vault_path_for_relative(&path)?;
            if validated.relative() != path {
                return Err(GitError::InvalidPendingMetadata);
            }
            paths.insert(path);
        }
        Ok(paths)
    }

    fn clear_pending_paths(&self, paths: &[PathBuf]) -> Result<(), GitError> {
        let directory = self.pending_directory()?;
        for path in paths {
            let text = path
                .to_str()
                .ok_or_else(|| GitError::NonUtf8Path { path: path.clone() })?;
            let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
            let marker = directory.join(format!("{}.pending", <&str>::from(&digest)));
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(GitError::PendingMetadata(source)),
            }
        }
        Ok(())
    }

    fn pending_directory(&self) -> Result<PathBuf, GitError> {
        Ok(open_repository(self.root.path())?
            .path()
            .join(PENDING_DIRECTORY))
    }

    fn stage_present_path(
        index: &mut git2::Index,
        relative: &Path,
        absolute: &Path,
    ) -> Result<(), GitError> {
        if absolute.is_dir() {
            index
                .add_all([relative], IndexAddOption::DEFAULT, None)
                .map_err(GitError::Repository)
        } else {
            index.add_path(relative).map_err(GitError::Repository)
        }
    }

    fn relative_path<'a>(&self, path: &'a VaultPath) -> Result<&'a Path, GitError> {
        let absolute = <&Path>::from(path);
        if !absolute.starts_with(self.root.path()) {
            return Err(GitError::WrongVault);
        }
        Ok(path.relative())
    }

    fn maintain_ignore_policy<W>(
        &self,
        writer: &W,
    ) -> Result<Option<OperationEvent>, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        let target = self.root.path().join(".gitignore");
        let (mut content, expected_hash) = match std::fs::read(&target) {
            Ok(bytes) => {
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                let hash = ContentHash::from(<[u8; 32]>::from(hasher.finalize()));
                let content =
                    String::from_utf8(bytes).map_err(|source| GitError::NonUtf8Ignore {
                        path: target.clone(),
                        source,
                    })?;
                (content, Some(hash))
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => (String::new(), None),
            Err(source) => {
                return Err(GitError::ReadIgnore {
                    path: target,
                    source,
                }
                .into());
            }
        };
        let mut changed = false;
        for required in [DERIVED_IGNORE, OBSIDIAN_WORKSPACE_IGNORE] {
            if !content.lines().any(|line| line == required) {
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(required);
                content.push('\n');
                changed = true;
            }
        }
        if !changed {
            return Ok(None);
        }

        let raw = target.to_str().ok_or_else(|| GitError::NonUtf8Path {
            path: target.clone(),
        })?;
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(GitError::Path)?;
        let result = writer
            .persist(&WriteMutation {
                path,
                content,
                expected_hash,
                force: false,
                origin: Origin::Internal("git".to_owned()),
            })
            .map_err(GitWriteError::Write)?;
        Ok(result.event)
    }

    fn signature(&self) -> Result<Signature<'static>, GitError> {
        let now = self.clock.now();
        let time = Time::new(
            now.unix_timestamp(),
            i32::from(now.offset().whole_minutes()),
        );
        Signature::new(&self.author_name, &self.author_email, &time).map_err(GitError::Repository)
    }
}

fn open_repository(root: &Path) -> Result<Repository, GitError> {
    Repository::open(root).map_err(|error| {
        if error.code() == git2::ErrorCode::NotFound {
            GitError::NotRepository
        } else {
            GitError::Repository(error)
        }
    })
}

fn historical_files(
    repository: &Repository,
    tree: &git2::Tree<'_>,
    selected: &Path,
) -> Result<BTreeMap<PathBuf, String>, GitError> {
    let mut files = BTreeMap::new();
    if selected.as_os_str().is_empty() {
        collect_historical_files(repository, tree, Path::new(""), &mut files)?;
        return Ok(files);
    }
    let entry = tree.get_path(selected).map_err(GitError::Repository)?;
    match entry.kind() {
        Some(git2::ObjectType::Blob) => {
            files.insert(
                selected.to_path_buf(),
                historical_blob(repository, entry.id(), selected)?,
            );
        }
        Some(git2::ObjectType::Tree) => {
            let subtree = repository
                .find_tree(entry.id())
                .map_err(GitError::Repository)?;
            collect_historical_files(repository, &subtree, selected, &mut files)?;
        }
        _ => return Err(GitError::UnsupportedRestoreObject),
    }
    Ok(files)
}

fn collect_historical_files(
    repository: &Repository,
    tree: &git2::Tree<'_>,
    base: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<(), GitError> {
    for entry in tree {
        let name = entry.name().map_err(|_| GitError::NonUtf8Path {
            path: base.to_path_buf(),
        })?;
        let path = base.join(name);
        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                files.insert(
                    path.clone(),
                    historical_blob(repository, entry.id(), &path)?,
                );
            }
            Some(git2::ObjectType::Tree) => {
                let subtree = repository
                    .find_tree(entry.id())
                    .map_err(GitError::Repository)?;
                collect_historical_files(repository, &subtree, &path, files)?;
            }
            _ => return Err(GitError::UnsupportedRestoreObject),
        }
    }
    Ok(())
}

fn historical_blob(
    repository: &Repository,
    id: git2::Oid,
    path: &Path,
) -> Result<String, GitError> {
    let blob = repository.find_blob(id).map_err(GitError::Repository)?;
    Ok(std::str::from_utf8(blob.content())
        .map_err(|source| GitError::NonUtf8Restore {
            path: path.to_path_buf(),
            source,
        })?
        .to_owned())
}

fn current_text_and_hash(path: &VaultPath) -> Result<(String, Option<ContentHash>), GitError> {
    let target = <&Path>::from(path);
    match std::fs::read(target) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hash = ContentHash::from(<[u8; 32]>::from(hasher.finalize()));
            let content = String::from_utf8(bytes).map_err(|source| GitError::NonUtf8Current {
                path: path.relative().to_path_buf(),
                source,
            })?;
            Ok((content, Some(hash)))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), None)),
        Err(source) => Err(GitError::ReadRestoreTarget {
            path: path.relative().to_path_buf(),
            source,
        }),
    }
}

/// One explicit or debounced commit outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommitResult {
    pub commit_id: Option<String>,
    pub committed_paths: Vec<PathBuf>,
    pub message: Option<String>,
}

/// Validated inputs for one forward-only historical restoration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRestoreRequest {
    pub path: VaultPath,
    pub reference: String,
    pub dry_run: bool,
}

/// Historical restoration preview or applied outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRestoreResult {
    pub diff: String,
    pub applied: bool,
    pub events: Vec<OperationEvent>,
    pub warnings: Vec<OperationWarning>,
}

fn entry_is_within(entry: &IndexEntry, path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let normalised = path.replace('\\', "/");
    entry.path == normalised.as_bytes()
        || entry.path.starts_with(format!("{normalised}/").as_bytes())
}

fn generated_commit_message(events: &[OperationEvent]) -> String {
    let count = events.len();
    let noun = if count == 1 {
        "operation"
    } else {
        "operations"
    };
    let mut message = format!("mcp: {count} {noun}");
    if !events.is_empty() {
        message.push_str("\n\n");
        for event in events {
            message.push_str(&event.summary);
            message.push('\n');
        }
    }
    message
}

impl<C> VersionsVault for Git2Vault<C>
where
    C: Clock,
{
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        self.stage_event(event).map_err(OperationWarning::from)
    }
}

/// Typed local Git adapter failures.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("Git author name must not be empty")]
    InvalidAuthorName,
    #[error("Git author email must contain a local and domain part")]
    InvalidAuthorEmail,
    #[error("Git root is not present in the configured vault set")]
    RootNotConfigured,
    #[error("Git repository operation failed")]
    Repository(#[source] git2::Error),
    #[error("vault is not a Git repository")]
    NotRepository,
    #[error(".gitignore could not be read: {path}")]
    ReadIgnore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(".gitignore is not valid UTF-8: {path}")]
    NonUtf8Ignore {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("Git-managed path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    #[error("Git-managed path is invalid")]
    Path(#[source] contextos_core::PathError),
    #[error("Git operation path belongs to another vault")]
    WrongVault,
    #[error("Git protected restore paths must be non-empty relative paths without traversal")]
    InvalidProtectedRestorePath,
    #[error("Git pending-path state is unavailable")]
    PendingStateUnavailable,
    #[error("Git pending-path metadata could not be accessed")]
    PendingMetadata(#[source] std::io::Error),
    #[error("Git pending-path metadata is invalid")]
    InvalidPendingMetadata,
    #[error("Git status contains a path that is not valid UTF-8")]
    NonUtf8StatusPath,
    #[error("Git diff byte limit must be greater than zero")]
    InvalidDiffLimit,
    #[error("Git log limit must be greater than zero")]
    InvalidLogLimit,
    #[error("historical Git content is not UTF-8: {path}")]
    NonUtf8Restore {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("current restore target is not UTF-8: {path}")]
    NonUtf8Current {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("current restore target could not be read: {path}")]
    ReadRestoreTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("restore target could not be inspected: {path}")]
    WalkRestoreTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("historical Git object is not a file or directory")]
    UnsupportedRestoreObject,
    #[error("restore would delete MCP-owned paths but destructive restore is disabled")]
    DestructiveRestoreDisabled,
    #[error("operation-log content is append-only and cannot be selected for Git restore")]
    ProtectedRestorePath,
}

impl GitError {
    /// Returns a stable machine-readable Git error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Path(error) => error.code(),
            Self::InvalidAuthorName
            | Self::InvalidAuthorEmail
            | Self::RootNotConfigured
            | Self::InvalidProtectedRestorePath
            | Self::Repository(_)
            | Self::ReadIgnore { .. }
            | Self::NonUtf8Ignore { .. }
            | Self::NonUtf8Path { .. } => "git/init",
            Self::NotRepository => "git/not-a-repo",
            Self::WrongVault
            | Self::PendingStateUnavailable
            | Self::PendingMetadata(_)
            | Self::InvalidPendingMetadata => "git/stage",
            Self::NonUtf8StatusPath | Self::InvalidDiffLimit | Self::InvalidLogLimit => {
                "git/inspect"
            }
            Self::NonUtf8Restore { .. }
            | Self::NonUtf8Current { .. }
            | Self::ReadRestoreTarget { .. }
            | Self::WalkRestoreTarget { .. }
            | Self::UnsupportedRestoreObject
            | Self::DestructiveRestoreDisabled
            | Self::ProtectedRestorePath => "git/restore",
        }
    }
}

impl From<GitError> for OperationWarning {
    fn from(value: GitError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}

/// A Git operation that can additionally fail at the vault write boundary.
#[derive(Debug, Error)]
pub enum GitWriteError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("Git-requested vault content could not be persisted")]
    Write(#[source] E),
}

impl<E> GitWriteError<E>
where
    E: std::error::Error + 'static,
{
    /// Returns a stable machine-readable write-operation error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Git(error) => error.code(),
            Self::Write(_) => "git/write",
        }
    }
}
