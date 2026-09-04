#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use contextos_core::{
    ListsVault, MaintainsIndexes, MoveMutation, MovesVault, OperationEvent, OperationWarning, Origin, ReadsVault,
    VaultEntry, VaultEntryKind, VaultPath, VaultPathInput, VaultRoot, VaultRootId, VaultSet, WriteMutation,
    WritesVault,
};
use contextos_obsidian::{FrontmatterDocument, FrontmatterError};
use thiserror::Error;

const BEGIN_MARKER: &str = "<!-- contextos:index:begin -->";
const END_MARKER: &str = "<!-- contextos:index:end -->";

/// Filesystem kind represented by a managed index row.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IndexEntryKind {
    Directory,
    File,
}

/// One currently discovered directory entry and its derived summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub kind: IndexEntryKind,
    pub suggested_summary: String,
}

/// Trusted inputs for one pure managed-block reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexReconcileInput<'a> {
    pub source: Option<&'a str>,
    pub directory_name: &'a str,
    pub entries: Vec<IndexEntry>,
}

/// A complete reconciled `index.md` document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexDocument(String);

/// Inputs used to derive a new managed-row summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexSummaryInput<'a> {
    pub filename: &'a str,
    pub source: Option<&'a str>,
}

/// One single-line summary suitable for a managed index row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexSummary(String);

/// Auditable outcome of one recursive managed-index rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexRebuildResult {
    pub directories_scanned: usize,
    pub indexes_updated: usize,
    pub indexes_created: usize,
    pub skipped: usize,
    pub events: Vec<OperationEvent>,
}

#[derive(Debug, Default)]
struct RebuildState {
    directories_scanned: usize,
    indexes_updated: usize,
    indexes_created: usize,
    skipped: usize,
    events: Vec<OperationEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexWrite {
    Unchanged,
    Created,
    Updated,
}

impl From<RebuildState> for IndexRebuildResult {
    fn from(value: RebuildState) -> Self {
        Self {
            directories_scanned: value.directories_scanned,
            indexes_updated: value.indexes_updated,
            indexes_created: value.indexes_created,
            skipped: value.skipped,
            events: value.events,
        }
    }
}

/// Dependencies and policy for managed-index persistence.
#[derive(Clone, Debug)]
pub struct IndexServiceConfig<R, W> {
    pub root: VaultRoot,
    pub roots: VaultSet,
    pub reader: R,
    pub writer: W,
    pub excluded: Vec<String>,
}

/// Managed-index application service using only consumer-owned vault ports.
#[derive(Clone, Debug)]
pub struct IndexService<R, W> {
    root: VaultRoot,
    root_id: VaultRootId,
    roots: VaultSet,
    reader: R,
    writer: W,
    excluded: Vec<String>,
}

impl<R, W> TryFrom<IndexServiceConfig<R, W>> for IndexService<R, W>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
{
    type Error = IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>;

    fn try_from(value: IndexServiceConfig<R, W>) -> Result<Self, Self::Error> {
        let root_index = value
            .roots
            .iter()
            .position(|candidate| candidate == &value.root)
            .ok_or(IndexServiceError::RootNotConfigured)?;
        let root_id = VaultRootId::try_from(root_index).map_err(IndexServiceError::Path)?;
        Ok(Self {
            root: value.root,
            root_id,
            roots: value.roots,
            reader: value.reader,
            writer: value.writer,
            excluded: value.excluded,
        })
    }
}

impl<R, W> IndexService<R, W>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
{
    /// Rebuilds every non-excluded directory below the selected subtree.
    ///
    /// # Errors
    ///
    /// Returns a typed discovery, parse, path, or persistence error.
    pub fn rebuild(
        &self,
        directory: &VaultPath,
        origin: Origin,
    ) -> Result<Vec<OperationEvent>, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        Ok(self.rebuild_report(directory, origin, false)?.events)
    }

