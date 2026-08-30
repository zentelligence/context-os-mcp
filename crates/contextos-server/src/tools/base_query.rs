//! Vault-scan orchestration for the `base_query` MCP tool
//! (`tools::obsidian::query_base`): resolves the scan root, enumerates
//! candidate notes, evaluates each against
//! `contextos_obsidian::base_query`'s pure filter evaluator, sorts and
//! limits the matches, and renders the requested representation. Kept
//! separate from `tools/obsidian.rs` so that file's tool handlers stay as
//! thin as its siblings.

use std::cmp::Ordering;
use std::sync::Arc;

use contextos_core::{VaultPath, VaultPathInput, VaultRootId, VaultSet};
use contextos_fs::{FileInfoRequest, Filesystem, ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::{
    FileMetadata, FrontmatterDocument, LinkCollection, QueryDefinition, QueryFormat, RowContext,
    ScanRootHint, SortKey, compare_values, evaluate_filters, render, resolve_property, resolve_row,
    scan_root_hint,
};
use contextos_search::{SearchError, VaultSearchService};
use serde_json::{Map, Value};

use crate::tool_error::ToolError;

/// The `file.*` property name that, alone among the documented set,
/// structurally requires the link graph (backlinks cannot be derived from
/// a candidate's own content). Referenced by [`references_file_backlinks`].
const FILE_BACKLINKS_PROPERTY: &str = "file.backlinks";

/// Whether `definition` references [`FILE_BACKLINKS_PROPERTY`] anywhere a
/// row could need it: a display column, a sort key, or (via a conservative
/// substring check on the still-unparsed filter tree) a filter. Used to
/// decide, once per call rather than once per candidate, whether the link
/// graph must be resolved up front.
pub(crate) fn references_file_backlinks(definition: &QueryDefinition) -> bool {
    definition
        .columns
        .iter()
        .any(|column| column == FILE_BACKLINKS_PROPERTY)
        || definition
            .sort
            .iter()
            .any(|key| key.property == FILE_BACKLINKS_PROPERTY)
        || definition
            .filters
            .as_ref()
            .is_some_and(|filters| filters.to_string().contains(FILE_BACKLINKS_PROPERTY))
}

/// Safety cap on how many candidate files one `base_query` call reads and
/// evaluates. Reaching it sets the result's `truncated` flag rather than
/// silently scanning only part of the vault without saying so.
const MAX_CANDIDATE_FILES: usize = 5_000;

const CANDIDATE_PATTERN: &str = "**/*.md";

/// The rendered outcome of one `base_query` call, ready to become
/// [`crate::tools::obsidian_types::BaseQueryToolResult`]. `diagnostics`
/// carries one `format/frontmatter` entry per candidate whose frontmatter
/// failed to parse (the row is still included, with every frontmatter-
/// backed column resolving to `null`, per [`load_row`]'s existing
/// graceful-degradation policy) — surfaced so a caller sees *why* those
/// columns are empty rather than silently getting `null` with no
/// explanation.
pub(crate) struct QueryOutcome {
    pub(crate) content: String,
    pub(crate) columns: Vec<String>,
    pub(crate) matched: usize,
    pub(crate) truncated: bool,
    pub(crate) diagnostics: Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
}

/// One matched row's owned data, kept alive across the scan so it can be
/// sorted and rendered once every candidate file has been evaluated.
struct MatchedRow {
    frontmatter: Map<String, Value>,
    name: String,
    basename: String,
    path: String,
    ext: String,
    folder: String,
    size: u64,
    ctime: Option<String>,
    mtime: Option<String>,
    tags: Vec<String>,
    links: Vec<String>,
    embeds: Vec<String>,
    backlinks: Vec<String>,
}

impl MatchedRow {
    fn row_context(&self) -> RowContext<'_> {
        RowContext {
            frontmatter: &self.frontmatter,
            file: FileMetadata {
                name: &self.name,
                basename: &self.basename,
                path: &self.path,
                ext: &self.ext,
                folder: &self.folder,
                size: self.size,
                ctime: self.ctime.as_deref(),
                mtime: self.mtime.as_deref(),
            },
            tags: &self.tags,
            links: &self.links,
            embeds: &self.embeds,
            backlinks: &self.backlinks,
        }
    }
}

