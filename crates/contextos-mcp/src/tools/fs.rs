//! The 13 plain-filesystem tools (`fs_*`): safe read, write, and
//! directory operations over `VaultPath`-validated paths, plus the
//! `resource_link` attachment behaviour for
//! oversized text reads.

use std::sync::Arc;

use contextos_core::{
    ContentHash, CreateDirectoryMutation, DeleteMode, DeleteMutation, DeleteOutcome, MoveMutation, Origin,
    PipelineResult, VaultPath, VaultPathInput, VaultSet, WriteMutation,
};
use contextos_fs::{
    AttachmentRequest, DirectoryTreeRequest, EditFileRequest, FileInfoRequest, Filesystem, FsError,
    ListDirectoryRequest, ListDirectoryWithSizesRequest, ReadManyRequest, ReadTextRequest, SearchFilesRequest,
    TextEdit,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use rmcp::{tool, tool_router};

use crate::resource_support::{
    ResourceError, bounded_preview, fallible_output_schema_for, resource_link_for, resource_uri,
};
use crate::server::{ContextOsServer, ManagedIndexService, PathInput, RoutedMutationService};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute, execute_value};
use crate::tools::fs_types::{
    AllowedDirectoriesToolResult, AttachmentResource, BatchErrorToolResult, CreateDirectoryToolResult, DeleteFailure,
    DeleteInput, DeleteToolResult, DirectoryListingToolResult, EditInput, EditToolResult, FileInfoToolResult,
    ListDirectoryToolInput, MoveInput, MoveToolResult, PathsInput, ReadManyItemToolResult, ReadManyToolResult,
    ReadTextInput, ReadTextToolResult, SearchInput, SearchToolResult, ToolReadLimit, TreeInput, TreeNodeToolResult,
    WriteInput, WriteToolResult,
};

