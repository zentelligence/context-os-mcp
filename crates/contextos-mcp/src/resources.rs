//! The MCP resources capability: `resources/list` and `resources/read`,
//! serving every `hidden`-eligible vault file, any format, root confined
//! and read-only. See [`resource_support`](crate::resource_support)
//! for the shared URI/error/preview building blocks this and
//! `tools::fs::attach_file` both use.

use std::sync::Arc;

use base64::Engine;
use contextos_core::{VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{
    Filesystem, FsError, ReadTextRequest, SearchFilesRequest, mime_type_for_extension,
};
use rmcp::model::{
    Implementation, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};

use crate::resource_support::{
    ResourceError, path_for_resource_uri, resource_uri, resource_uri_template,
};
use crate::server::ContextOsServer;

#[tool_handler(router = self.effective_catalogue())]
impl ServerHandler for ContextOsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            "ContextOS MCP",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Safe filesystem operations over configured ContextOS vault roots".to_owned(),
        )
    }

    /// Lists the effective tool catalogue (core tools plus every registered
    /// [`ServerModule`](crate::server::ServerModule)'s tools). Written by
    /// hand rather than left to `#[tool_handler]`'s default: that
    /// generated body has no `.await` in it either, which trips the same
    /// `clippy::unused_async_trait_impl` lint this override avoids for
    /// [`Self::list_resource_templates`], but as macro-generated code it
    /// cannot itself carry a targeted `#[allow]`.
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, ErrorData>> {
        std::future::ready(Ok(ListToolsResult::with_all_items(
            self.effective_catalogue().list_all(),
        )))
    }

    /// Lists every eligible vault file across every configured root as an
    /// MCP resource, excluding any path matching the vault's configured
    /// `hidden` patterns via the same enumeration filtering
    /// `fs_search_files` already applies. Root-confined and read-only,
    /// mirroring every other filesystem surface; unlike a bounded content
    /// read, a resource listing must be complete rather than silently
    /// truncated, so no result cap is applied here.
    ///
    /// Enumerates only each vault's configured `resources_list_include`
    /// allowlist, not every eligible file: a vault with
    /// no configured patterns reports nothing here, since eagerly listing
    /// thousands of files has little discovery value. `resources/read` and
    /// `resources/templates/list` remain unrestricted regardless.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let include_patterns = Arc::clone(&self.resources_list_include);
        let resources = tokio::task::spawn_blocking(move || {
            list_vault_resources(&roots, &filesystem, &include_patterns)
        })
        .await
        .map_err(|_| ErrorData::internal_error("resource listing task failed", None))?
        .map_err(ResourceError::into_error_data)?;
        Ok(ListResourcesResult::with_all_items(resources))
    }

    /// Advertises one `{name}://{+path}` URI template per configured vault,
    /// so a client can construct a valid
    /// `resources/read` URI directly, without first calling
    /// [`Self::list_resources`]: useful once a vault holds enough files
    /// that eagerly enumerating all of them has little discovery value.
    /// Purely additive: [`Self::list_resources`] itself is unchanged.
    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourceTemplatesResult, ErrorData>> {
        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(
            vault_resource_templates(&self.roots),
        )))
    }

    /// Reads one vault file by the `{name}://{relative-path}` URI
    /// advertised in [`Self::list_resources`], enforcing the same root
    /// confinement and configurable size cap as every other text read
    /// surface. Any format is servable, no allow-list: binary content
    /// falls back to the same detection and
    /// encoding `fs_attach_file` already performs.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let uri = request.uri;
        tokio::task::spawn_blocking(move || read_vault_resource(&roots, &filesystem, &uri))
            .await
            .map_err(|_| ErrorData::internal_error("resource read task failed", None))?
            .map_err(ResourceError::into_error_data)
    }
}