/// Runs one `base_query`: resolves the scan root (narrowing it to a
/// `file.path == "..."` filter hint when present, per
/// [`scan_root_hint`]'s safety contract), enumerates markdown files,
/// evaluates each against `definition.filters`, sorts and limits the
/// matches, and renders `format`. `search` is the calling vault's
/// per-root search services (`ContextOsServer::search`); the link graph is
/// resolved from it, once, only when [`references_file_backlinks`] finds
/// `definition` actually needs it, and only for the same vault `root_id`
/// already resolves to. [`QueryOutcome::diagnostics`] carries one entry
/// per scanned candidate whose frontmatter failed to parse, see
/// [`load_row`].
///
/// # Errors
///
/// Returns a [`ToolError`] for any filesystem failure reading the vault
/// root itself, [`ToolError::SearchDisabled`] when `definition` references
/// `file.backlinks` but this vault has no search service configured, or
/// any [`contextos_obsidian::BaseQueryError`] raised while evaluating a
/// filter, sort key, or display column.
pub(crate) fn run(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    definition: &QueryDefinition,
    format: QueryFormat,
    search: &[Option<Arc<VaultSearchService>>],
) -> Result<QueryOutcome, ToolError> {
    let graph = if references_file_backlinks(definition) {
        Some(
            search
                .get(usize::try_from(root_id)?)
                .cloned()
                .flatten()
                .ok_or(ToolError::SearchDisabled)?,
        )
    } else {
        None
    };
    let vault_root = join_relative(roots, root_id, "")?;
    let narrowing = scan_root_hint(definition.filters.as_ref()).and_then(|hint| {
        let directory = match hint {
            ScanRootHint::Path(value) => candidate_directory(value),
            ScanRootHint::Folder(value) => (!value.is_empty()).then_some(value),
        }?;
        let narrowed_root = join_relative(roots, root_id, directory).ok()?;
        Some((directory.to_owned(), narrowed_root))
    });

    let (prefix, candidates) = match narrowing {
        Some((directory, narrowed_root)) => match search_candidates(filesystem, &narrowed_root) {
            Ok(candidates) => (format!("{directory}/"), candidates),
            // The hinted directory does not exist, is not a directory, or
            // is otherwise unusable: fall back to the unnarrowed vault
            // root. `scan_root_hint` is documented as optimisation-only,
            // so a failed narrowing attempt must never surface as a
            // `base_query` error, only as a slower scan.
            Err(_) => (String::new(), search_candidates(filesystem, &vault_root)?),
        },
        None => (String::new(), search_candidates(filesystem, &vault_root)?),
    };

    let truncated = candidates.len() > MAX_CANDIDATE_FILES;
    let candidates = &candidates[..candidates.len().min(MAX_CANDIDATE_FILES)];

    let mut matched = Vec::new();
    let mut diagnostics = Vec::new();
    for candidate in candidates {
        let relative = format!("{prefix}{candidate}");
        let Some(row) = load_row(
            filesystem,
            roots,
            root_id,
            &relative,
            graph.as_deref(),
            &mut diagnostics,
        )?
        else {
            continue;
        };
        if evaluate_filters(definition.filters.as_ref(), &row.row_context())? {
            matched.push(row);
        }
    }

    sort_rows(&definition.sort, &mut matched)?;
    let matched_count = matched.len();
    let limited = match definition.limit {
        Some(limit) => matched.into_iter().take(limit).collect::<Vec<_>>(),
        None => matched,
    };

    let mut rows = Vec::with_capacity(limited.len());
    for row in &limited {
        rows.push(resolve_row(&definition.columns, &row.row_context())?);
    }
    let content = render(&definition.columns, &rows, format);

    Ok(QueryOutcome {
        content,
        columns: definition.columns.clone(),
        matched: matched_count,
        truncated,
        diagnostics,
    })
}