    /// Rebuilds the selected subtree and reports exact scan and write counts.
    ///
    /// # Errors
    ///
    /// Returns a typed discovery, parse, path, or persistence error.
    pub fn rebuild_report(
        &self,
        directory: &VaultPath,
        _origin: Origin,
        dry_run: bool,
    ) -> Result<IndexRebuildResult, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        if directory.root_id() != self.root_id {
            return Err(IndexServiceError::WrongRoot);
        }
        let mut state = RebuildState::default();
        self.rebuild_directory(directory, &mut state, dry_run)?;
        Ok(IndexRebuildResult::from(state))
    }

    fn rebuild_directory(
        &self,
        directory: &VaultPath,
        state: &mut RebuildState,
        dry_run: bool,
    ) -> Result<(), IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        state.directories_scanned = state.directories_scanned.saturating_add(1);
        let mut listing = self.reader.list(directory).map_err(IndexServiceError::Read)?;
        let has_legacy = self.migrate_legacy_index(directory, &listing, &mut state.events, dry_run)?;
        if has_legacy && !dry_run {
            listing = self.reader.list(directory).map_err(IndexServiceError::Read)?;
        }
        state.skipped = state.skipped.saturating_add(
            listing
                .iter()
                .filter(|entry| self.is_config_excluded(directory, entry))
                .count(),
        );
        let visible = listing
            .into_iter()
            .filter(|entry| self.includes(directory, entry))
            .collect::<Vec<_>>();
        match self.persist_directory(directory, &visible, &mut state.events, dry_run, has_legacy)? {
            IndexWrite::Unchanged => {}
            IndexWrite::Created => {
                state.indexes_created = state.indexes_created.saturating_add(1);
            }
            IndexWrite::Updated => {
                state.indexes_updated = state.indexes_updated.saturating_add(1);
            }
        }
        for entry in visible.iter().filter(|entry| entry.kind == VaultEntryKind::Directory) {
            let child = self.child_path(directory, &entry.name)?;
            self.rebuild_directory(&child, state, dry_run)?;
        }
        Ok(())
    }

    fn persist_directory(
        &self,
        directory: &VaultPath,
        entries: &[VaultEntry],
        events: &mut Vec<OperationEvent>,
        dry_run: bool,
        legacy_preview: bool,
    ) -> Result<IndexWrite, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let index_path = self.child_path(directory, "index.md")?;
        let existing_path = if legacy_preview && dry_run {
            self.child_path(directory, "_index.md")?
        } else {
            index_path.clone()
        };
        let existing = self
            .reader
            .read_optional_text(&existing_path)
            .map_err(IndexServiceError::Read)?;
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries.iter().filter(|entry| entry.name != "index.md") {
            let is_markdown = Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
            let source = if entry.kind == VaultEntryKind::File && is_markdown {
                let child = self.child_path(directory, &entry.name)?;
                self.reader
                    .read_optional_text(&child)
                    .map_err(IndexServiceError::Read)?
                    .map(|text| text.content)
            } else {
                None
            };
            let summary = IndexSummary::try_from(IndexSummaryInput {
                filename: &entry.name,
                source: source.as_deref(),
            })
            .map_err(|source| IndexError::Frontmatter {
                path: directory.relative().join(&entry.name),
                source,
            })?;
            rows.push(IndexEntry {
                name: entry.name.clone(),
                kind: match entry.kind {
                    VaultEntryKind::File => IndexEntryKind::File,
                    VaultEntryKind::Directory => IndexEntryKind::Directory,
                },
                suggested_summary: <&str>::from(&summary).to_owned(),
            });
        }
        let directory_name = directory
            .relative()
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("Vault");
        let document = IndexDocument::try_from(IndexReconcileInput {
            source: existing.as_ref().map(|text| text.content.as_str()),
            directory_name,
            entries: rows,
        })?;
        let content = String::from(document);
        if existing.as_ref().is_some_and(|text| text.content == content) {
            return Ok(if dry_run && legacy_preview {
                IndexWrite::Updated
            } else {
                IndexWrite::Unchanged
            });
        }
        let created = existing.is_none();
        if dry_run {
            return Ok(if created {
                IndexWrite::Created
            } else {
                IndexWrite::Updated
            });
        }
        let result = self
            .writer
            .persist(&WriteMutation {
                path: index_path,
                content,
                expected_hash: existing.map(|text| text.content_hash),
                force: false,
                origin: Origin::Internal("index".to_owned()),
            })
            .map_err(IndexServiceError::Write)?;
        if let Some(event) = result.event {
            events.push(event);
        }
        Ok(if created {
            IndexWrite::Created
        } else {
            IndexWrite::Updated
        })
    }

    fn reconcile_event(
        &self,
        event: &OperationEvent,
    ) -> Result<Vec<OperationEvent>, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let mut directories = Vec::new();
        for path in &event.paths {
            if path.root_id() != self.root_id {
                continue;
            }
            let absolute = <&Path>::from(path);
            if absolute.is_dir() && !self.is_excluded(path.relative()) {
                directories.push(path.clone());
            }
            let mut ancestor = path.relative().parent().map(Path::to_path_buf);
            while let Some(relative) = ancestor {
                let next = relative.parent().map(Path::to_path_buf);
                let parent = self.path_from_relative(&relative)?;
                if !self.is_excluded(parent.relative()) && !directories.iter().any(|directory| directory == &parent) {
                    directories.push(parent);
                }
                ancestor = next;
            }
        }

        let mut events = Vec::new();
        for directory in directories {
            let mut listing = self.reader.list(&directory).map_err(IndexServiceError::Read)?;
            if self.migrate_legacy_index(&directory, &listing, &mut events, false)? {
                listing = self.reader.list(&directory).map_err(IndexServiceError::Read)?;
            }
            let visible = listing
                .into_iter()
                .filter(|entry| self.includes(&directory, entry))
                .collect::<Vec<_>>();
            self.persist_directory(&directory, &visible, &mut events, false, false)?;
        }
        Ok(events)
    }

    fn includes(&self, directory: &VaultPath, entry: &VaultEntry) -> bool {
        if entry.name.starts_with('.') || matches!(entry.name.as_str(), "index.md" | "_index.md") {
            return false;
        }
        let relative = directory.relative().join(&entry.name);
        !self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    fn is_config_excluded(&self, directory: &VaultPath, entry: &VaultEntry) -> bool {
        let relative = directory.relative().join(&entry.name);
        self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    fn child_path(
        &self,
        directory: &VaultPath,
        name: &str,
    ) -> Result<VaultPath, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let relative = directory.relative().join(name);
        let absolute = self.root.path().join(&relative);
        let raw = absolute
            .to_str()
            .ok_or_else(|| IndexServiceError::NonUtf8Path { path: absolute.clone() })?;
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(IndexServiceError::Path)
    }

    fn path_from_relative(
        &self,
        relative: &Path,
    ) -> Result<VaultPath, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let absolute = self.root.path().join(relative);
        let raw = absolute
            .to_str()
            .ok_or_else(|| IndexServiceError::NonUtf8Path { path: absolute.clone() })?;
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(IndexServiceError::Path)
    }

    fn is_excluded(&self, relative: &Path) -> bool {
        self.excluded.iter().any(|excluded| {
            let excluded = Path::new(excluded);
            relative == excluded || relative.starts_with(excluded)
        })
    }

    /// Reports whether this service would maintain `directory`'s managed
    /// `index.md`: the directory belongs to this service's own
    /// root and is not excluded from index maintenance. The guarded
    /// delete path uses this to decide whether a directory containing only
    /// `index.md`/`_index.md` may be treated as empty for deletion
    /// purposes; a directory this returns `false` for keeps its literal
    /// on-disk contents authoritative.
    #[must_use]
    pub fn manages_directory(&self, directory: &VaultPath) -> bool {
        directory.root_id() == self.root_id && !self.is_excluded(directory.relative())
    }

    fn migrate_legacy_index(
        &self,
        directory: &VaultPath,
        listing: &[VaultEntry],
        events: &mut Vec<OperationEvent>,
        dry_run: bool,
    ) -> Result<bool, IndexServiceError<<R as ReadsVault>::Error, <W as WritesVault>::Error>> {
        let has_legacy = listing.iter().any(|entry| entry.name == "_index.md");
        if !has_legacy {
            return Ok(false);
        }
        if listing.iter().any(|entry| entry.name == "index.md") {
            return Err(IndexServiceError::LegacyIndexConflict {
                directory: directory.relative().to_path_buf(),
            });
        }

        if dry_run {
            return Ok(true);
        }

        let result = self
            .writer
            .move_path(&MoveMutation {
                source: self.child_path(directory, "_index.md")?,
                destination: self.child_path(directory, "index.md")?,
                origin: Origin::Internal("index".to_owned()),
            })
            .map_err(IndexServiceError::Write)?;
        if let Some(event) = result.event {
            events.push(event);
        }
        Ok(true)
    }
}