#[tool_router(vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "fs_read_text_file",
        description = "Read a UTF-8 file with an optional head, tail, or inclusive line range",
        output_schema = fallible_output_schema_for::<ReadTextToolResult>()
    )]
    async fn read_text(&self, Parameters(input): Parameters<ReadTextInput>) -> Result<CallToolResult, ToolFailure> {
        let ToolReadLimit(limit) = ToolReadLimit::try_from(&input)?;
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let threshold_bytes = self.resource_link_threshold_bytes;
        let roots_for_link = Arc::clone(&roots);
        let (mut result, path) = evaluate(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let read = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit,
            })?;
            Ok((ReadTextToolResult::from(read), path))
        })
        .await?;
        // At or above the configured threshold, bound the
        // inline preview and attach a `resource_link` so a resource-aware
        // host fetches the full content via `resources/read` instead of
        // exhausting its own inline-result budget. Below threshold,
        // behaviour is unchanged.
        let mut resource_links = Vec::new();
        let threshold = usize::try_from(threshold_bytes).unwrap_or(usize::MAX);
        if result.content.len() >= threshold {
            let (preview, bounded) = bounded_preview(&result.content, threshold);
            result.content = preview;
            result.truncated = result.truncated || bounded;
            resource_links.extend(resource_link_for(&path, &roots_for_link));
        }
        let value = serde_json::to_value(result).map_err(ToolError::CallToolResultSerialisation)?;
        let mut content = vec![ContentBlock::text(value.to_string())];
        content.extend(resource_links);
        let mut tool_result = CallToolResult::success(content);
        tool_result.structured_content = Some(value);
        Ok(tool_result)
    }

    #[tool(
        name = "fs_read_multiple_files",
        description = "Read several UTF-8 files with isolated per-file failures",
        output_schema = fallible_output_schema_for::<ReadManyToolResult>()
    )]
    async fn read_many(&self, Parameters(input): Parameters<PathsInput>) -> Result<CallToolResult, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let threshold_bytes = self.resource_link_threshold_bytes;
        let (files, resource_links) = evaluate(move || {
            let batch_capacity = filesystem.batch_capacity();
            if input.paths.len() > batch_capacity {
                return Err(contextos_fs::FsError::BatchTooLarge {
                    count: input.paths.len(),
                    maximum: batch_capacity,
                }
                .into());
            }
            let mut slots = Vec::with_capacity(input.paths.len());
            let mut valid_indices = Vec::with_capacity(input.paths.len());
            let mut valid_paths = Vec::with_capacity(input.paths.len());
            for raw in input.paths {
                match VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &raw,
                }) {
                    Ok(path) => {
                        valid_indices.push(slots.len());
                        valid_paths.push(path);
                        slots.push(None);
                    }
                    Err(error) => slots.push(Some(ReadManyItemToolResult {
                        path: raw,
                        content: None,
                        content_hash: None,
                        truncated: false,
                        error: Some(BatchErrorToolResult::from(error)),
                    })),
                }
            }
            let paths_for_links = valid_paths.clone();
            let results = filesystem.read_many(ReadManyRequest { paths: valid_paths })?;
            // Per-file resource_link attachment, isolated
            // per item exactly like the existing per-file error handling:
            // one oversized file's link never affects another.
            let threshold = usize::try_from(threshold_bytes).unwrap_or(usize::MAX);
            let mut resource_links = Vec::new();
            for ((index, path), result) in std::iter::zip(std::iter::zip(valid_indices, paths_for_links), results) {
                let mut item = ReadManyItemToolResult::from(result);
                if let Some(content) = item.content.as_mut()
                    && content.len() >= threshold
                {
                    let (preview, _) = bounded_preview(content, threshold);
                    *content = preview;
                    item.truncated = true;
                    resource_links.extend(resource_link_for(&path, &roots));
                }
                slots[index] = Some(item);
            }
            let files = slots
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .ok_or(ToolError::BatchAssembly)?;
            Ok((files, resource_links))
        })
        .await?;
        let value =
            serde_json::to_value(ReadManyToolResult { files }).map_err(ToolError::CallToolResultSerialisation)?;
        let mut content = vec![ContentBlock::text(value.to_string())];
        content.extend(resource_links);
        let mut tool_result = CallToolResult::success(content);
        tool_result.structured_content = Some(value);
        Ok(tool_result)
    }

    #[tool(
        name = "fs_attach_file",
        description = "Embed a text or base64 binary file as a size-capped MCP resource"
    )]
    async fn attach_file(&self, Parameters(input): Parameters<PathInput>) -> Result<CallToolResult, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let roots_for_uri = Arc::clone(&roots);
        let attachment = evaluate(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            filesystem
                .read_attachment(&AttachmentRequest { path })
                .map_err(ToolError::from)
        })
        .await?;
        let resource = AttachmentResource::try_from((attachment, roots_for_uri.as_ref()))?;
        Ok(CallToolResult::success(vec![ContentBlock::resource(
            ResourceContents::from(resource),
        )]))
    }

    #[tool(
        name = "fs_write_file",
        description = "Atomically create or replace a UTF-8 file with conflict protection",
        output_schema = fallible_output_schema_for::<WriteToolResult>()
    )]
    async fn write_file(
        &self,
        Parameters(input): Parameters<WriteInput>,
    ) -> Result<Json<WriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let service = Arc::clone(&self.mutations);
        let request = evaluate(move || {
            Ok(WriteMutation {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
                content: input.content,
                expected_hash: input.expected_hash.map(ContentHash::try_from).transpose()?,
                force: input.force,
                origin: Origin::Tool("fs_write_file".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            Ok(WriteToolResult::from(service.write_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_edit_file",
        description = "Apply exact-match edits transactionally, with optional dry-run unified diff",
        output_schema = fallible_output_schema_for::<EditToolResult>()
    )]
    async fn edit_file(&self, Parameters(input): Parameters<EditInput>) -> Result<Json<EditToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let service = Arc::clone(&self.mutations);
        let request = evaluate(move || {
            Ok(EditFileRequest {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
                edits: input.edits.into_iter().map(TextEdit::from).collect(),
                dry_run: input.dry_run,
                expected_hash: input.expected_hash.map(ContentHash::try_from).transpose()?,
                force: input.force,
                origin: Origin::Tool("fs_edit_file".to_owned()),
            })
        })
        .await?;
        let guards = if request.dry_run {
            Vec::new()
        } else {
            self.writes
                .lock_roots(&[request.path.root_id()])
                .await
                .map_err(ToolFailure::from)?
        };
        execute(move || {
            let _guards = guards;
            Ok(EditToolResult::from(service.edit_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_create_directory",
        description = "Create a directory tree idempotently",
        output_schema = fallible_output_schema_for::<CreateDirectoryToolResult>()
    )]
    async fn create_directory(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<CreateDirectoryToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let service = Arc::clone(&self.mutations);
        let request = evaluate(move || {
            Ok(CreateDirectoryMutation {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
                origin: Origin::Tool("fs_create_directory".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            Ok(CreateDirectoryToolResult::from(service.create_directory(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_list_directory",
        description = "List direct children with file and directory markers; with_sizes additionally includes size and modified time, and accepts sort_by",
        output_schema = fallible_output_schema_for::<DirectoryListingToolResult>()
    )]
    async fn list_directory(
        &self,
        Parameters(input): Parameters<ListDirectoryToolInput>,
    ) -> Result<Json<DirectoryListingToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            if !input.with_sizes {
                if input.sort_by.is_some() {
                    return Err(ToolError::Invalid(
                        "fs_list_directory 'sort_by' requires 'with_sizes: true'",
                    ));
                }
                return Ok(DirectoryListingToolResult::from(
                    filesystem.list_directory(&ListDirectoryRequest { path })?,
                ));
            }
            Ok(DirectoryListingToolResult::from(filesystem.list_directory_with_sizes(
                &ListDirectoryWithSizesRequest {
                    path,
                    sort_by: input.sort_by.unwrap_or_default().into(),
                },
            )?))
        })
        .await
    }

    #[tool(
        name = "fs_directory_tree",
        description = "Return a bounded recursive JSON directory tree with exclusions"
    )]
    async fn directory_tree(&self, Parameters(input): Parameters<TreeInput>) -> Result<CallToolResult, ToolFailure> {
        let max_depth = usize::try_from(input.max_depth).map_err(ToolError::from)?;
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let tree = evaluate(move || {
            let request = DirectoryTreeRequest {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
                exclude_patterns: input.exclude_patterns,
                max_depth,
            };
            Ok(TreeNodeToolResult::from(filesystem.directory_tree(&request)?))
        })
        .await?;
        // No `output_schema` is advertised for this tool (unlike every
        // other tool here): `TreeNodeToolResult` is self-referential
        // (`children: Option<Vec<Self>>`), and `inline_local_refs`
        // (`resource_support.rs`) cannot inline a recursive `$ref` without
        // an infinite schema, the same structural limit `schemars`' own
        // `inline_subschemas` setting hits. It was previously left in as
        // the catalogue's one exception; not itself separately confirmed
        // live, but the last remaining schema shape once every other kind
        // of `$ref`/`$defs`/composition confirmed to take down Cowork's
        // whole per-task tool registry had already been eliminated
        // elsewhere. Omitting `output_schema` here removes the last
        // suspect at negligible cost: it is optional MCP metadata, and the
        // runtime response shape below is unchanged.
        let value = serde_json::to_value(tree).map_err(ToolError::CallToolResultSerialisation)?;
        let mut tool_result = CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
        tool_result.structured_content = Some(value);
        Ok(tool_result)
    }

    #[tool(
        name = "fs_move_file",
        description = "Move or rename a file or directory without replacing the destination",
        output_schema = fallible_output_schema_for::<MoveToolResult>()
    )]
    async fn move_file(&self, Parameters(input): Parameters<MoveInput>) -> Result<Json<MoveToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let service = Arc::clone(&self.mutations);
        let request = evaluate(move || {
            Ok(MoveMutation {
                source: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.source,
                })?,
                destination: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.destination,
                })?,
                origin: Origin::Tool("fs_move_file".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.source.root_id(), request.destination.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            Ok(MoveToolResult::from(service.move_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_delete_file",
        description = "Move a file or empty directory to trash, or hard-delete when configured",
        output_schema = fallible_output_schema_for::<DeleteToolResult>()
    )]
    async fn delete_file(
        &self,
        Parameters(input): Parameters<DeleteInput>,
    ) -> Result<Json<DeleteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let roots_for_delete = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        let destructive_delete = Arc::clone(&self.destructive_delete);
        let indexes = Arc::clone(&self.indexes);
        let (is_single, plan) = evaluate(move || plan_delete(&roots, &filesystem, &destructive_delete, input)).await?;
        let root_ids = plan
            .iter()
            .filter_map(|attempt| attempt.as_ref().ok())
            .map(|(mutation, _)| mutation.path.root_id())
            .collect::<Vec<_>>();
        let guards = self.writes.lock_roots(&root_ids).await.map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            let attempts = plan
                .into_iter()
                .map(|attempt| match attempt {
                    Ok((mutation, root_index)) => delete_honouring_managed_index(
                        service.as_ref(),
                        indexes.as_ref(),
                        roots_for_delete.as_ref(),
                        root_index,
                        &mutation,
                    )
                    .map_err(|error| Box::new((mutation.path.relative().to_string_lossy().into_owned(), error))),
                    Err(failure) => Err(failure),
                })
                .collect::<Vec<_>>();
            // A caller passing a single `path` expects a whole-call error
            // when that one target fails to delete, not a successful call
            // whose `results[0]` carries the failure. The `paths`/`pattern`
            // batch forms are separate surface, free to adopt a
            // partial-success contract outright.
            if is_single {
                let outcome = attempts.into_iter().next().ok_or(ToolError::BatchAssembly)?;
                return match outcome {
                    Ok(result) => Ok(DeleteToolResult::from(vec![Ok(result)])),
                    Err(failure) => Err(failure.1),
                };
            }
            Ok(DeleteToolResult::from(attempts))
        })
        .await
    }

    #[tool(
        name = "fs_search_files",
        description = "Find paths by case-insensitive glob with exclusions; prefix pattern with **/ to match at any depth, not just directly inside path",
        output_schema = fallible_output_schema_for::<SearchToolResult>()
    )]
    async fn search_files(
        &self,
        Parameters(input): Parameters<SearchInput>,
    ) -> Result<Json<SearchToolResult>, ToolFailure> {
        let max_results = usize::try_from(input.max_results).map_err(ToolError::from)?;
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let request = SearchFilesRequest {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
                pattern: input.pattern,
                exclude_patterns: input.exclude_patterns,
                max_results,
            };
            Ok(SearchToolResult::from(filesystem.search_files(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_get_file_info",
        description = "Return path metadata, permissions, timestamps, and bounded content hash",
        output_schema = fallible_output_schema_for::<FileInfoToolResult>()
    )]
    async fn file_info(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<FileInfoToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let request = FileInfoRequest {
                path: VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: &input.path,
                })?,
            };
            Ok(FileInfoToolResult::from(filesystem.file_info(&request)?))
        })
        .await
    }

    #[tool(
        name = "fs_list_allowed_directories",
        description = "List configured, resolved vault roots and managed flags",
        output_schema = fallible_output_schema_for::<AllowedDirectoriesToolResult>()
    )]
    async fn allowed_directories(&self) -> Result<Json<AllowedDirectoriesToolResult>, ToolFailure> {
        let filesystem = Arc::clone(&self.filesystem);
        execute_value(move || AllowedDirectoriesToolResult::from(filesystem.list_allowed_directories())).await
    }
}