/// Enumerates only the files named by each vault's configured
/// `resources_list_include` allowlist, never a bare `**/*` walk: eagerly
/// dumping every file in a vault holding thousands of them has little
/// discovery value, the complaint this replaces a prior, unconditional
/// `**/*` walk to fix. `include_patterns` is index-aligned with `roots`; a
/// vault with no configured patterns contributes nothing. Every pattern is
/// still subject to `hidden` via `filesystem.search_files` unchanged.
/// `resources/read` and `resources/templates/list` remain entirely
/// unrestricted by this: it narrows autonomous enumeration only, never
/// direct access.
pub(crate) fn list_vault_resources(
    roots: &VaultSet,
    filesystem: &Filesystem,
    include_patterns: &[Vec<String>],
) -> Result<Vec<Resource>, ResourceError> {
    let mut resources = Vec::new();
    for (index, root) in roots.into_iter().enumerate() {
        let patterns = include_patterns.get(index).map_or(&[][..], Vec::as_slice);
        if patterns.is_empty() {
            continue;
        }
        let root_text = root
            .path()
            .to_str()
            .ok_or_else(|| ResourceError::InvalidPath {
                path: root.path().to_path_buf(),
            })?;
        let root_path = VaultPath::try_from(VaultPathInput {
            roots,
            raw: root_text,
        })?;
        // A file matching more than one configured pattern must still be
        // listed once, not once per matching pattern.
        let mut seen = std::collections::BTreeSet::new();
        for pattern in patterns {
            let matches = filesystem.search_files(&SearchFilesRequest {
                path: root_path.clone(),
                pattern: pattern.clone(),
                exclude_patterns: Vec::new(),
                max_results: usize::MAX,
            })?;
            for relative in matches {
                if !seen.insert(relative.clone()) {
                    continue;
                }
                let absolute = root.path().join(&relative);
                // `symlink_metadata` (never following the link): a genuine
                // in-root directory (the widened glob now matches those
                // too) is never a valid resource and is skipped outright.
                // A symlink of any name is still listed (this codebase's
                // `search_files` never filters symlinks out), but its size
                // is never disclosed unless it is genuinely an in-root
                // regular file: following the link here would leak the
                // byte size of a target `read_resource` will separately
                // reject.
                let metadata = std::fs::symlink_metadata(&absolute).ok();
                if metadata.as_ref().is_some_and(std::fs::Metadata::is_dir) {
                    continue;
                }
                let uri = resource_uri(root.name(), std::path::Path::new(&relative));
                let mime_type = mime_type_for_extension(&absolute)
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                let size = metadata
                    .filter(std::fs::Metadata::is_file)
                    .map(|metadata| metadata.len());
                let name = relative.replace(std::path::MAIN_SEPARATOR, "/");
                let mut resource = Resource::new(uri, name).with_mime_type(mime_type);
                if let Some(size) = size {
                    resource = resource.with_size(size);
                }
                resources.push(resource);
            }
        }
    }
    Ok(resources)
}

/// Builds `resources/templates/list`'s response: one
/// `{name}://{+path}` template per configured vault, no filesystem access,
/// unlike [`list_vault_resources`]'s full walk.
fn vault_resource_templates(roots: &VaultSet) -> Vec<ResourceTemplate> {
    roots
        .into_iter()
        .map(|root| {
            let name = root.name();
            ResourceTemplate::new(resource_uri_template(name), name.to_owned()).with_description(
                format!(
                    "Any file in the '{name}' vault, addressed by its vault-relative path; \
                     read via resources/read."
                ),
            )
        })
        .collect()
}

/// Counts files each configured vault root's `resources_list_include`
/// allowlist actually enumerates, index-aligned with `roots`, for
/// `vault_info` reporting. Reuses [`list_vault_resources`]'s existing walk
/// rather than a second one: each listed resource's
/// `{name}://{relative-path}` URI is resolved back to its owning root via
/// the same [`VaultPath`] validation every other tool path already goes
/// through. This counts the same narrowed set
/// `resources/list` itself reports, not every file `resources/read` could
/// still serve directly; a vault with no configured allowlist reports `0`
/// here, correctly, not the size of its full eligible-file set.
pub(crate) fn resource_eligible_file_counts(
    roots: &VaultSet,
    filesystem: &Filesystem,
    include_patterns: &[Vec<String>],
) -> Result<Vec<usize>, ResourceError> {
    let mut counts = vec![0_usize; roots.len()];
    for resource in list_vault_resources(roots, filesystem, include_patterns)? {
        let path = path_for_resource_uri(&resource.uri, roots)?;
        let index = usize::try_from(path.root_id())?;
        counts[index] += 1;
    }
    Ok(counts)
}

fn read_vault_resource(
    roots: &VaultSet,
    filesystem: &Filesystem,
    uri: &str,
) -> Result<ReadResourceResult, ResourceError> {
    let path = path_for_resource_uri(uri, roots)?;
    let absolute: &std::path::Path = (&path).into();
    match filesystem.read_text(&ReadTextRequest {
        path: path.clone(),
        limit: None,
    }) {
        Ok(result) => {
            let mime_type =
                mime_type_for_extension(absolute).unwrap_or_else(|| "text/plain".to_owned());
            Ok(ReadResourceResult::new(vec![
                ResourceContents::text(result.content, uri.to_owned()).with_mime_type(mime_type),
            ]))
        }
        // Any format is servable, no allow-list: binary content falls back
        // to the same detection and encoding `fs_attach_file` already
        // performs, reusing `read_attachment` rather than reimplementing
        // it. This is capped independently at `read_attachment`'s fixed
        // 10 MiB, not the configurable text-size cap, which the `Ok`
        // branch above still enforces unchanged for text content.
        Err(FsError::Binary { .. }) => {
            let attachment =
                filesystem.read_attachment(&contextos_fs::AttachmentRequest { path })?;
            let blob = base64::engine::general_purpose::STANDARD.encode(attachment.bytes);
            Ok(ReadResourceResult::new(vec![
                ResourceContents::blob(blob, uri.to_owned()).with_mime_type(attachment.mime_type),
            ]))
        }
        Err(error) => Err(error.into()),
    }
}