impl<R, W> MaintainsIndexes for IndexService<R, W>
where
    R: ReadsVault + ListsVault<Error = <R as ReadsVault>::Error>,
    <R as ReadsVault>::Error: std::error::Error + 'static,
    W: WritesVault + MovesVault<Error = <W as WritesVault>::Error>,
    <W as WritesVault>::Error: std::error::Error + 'static,
{
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.reconcile_event(event).map_err(OperationWarning::from)
    }
}

/// Typed managed-index application-service failures.
#[derive(Debug, Error)]
pub enum IndexServiceError<R, W>
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    #[error("vault content could not be read for index reconciliation")]
    Read(#[source] R),
    #[error("managed index could not be persisted")]
    Write(#[source] W),
    #[error("managed index path is invalid")]
    Path(#[source] contextos_core::PathError),
    #[error("managed index path is not valid UTF-8: {path}")]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("managed index root is not present in the configured vault set")]
    RootNotConfigured,
    #[error("managed index request selected a different vault root")]
    WrongRoot,
    #[error("both _index.md and index.md exist in {directory}; merge or rename one before rebuilding")]
    LegacyIndexConflict { directory: std::path::PathBuf },
    #[error(transparent)]
    Index(#[from] IndexError),
}

impl<R, W> IndexServiceError<R, W>
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    /// Returns a stable machine-readable service error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Index(error) => error.code(),
            Self::LegacyIndexConflict { .. } => "index/legacy-conflict",
            Self::RootNotConfigured | Self::WrongRoot => "index/root",
            Self::Read(_) | Self::Write(_) | Self::Path(_) | Self::NonUtf8Path { .. } => "index/reconcile",
        }
    }
}