/// `contextos-index`'s own reconciliation recreates
/// `index.md`/`_index.md` after every mutation, so `fs_delete_file`'s
/// non-recursive emptiness guard (`contextos-fs::mutate::delete`)
/// sees a directory as non-empty even once every file the caller actually
/// created is gone. When the target directory is under this vault's
/// active index management, clear those managed artefacts first
/// (`Origin::Internal`, hard-deleted rather than trashed since they are
/// regenerable, not operator content) and retry the original deletion.
/// A directory that is not index-managed (an unmanaged root, or a
/// directory matched by `index_md.exclude`) may hold a real file
/// literally named `index.md`; this must never touch that directory's
/// contents on the caller's behalf, so the original error is returned
/// unchanged.
fn delete_honouring_managed_index(
    service: &RoutedMutationService,
    indexes: &[Option<ManagedIndexService>],
    roots: &VaultSet,
    root_index: usize,
    request: &DeleteMutation,
) -> Result<PipelineResult<DeleteOutcome>, ToolError> {
    match service.delete_path(request) {
        Ok(outcome) => Ok(outcome),
        Err(FsError::DirectoryNotEmpty { path }) => {
            let is_managed = indexes
                .get(root_index)
                .and_then(Option::as_ref)
                .is_some_and(|index_service| index_service.manages_directory(&request.path));
            if !is_managed {
                return Err(ToolError::from(FsError::DirectoryNotEmpty { path }));
            }
            for name in ["index.md", "_index.md"] {
                let child = child_path(&request.path, roots, name)?;
                let cleanup = DeleteMutation {
                    path: child,
                    mode: DeleteMode::Hard,
                    origin: Origin::Internal("fs_delete_file".to_owned()),
                };
                match service.delete_path(&cleanup) {
                    Ok(_) | Err(FsError::NotFound { .. }) => {}
                    Err(error) => return Err(ToolError::from(error)),
                }
            }
            Ok(service.delete_path(request)?)
        }
        Err(error) => Err(ToolError::from(error)),
    }
}

