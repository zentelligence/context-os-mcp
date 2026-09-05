//! `note_create`, `frontmatter_read`, `frontmatter_update`, `base_*`,
//! `canvas_*`, and `links_read`: Obsidian-flavoured Markdown, Bases, and
//! JSON Canvas document support layered over the plain filesystem.

use std::sync::Arc;

use contextos_core::{Clock, ContentHash, Origin, SystemClock, VaultPath, VaultPathInput, WriteMutation};
use contextos_fs::{ReadTextRequest, SearchFilesRequest};
use contextos_obsidian::{
    BaseDocument, BaseOperation, CanvasCreateInput, CanvasDocument, CanvasOperation, FrontmatterDocument,
    LinkCollection, NoteCreateInput, NoteDocument, QueryDefinition, QueryFormat,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::tool;

use crate::resource_support::fallible_output_schema_for;
use crate::server::{ContextOsServer, PathInput};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::diagnostics::StructuredDiagnosticToolResult;
use crate::tools::obsidian_types::{
    BaseApplyToolInput, BaseCreateToolInput, BaseQueryToolInput, BaseQueryToolResult, BaseReadSource,
    BaseReadToolResult, BaseVaultPath, CanvasApplyToolInput, CanvasCreateToolInput, CanvasReadSource,
    CanvasReadToolResult, CanvasVaultPath, FrontmatterReadToolResult, FrontmatterUpdateInput,
    FrontmatterUpdateToolResult, LinkDirectionInput, LinksReadInput, LinksReadToolResult, NoteCreateToolInput,
    NoteCreateToolResult, StructuredPathInput, StructuredWriteToolResult,
};

#[rmcp::tool_router(router = obsidian_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "note_create",
        description = "Create a validated Obsidian Markdown note with standard frontmatter defaults",
        output_schema = fallible_output_schema_for::<NoteCreateToolResult>()
    )]
    async fn create_note(
        &self,
        Parameters(input): Parameters<NoteCreateToolInput>,
    ) -> Result<Json<NoteCreateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let service = Arc::clone(&self.mutations);
        let request = evaluate(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            if path.relative().extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                return Err(ToolError::Invalid("note path must end in .md"));
            }
            let title = input
                .frontmatter
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    path.relative()
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .map(|stem| stem.replace(['-', '_'], " "))
                })
                .ok_or(ToolError::Invalid("note path must have a UTF-8 filename"))?;
            let mut content = input.content;
            if !input.references.is_empty() {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str("\n# References\n");
                for reference in input.references {
                    if reference.target.trim().is_empty()
                        || reference.target.contains("[[")
                        || reference.target.contains("]]")
                    {
                        return Err(ToolError::Invalid("reference target is invalid"));
                    }
                    content.push_str("\n- [[");
                    content.push_str(reference.target.trim());
                    content.push_str("]]: ");
                    content.push_str(&reference.summary.split_whitespace().collect::<Vec<_>>().join(" "));
                    content.push('\n');
                }
            }
            let note = NoteDocument::try_from(NoteCreateInput {
                title: &title,
                frontmatter: input.frontmatter,
                content: &content,
                timestamp: &SystemClock.now().to_string(),
            })?;
            Ok(WriteMutation {
                path,
                content: String::try_from(note)?,
                expected_hash: None,
                force: false,
                origin: Origin::Tool("note_create".to_owned()),
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
            Ok(NoteCreateToolResult::from(service.write_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "frontmatter_read",
        description = "Read ordered YAML frontmatter from an Obsidian Markdown note",
        output_schema = fallible_output_schema_for::<FrontmatterReadToolResult>()
    )]
    async fn read_frontmatter(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<FrontmatterReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            let document = FrontmatterDocument::try_from(source.content.as_str())?;
            Ok(FrontmatterReadToolResult::from((document, source.content_hash)))
        })
        .await
    }

    #[tool(
        name = "frontmatter_update",
        description = "Apply an atomic JSON merge patch to note frontmatter while preserving its body",
        output_schema = fallible_output_schema_for::<FrontmatterUpdateToolResult>()
    )]
    async fn update_frontmatter(
        &self,
        Parameters(input): Parameters<FrontmatterUpdateInput>,
    ) -> Result<Json<FrontmatterUpdateToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = FrontmatterDocument::try_from(source.content.as_str())?;
            document.apply_merge_patch(input.patch, &SystemClock.now().to_string());
            let content = String::try_from(&document)?;
            let result = service.write_file(&WriteMutation {
                path,
                content,
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("frontmatter_update".to_owned()),
            })?;
            Ok(FrontmatterUpdateToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "base_create",
        description = "Create a validated Obsidian Bases YAML definition",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn create_base(
        &self,
        Parameters(input): Parameters<BaseCreateToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let request = evaluate(move || {
            let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let document = BaseDocument::try_from(input.definition)?;
            Ok(WriteMutation {
                path,
                content: String::try_from(&document)?,
                expected_hash: None,
                force: false,
                origin: Origin::Tool("base_create".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            Ok(StructuredWriteToolResult::from(service.write_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "base_read",
        description = "Read an ordered Obsidian Bases definition with schema diagnostics; a file that fails to parse at all is reported as a diagnostic, not a tool error",
        output_schema = fallible_output_schema_for::<BaseReadToolResult>()
    )]
    async fn read_base(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<BaseReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            Ok(BaseReadToolResult::from(BaseReadSource {
                document: BaseDocument::try_from(source.content.as_str()),
                content_hash: source.content_hash,
            }))
        })
        .await
    }

    #[tool(
        name = "base_apply",
        description = "Apply ordered Base operations atomically after validating the complete result",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn apply_base(
        &self,
        Parameters(input): Parameters<BaseApplyToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let BaseVaultPath(path) = evaluate(move || {
            BaseVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = BaseDocument::try_from(source.content.as_str())?;
            document.apply(input.operations.into_iter().map(BaseOperation::from).collect())?;
            let result = service.write_file(&WriteMutation {
                path,
                content: String::try_from(&document)?,
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("base_apply".to_owned()),
            })?;
            Ok(StructuredWriteToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "base_query",
        description = "Execute a Base view's filter tree against the vault and return matching rows as a Markdown table (default), JSON, or CSV. Evaluates a documented subset of the Bases filter grammar (==, !=, .contains(), file.hasTag(), file.inFolder(), and/or/not, and every documented file.* property: ext/name/basename/path/folder/size/ctime/mtime/tags/links/embeds/backlinks/properties). A display column naming a formula evaluates it only when the formula body is a bare property reference (shown as that property's own value) or file.asLink(display?) (a link to the row's own file, display defaulting to its basename); every other formula (arithmetic, if(), date functions, etc.) is shown as an unevaluated marker, and a formula.* reference in a filter or sort key is always rejected. Prefer file.inFolder(\"...\") over file.folder == \"...\" || file.folder.contains(\".../\") to scope a query to a directory and its subdirectories: file.inFolder() also lets the scan itself narrow to that directory, where the .contains() form cannot.",
        output_schema = fallible_output_schema_for::<BaseQueryToolResult>()
    )]
    async fn query_base(
        &self,
        Parameters(input): Parameters<BaseQueryToolInput>,
    ) -> Result<Json<BaseQueryToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let search = Arc::clone(&self.search);
        execute(move || {
            let format = QueryFormat::from(input.format);
            let (root_id, mut definition, document_diagnostics) = match (&input.path, &input.definition) {
                (Some(_), Some(_)) => {
                    return Err(ToolError::Invalid(
                        "base_query accepts exactly one of path or definition, not both",
                    ));
                }
                (None, None) => {
                    return Err(ToolError::Invalid("base_query requires either path or definition"));
                }
                (Some(raw), None) => {
                    let BaseVaultPath(path) = BaseVaultPath::try_from(StructuredPathInput { roots: &roots, raw })?;
                    let source = filesystem.read_text(&ReadTextRequest {
                        path: path.clone(),
                        limit: None,
                    })?;
                    let document = BaseDocument::try_from(source.content.as_str())?;
                    let definition = QueryDefinition::from_document(&document, input.view.as_deref())?;
                    let diagnostics = document
                        .diagnostics()
                        .into_iter()
                        .map(StructuredDiagnosticToolResult::from)
                        .collect();
                    (path.root_id(), definition, diagnostics)
                }
                (None, Some(inline)) => {
                    let definition = QueryDefinition::from_inline(inline)?;
                    let raw = input.vault.clone().unwrap_or_else(|| ".".to_owned());
                    let vault_path = VaultPath::try_from(VaultPathInput {
                        roots: &roots,
                        raw: &raw,
                    })?;
                    (vault_path.root_id(), definition, Vec::new())
                }
            };
            if let Some(limit) = input.limit {
                definition.limit = Some(usize::try_from(limit)?);
            }
            let outcome = crate::tools::base_query::run(&filesystem, &roots, root_id, &definition, format, &search)?;
            let diagnostics = document_diagnostics.into_iter().chain(outcome.diagnostics).collect();
            Ok(BaseQueryToolResult {
                content: outcome.content,
                columns: outcome.columns,
                matched: outcome.matched,
                truncated: outcome.truncated,
                diagnostics,
            })
        })
        .await
    }

    #[tool(
        name = "canvas_create",
        description = "Create a validated JSON Canvas 1.0 document and generate omitted identifiers",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn create_canvas(
        &self,
        Parameters(input): Parameters<CanvasCreateToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let request = evaluate(move || {
            let CanvasVaultPath(path) = CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let document = CanvasDocument::try_from(CanvasCreateInput {
                nodes: input.nodes,
                edges: input.edges,
            })?;
            Ok(WriteMutation {
                path,
                content: String::try_from(&document)?,
                expected_hash: None,
                force: false,
                origin: Origin::Tool("canvas_create".to_owned()),
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[request.path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            Ok(StructuredWriteToolResult::from(service.write_file(&request)?))
        })
        .await
    }

    #[tool(
        name = "canvas_read",
        description = "Read JSON Canvas nodes and edges with schema diagnostics; a file that fails to parse at all is reported as a diagnostic, not a tool error",
        output_schema = fallible_output_schema_for::<CanvasReadToolResult>()
    )]
    async fn read_canvas(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<Json<CanvasReadToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let CanvasVaultPath(path) = CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            Ok(CanvasReadToolResult::from(CanvasReadSource {
                document: CanvasDocument::try_from(source.content.as_str()),
                content_hash: source.content_hash,
            }))
        })
        .await
    }

    #[tool(
        name = "canvas_apply",
        description = "Apply ordered JSON Canvas operations atomically after full validation",
        output_schema = fallible_output_schema_for::<StructuredWriteToolResult>()
    )]
    async fn apply_canvas(
        &self,
        Parameters(input): Parameters<CanvasApplyToolInput>,
    ) -> Result<Json<StructuredWriteToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let CanvasVaultPath(path) = evaluate(move || {
            CanvasVaultPath::try_from(StructuredPathInput {
                roots: &roots,
                raw: &input.path,
            })
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let filesystem = Arc::clone(&self.filesystem);
        let service = Arc::clone(&self.mutations);
        execute(move || {
            let _guards = guards;
            let source = filesystem.read_text(&ReadTextRequest {
                path: path.clone(),
                limit: None,
            })?;
            let mut document = CanvasDocument::try_from(source.content.as_str())?;
            document.apply(input.operations.into_iter().map(CanvasOperation::from).collect())?;
            let result = service.write_file(&WriteMutation {
                path,
                content: String::try_from(&document)?,
                expected_hash: input
                    .expected_hash
                    .map(ContentHash::try_from)
                    .transpose()?
                    .or(Some(source.content_hash)),
                force: false,
                origin: Origin::Tool("canvas_apply".to_owned()),
            })?;
            Ok(StructuredWriteToolResult::from(result))
        })
        .await
    }

    #[tool(
        name = "links_read",
        description = "Read outgoing Obsidian wikilinks and embeds from a Markdown note",
        output_schema = fallible_output_schema_for::<LinksReadToolResult>()
    )]
    async fn read_links(
        &self,
        Parameters(input): Parameters<LinksReadInput>,
    ) -> Result<Json<LinksReadToolResult>, ToolFailure> {
        if !matches!(input.direction, LinkDirectionInput::Out) {
            return Err(ToolFailure::from(ToolError::Invalid(
                "incoming links are not available from links_read; use query_graph's \
                 'backlinks' operation instead",
            )));
        }
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &input.path,
            })?;
            let root = roots
                .iter()
                .nth(usize::try_from(path.root_id())?)
                .ok_or(ToolError::Invalid("link source vault is not configured"))?;
            let root_text = root
                .path()
                .to_str()
                .ok_or(ToolError::Invalid("link source vault path must be valid UTF-8"))?;
            let root_path = VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: root_text,
            })?;
            let source = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            let links = LinkCollection::try_from(source.content)?;
            let mut unresolved = Vec::new();
            for link in links.outgoing() {
                let target = std::path::Path::new(&link.target);
                let invalid = target.is_absolute()
                    || link.target.contains(['\\', ':'])
                    || target.components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir
                                | std::path::Component::RootDir
                                | std::path::Component::Prefix(_)
                        )
                    });
                let mut candidate = target.to_path_buf();
                if candidate.extension().is_none() {
                    candidate.set_extension("md");
                }
                let Some(candidate) = candidate.to_str() else {
                    if !unresolved.contains(&link.target) {
                        unresolved.push(link.target.clone());
                    }
                    continue;
                };
                let escaped = globset::escape(candidate);
                let patterns = if invalid || candidate.contains('/') {
                    vec![escaped]
                } else {
                    vec![escaped.clone(), format!("**/{escaped}")]
                };
                let mut resolved = false;
                if !invalid {
                    for pattern in patterns {
                        if !filesystem
                            .search_files(&SearchFilesRequest {
                                path: root_path.clone(),
                                pattern,
                                exclude_patterns: vec![".git/**".to_owned(), ".contextos/**".to_owned()],
                                max_results: 1,
                            })?
                            .is_empty()
                        {
                            resolved = true;
                            break;
                        }
                    }
                }
                if !resolved && !unresolved.contains(&link.target) {
                    unresolved.push(link.target.clone());
                }
            }
            Ok(LinksReadToolResult::from((links, unresolved)))
        })
        .await
    }
}