impl<R, W> From<IndexServiceError<R, W>> for OperationWarning
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    fn from(value: IndexServiceError<R, W>) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}

impl<'a> From<&'a IndexSummary> for &'a str {
    fn from(value: &'a IndexSummary) -> Self {
        &value.0
    }
}

impl TryFrom<IndexSummaryInput<'_>> for IndexSummary {
    type Error = FrontmatterError;

    fn try_from(value: IndexSummaryInput<'_>) -> Result<Self, Self::Error> {
        if let Some(source) = value.source {
            let document = FrontmatterDocument::try_from(source)?;
            let title = document
                .frontmatter()
                .get("title")
                .and_then(|property| property.as_str())
                .map(str::trim)
                .filter(|title| !title.is_empty());
            let heading = first_heading(document.body());
            let sentence = first_sentence(document.body());
            if let Some(summary) = descriptive_summary(title.or(heading), sentence.as_deref()) {
                return Ok(Self(summary));
            }
        }

        let stem = value.filename.strip_suffix(".md").unwrap_or(value.filename);
        Ok(Self(directory_title(stem)))
    }
}

impl TryFrom<IndexReconcileInput<'_>> for IndexDocument {
    type Error = IndexError;

    fn try_from(mut value: IndexReconcileInput<'_>) -> Result<Self, Self::Error> {
        validate_entries(&value.entries)?;
        value.entries.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });

        match value.source {
            None => {
                let block = render_block(&value.entries, &HashMap::new());
                Ok(Self(format!(
                    "# {}: Index\n\n{block}\n",
                    directory_title(value.directory_name)
                )))
            }
            Some(source) => reconcile_existing(source, &value.entries),
        }
    }
}

impl From<IndexDocument> for String {
    fn from(value: IndexDocument) -> Self {
        value.0
    }
}

fn validate_entries(entries: &[IndexEntry]) -> Result<(), IndexError> {
    let mut names = HashSet::with_capacity(entries.len());
    for entry in entries {
        if entry.name.is_empty() || entry.name.contains(['\r', '\n']) {
            return Err(IndexError::InvalidEntryName {
                name: entry.name.clone(),
            });
        }
        if !names.insert(entry.name.clone()) {
            return Err(IndexError::DuplicateEntry {
                name: entry.name.clone(),
            });
        }
    }
    Ok(())
}

fn reconcile_existing(source: &str, entries: &[IndexEntry]) -> Result<IndexDocument, IndexError> {
    let begin = source.find(BEGIN_MARKER);
    let end = source.find(END_MARKER);
    match (begin, end) {
        (None, None) => {
            let mut content = source.to_owned();
            if !content.ends_with('\n') {
                content.push('\n');
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("## Contents\n\n");
            content.push_str(&render_block(entries, &HashMap::new()));
            content.push('\n');
            Ok(IndexDocument(content))
        }
        (Some(begin), Some(end)) if begin < end => {
            let end_after_marker = end.saturating_add(END_MARKER.len());
            if source[end_after_marker..].contains(BEGIN_MARKER)
                || source[end_after_marker..].contains(END_MARKER)
                || source[begin.saturating_add(BEGIN_MARKER.len())..end].contains(BEGIN_MARKER)
            {
                return Err(IndexError::InvalidMarkers);
            }
            let summaries = existing_summaries(
                &source[begin.saturating_add(BEGIN_MARKER.len())..end],
                source[..begin].lines().count().saturating_add(1),
            )?;
            let block = render_block(entries, &summaries);
            let mut content = String::with_capacity(
                source
                    .len()
                    .saturating_sub(end_after_marker.saturating_sub(begin))
                    .saturating_add(block.len()),
            );
            content.push_str(&source[..begin]);
            content.push_str(&block);
            content.push_str(&source[end_after_marker..]);
            Ok(IndexDocument(content))
        }
        _ => Err(IndexError::InvalidMarkers),
    }
}

fn existing_summaries(block: &str, first_line: usize) -> Result<HashMap<String, String>, IndexError> {
    let mut summaries = HashMap::new();
    for (line_index, line) in block.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "| Item | Summary |" || trimmed == "| --- | --- |" {
            continue;
        }
        let Some(row) = trimmed.strip_prefix("| [") else {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        };
        let Some(destination_start) = row.find("](") else {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        };
        let destination = &row[destination_start.saturating_add(2)..];
        let Some(destination_end) = destination.find(") | ") else {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        };
        let target = &destination[..destination_end];
        let summary_with_end = &destination[destination_end.saturating_add(4)..];
        let Some(summary) = summary_with_end.strip_suffix(" |") else {
            return Err(IndexError::InvalidRow {
                line: first_line.saturating_add(line_index),
            });
        };
        let name = target.strip_suffix("/index.md").unwrap_or(target);
        summaries.insert(name.to_owned(), summary.to_owned());
    }
    Ok(summaries)
}