/// Builds the `VaultPath` for `directory/{relative}`, joining onto
/// `directory`'s own relative path first and building one fresh
/// `{vault-name}://{relative-path}` URI from the result (`resource_uri`,
/// the same construction `resources/read` uses), rather than
/// text-concatenating `/{relative}` onto an already-built URI: when
/// `directory` is a vault's root, that URI's path component is empty, and
/// appending onto it directly would produce a leading-slash remainder
/// that `VaultPath::try_from` resolves as an absolute filesystem path
/// instead of a vault-relative one.
fn child_path(directory: &VaultPath, roots: &VaultSet, relative: &str) -> Result<VaultPath, ToolError> {
    let root = roots.root(directory.root_id()).ok_or_else(|| {
        ToolError::from(ResourceError::InvalidPath {
            path: <&std::path::Path>::from(directory).to_path_buf(),
        })
    })?;
    let joined = directory.relative().join(relative);
    let raw = resource_uri(root.name(), &joined);
    Ok(VaultPath::try_from(VaultPathInput { roots, raw: &raw })?)
}

/// One resolved `fs_delete_file` target's path, or the [`DeleteFailure`]
/// that stopped it from resolving.
type DeleteTargetResult = Result<VaultPath, DeleteFailure>;

/// One planned deletion (mutation plus its resolved root index for
/// `delete_honouring_managed_index`), or the [`DeleteFailure`] that
/// stopped it from being planned.
type DeletePlanItem = Result<(DeleteMutation, usize), DeleteFailure>;