/// Resolves `suffix` (vault-root-relative, forward-slash separated; empty
/// for the vault root itself) as a [`VaultPath`], via the same "absolute
/// root path, then re-resolve" approach `links_read` already uses
/// (`tools/obsidian.rs::read_links`) to obtain an unambiguous whole-vault
/// `VaultPath` from a root id alone.
fn join_relative(
    roots: &VaultSet,
    root_id: VaultRootId,
    suffix: &str,
) -> Result<VaultPath, ToolError> {
    let root = roots
        .iter()
        .nth(usize::try_from(root_id)?)
        .ok_or(ToolError::Invalid("configured root not found"))?;
    let absolute = if suffix.is_empty() {
        root.path().to_path_buf()
    } else {
        root.path().join(suffix)
    };
    let raw = absolute
        .to_str()
        .ok_or(ToolError::Invalid("vault path must be valid UTF-8"))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

fn search_candidates(filesystem: &Filesystem, root: &VaultPath) -> Result<Vec<String>, ToolError> {
    Ok(filesystem.search_files(&SearchFilesRequest {
        path: root.clone(),
        pattern: CANDIDATE_PATTERN.to_owned(),
        exclude_patterns: vec![".git/**".to_owned(), ".contextos/**".to_owned()],
        max_results: MAX_CANDIDATE_FILES.saturating_add(1),
    })?)
}

/// Returns the parent directory of a [`ScanRootHint::Path`] value, or
/// `None` when the hint has no `/` to derive a safe subdirectory from (a
/// bare filename hint is left unnarrowed rather than guessed at).
/// [`ScanRootHint::Folder`] values are already a directory and are used
/// directly by the caller instead.
fn candidate_directory(hint: &str) -> Option<&str> {
    if let Some(stripped) = hint.strip_suffix('/') {
        return (!stripped.is_empty()).then_some(stripped);
    }
    hint.rsplit_once('/')
        .map(|(directory, _)| directory)
        .filter(|directory| !directory.is_empty())
}

/// Reads one candidate file and builds its [`MatchedRow`], or `None` when
/// the file cannot be read (permissions, size limits, or a concurrent
/// deletion mid-scan): a single unreadable candidate is skipped rather
/// than failing the whole query, the same graceful-degradation discipline
/// `contextos_search::IndexedDocument` already applies to indexing.
/// Frontmatter that fails to parse degrades to an empty frontmatter map
/// with the whole file treated as body, appending a `format/frontmatter`
/// entry to `diagnostics` naming the candidate — every frontmatter-backed
/// column resolves to `null` for this row, and any filter referencing a
/// frontmatter property silently cannot match it, so the caller needs to
/// know why rather than read an empty result as "genuinely no matches".
/// Malformed wikilink syntax degrades `links`/`embeds` to empty lists
/// rather than failing the row (no diagnostic: less consequential, and
/// `links_read` already reports malformed-link errors directly for a
/// caller who cares); a candidate not yet present in the link graph
/// degrades `backlinks` to an empty list for the same reason, since
/// `base_query` runs a fresh filesystem scan independent of the graph's
/// own index state. Any other graph failure (the graph specifically
/// disabled for this vault, distinct from the whole-vault
/// [`ToolError::SearchDisabled`] gate `run` already applies before
/// scanning) fails the whole query rather than silently reporting empty
/// backlinks as a real answer.
fn load_row(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    relative: &str,
    graph: Option<&VaultSearchService>,
    diagnostics: &mut Vec<crate::tools::diagnostics::StructuredDiagnosticToolResult>,
) -> Result<Option<MatchedRow>, ToolError> {
    let path = join_relative(roots, root_id, relative)?;
    let Ok(source) = filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: None,
    }) else {
        return Ok(None);
    };
    let Ok(info) = filesystem.file_info(&FileInfoRequest { path }) else {
        return Ok(None);
    };
    let (frontmatter, body) = match FrontmatterDocument::try_from(source.content.as_str()) {
        Ok(parsed) => (parsed.frontmatter().clone(), parsed.body().to_owned()),
        Err(error) => {
            diagnostics.push(crate::tools::diagnostics::StructuredDiagnosticToolResult {
                code: error.code().to_owned(),
                path: relative.to_owned(),
                message: error.to_string(),
            });
            (Map::new(), source.content)
        }
    };
    let tags = contextos_core::extract_tags(&frontmatter, &body);
    let file_path = std::path::Path::new(relative);
    let name = file_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let basename = file_path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let ext = file_path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned();
    let folder = file_path
        .parent()
        .and_then(std::path::Path::to_str)
        .unwrap_or_default()
        .to_owned();
    let (links, embeds) = match LinkCollection::try_from(body.as_str()) {
        Ok(collection) => {
            let mut links = Vec::new();
            let mut embeds = Vec::new();
            for link in collection.outgoing() {
                if link.embed {
                    embeds.push(link.target.clone());
                } else {
                    links.push(link.target.clone());
                }
            }
            (links, embeds)
        }
        Err(_) => (Vec::new(), Vec::new()),
    };
    let backlinks = match graph {
        Some(service) => match service.graph_backlinks(relative) {
            Ok(view) => {
                let mut paths: Vec<String> = view.edges.into_iter().map(|edge| edge.from).collect();
                paths.sort();
                paths.dedup();
                paths
            }
            // A candidate the fresh filesystem scan just found but the
            // graph's own index has not yet caught up to degrades to an
            // empty backlink list, the same tolerance this function already
            // gives frontmatter-parse and wikilink-parse failures. Any
            // other failure (graph disabled for this vault specifically,
            // even though some other search feature is on) is a whole-vault
            // capability gap, not a per-candidate one: propagate it rather
            // than silently reporting "no backlinks" as if it were a real
            // answer.
            Err(SearchError::UnknownNote { .. }) => Vec::new(),
            Err(error) => return Err(ToolError::from(error)),
        },
        None => Vec::new(),
    };
    Ok(Some(MatchedRow {
        frontmatter,
        name,
        basename,
        path: relative.to_owned(),
        ext,
        folder,
        size: info.size,
        ctime: info.created.map(|value| value.to_string()),
        mtime: info.modified.map(|value| value.to_string()),
        tags,
        links,
        embeds,
        backlinks,
    }))
}

/// Stable multi-key sort applying each [`SortKey`] in reverse, the
/// standard technique for building a multi-key ordering from repeated
/// single-key stable sorts.
fn sort_rows(sort: &[SortKey], rows: &mut [MatchedRow]) -> Result<(), ToolError> {
    for key in sort.iter().rev() {
        let mut failure = None;
        rows.sort_by(|a, b| {
            if failure.is_some() {
                return Ordering::Equal;
            }
            let value_a = resolve_property(&key.property, &a.row_context(), "sort");
            let value_b = resolve_property(&key.property, &b.row_context(), "sort");
            match (value_a, value_b) {
                (Ok(value_a), Ok(value_b)) => {
                    let ordering = compare_values(value_a.as_ref(), value_b.as_ref());
                    if key.descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                }
                (Err(error), _) | (_, Err(error)) => {
                    failure = Some(error);
                    Ordering::Equal
                }
            }
        });
        if let Some(error) = failure {
            return Err(ToolError::from(error));
        }
    }
    Ok(())
}