fn render_block(entries: &[IndexEntry], existing: &HashMap<String, String>) -> String {
    let mut block = String::from(BEGIN_MARKER);
    block.push_str("\n| Item | Summary |\n| --- | --- |");
    for entry in entries {
        let summary = match existing.get(&entry.name) {
            Some(summary) => summary.clone(),
            None => format!("{} <!-- auto -->", single_line_summary(&entry.suggested_summary)),
        };
        block.push('\n');
        match entry.kind {
            IndexEntryKind::Directory => {
                block.push_str("| [");
                block.push_str(&entry.name);
                block.push_str("/](");
                block.push_str(&entry.name);
                block.push_str("/index.md) | ");
            }
            IndexEntryKind::File => {
                block.push_str("| [");
                block.push_str(&entry.name);
                block.push_str("](");
                block.push_str(&entry.name);
                block.push_str(") | ");
            }
        }
        block.push_str(&summary);
        block.push_str(" |");
    }
    block.push('\n');
    block.push_str(END_MARKER);
    block
}

fn single_line_summary(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

fn directory_title(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_heading(body: &str) -> Option<&str> {
    body.lines().find_map(|line| {
        line.trim()
            .strip_prefix('#')
            .map(str::trim)
            .filter(|heading| !heading.is_empty())
    })
}

fn first_sentence(body: &str) -> Option<String> {
    let mut paragraph = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    if paragraph.is_empty() {
        return None;
    }
    let sentence_end = paragraph
        .char_indices()
        .find(|(_, character)| matches!(character, '.' | '!' | '?'))
        .map_or(paragraph.len(), |(index, character)| {
            index.saturating_add(character.len_utf8())
        });
    Some(paragraph[..sentence_end].to_owned())
}

fn descriptive_summary(title: Option<&str>, sentence: Option<&str>) -> Option<String> {
    match (title, sentence) {
        (Some(title), Some(sentence)) if title != sentence => Some(format!("{title}: {sentence}")),
        (Some(title), Some(_) | None) => Some(title.to_owned()),
        (None, Some(sentence)) => Some(sentence.to_owned()),
        (None, None) => None,
    }
}

/// Typed managed-index reconciliation failures.
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("managed index markers are missing, duplicated, or out of order")]
    InvalidMarkers,
    #[error("managed index row on line {line} is malformed")]
    InvalidRow { line: usize },
    #[error("index entry name is invalid: {name}")]
    InvalidEntryName { name: String },
    #[error("index entry appears more than once: {name}")]
    DuplicateEntry { name: String },
    /// `path` is the offending file's vault-relative path, attached at the
    /// one call site (`IndexService::persist_directory`) that has both the
    /// containing directory and the entry's filename in scope;
    /// `IndexSummary::try_from` itself only ever sees a bare filename, so
    /// it cannot build this path on its own (hence `FrontmatterError`
    /// rather than `Self` as that conversion's error type).
    #[error("YAML frontmatter is invalid in {path}: {source}")]
    Frontmatter {
        path: std::path::PathBuf,
        #[source]
        source: FrontmatterError,
    },
}

impl IndexError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Frontmatter { source, .. } => source.code(),
            Self::InvalidMarkers
            | Self::InvalidRow { .. }
            | Self::InvalidEntryName { .. }
            | Self::DuplicateEntry { .. } => "index/stale",
        }
    }

    /// Returns an actionable recovery instruction.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::Frontmatter { source, .. } => source.remediation(),
            Self::InvalidMarkers
            | Self::InvalidRow { .. }
            | Self::InvalidEntryName { .. }
            | Self::DuplicateEntry { .. } => "Correct the managed index block or rebuild the affected subtree.",
        }
    }
}