/// Resolves `fs_delete_file`'s input into every target to attempt:
/// a single `path`, multiple explicit `paths`, or every match
/// of `pattern` under `path`. Exactly one of those three selector styles
/// must be given; any other combination, or none, is a whole-call error,
/// since it is a caller mistake rather than a per-target failure.
///
/// The returned `bool` is `true` only for the single-`path` form: that one
/// target's failure must still fail the whole call, unlike the
/// `paths`/`pattern` batch forms.
fn resolve_delete_targets(
    roots: &VaultSet,
    filesystem: &Filesystem,
    input: DeleteInput,
) -> Result<(bool, Vec<DeleteTargetResult>), ToolError> {
    match (input.path, input.paths, input.pattern) {
        (Some(path), paths, None) if paths.is_empty() => Ok((true, vec![resolve_one_target(roots, &path)])),
        (None, paths, None) if !paths.is_empty() => {
            Ok((false, paths.iter().map(|raw| resolve_one_target(roots, raw)).collect()))
        }
        (Some(base), paths, Some(pattern)) if paths.is_empty() => {
            let base_path = VaultPath::try_from(VaultPathInput { roots, raw: &base })?;
            let capacity = filesystem.batch_capacity();
            let matches = filesystem.search_files(&SearchFilesRequest {
                path: base_path.clone(),
                pattern,
                exclude_patterns: Vec::new(),
                max_results: capacity.saturating_add(1),
            })?;
            let targets = matches
                .iter()
                .map(|relative| child_path(&base_path, roots, relative).map(Ok))
                .collect::<Result<Vec<_>, ToolError>>()?;
            Ok((false, targets))
        }
        _ => Err(ToolError::Invalid(
            "fs_delete_file accepts exactly one of: 'path' alone, 'paths', or 'path' with 'pattern'",
        )),
    }
}

fn resolve_one_target(roots: &VaultSet, raw: &str) -> DeleteTargetResult {
    VaultPath::try_from(VaultPathInput { roots, raw })
        .map_err(|error| Box::new((raw.to_owned(), ToolError::from(error))))
}

/// Builds the complete deletion plan for one `fs_delete_file` call:
/// resolves every target, enforces the shared batch capacity
/// (`FsError::BatchTooLarge`, matching `fs_read_multiple_files`), and
/// checks the `hard`/`destructive_delete` gate per target's own root
/// (targets may span more than one configured vault).
fn plan_delete(
    roots: &VaultSet,
    filesystem: &Filesystem,
    destructive_delete: &[bool],
    input: DeleteInput,
) -> Result<(bool, Vec<DeletePlanItem>), ToolError> {
    let hard = input.hard;
    let (is_single, targets) = resolve_delete_targets(roots, filesystem, input)?;
    let capacity = filesystem.batch_capacity();
    if targets.len() > capacity {
        return Err(FsError::BatchTooLarge {
            count: targets.len(),
            maximum: capacity,
        }
        .into());
    }
    let plan = targets
        .into_iter()
        .map(|target| match target {
            Ok(path) => plan_one_delete(path, hard, destructive_delete),
            Err(failure) => Err(failure),
        })
        .collect();
    Ok((is_single, plan))
}

fn plan_one_delete(path: VaultPath, hard: bool, destructive_delete: &[bool]) -> DeletePlanItem {
    let display = path.relative().to_string_lossy().into_owned();
    let root_index = match usize::try_from(path.root_id()) {
        Ok(index) => index,
        Err(error) => return Err(Box::new((display, ToolError::from(error)))),
    };
    let mode = if hard {
        if !destructive_delete.get(root_index).copied().unwrap_or(false) {
            return Err(Box::new((display, ToolError::DestructiveDeleteDisabled)));
        }
        DeleteMode::Hard
    } else {
        DeleteMode::Trash
    };
    Ok((
        DeleteMutation {
            path,
            mode,
            origin: Origin::Tool("fs_delete_file".to_owned()),
        },
        root_index,
    ))
}
