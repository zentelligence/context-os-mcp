//! `mermaid_validate` and `mermaid_render`: headless
//! Mermaid diagram parsing, layout, and SVG rendering via
//! `contextos-mermaid`'s `ParsesMermaid`/`RendersMermaid` traits.
//! Read-only; never writes to the vault.

use std::sync::Arc;

use contextos_core::{VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{Filesystem, ReadTextRequest};
use contextos_mermaid::{MermaidDiagnostic, ParsesMermaid, RendersMermaid};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use rmcp::{schemars, tool};
use serde::Deserialize;

use crate::resource_support::fallible_output_schema_for;
use crate::server::ContextOsServer;
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::diagnostics::{StructuredDiagnosticToolResult, StructuredValidationToolResult};

#[rmcp::tool_router(router = mermaid_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "mermaid_validate",
        description = "Validate a Mermaid diagram from a vault note's fenced ```mermaid block or inline source, without rendering",
        output_schema = fallible_output_schema_for::<StructuredValidationToolResult>()
    )]
    async fn validate_mermaid(
        &self,
        Parameters(input): Parameters<MermaidSourceInput>,
    ) -> Result<Json<StructuredValidationToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let mermaid = Arc::clone(&self.mermaid);
        execute(move || {
            let source = resolve_mermaid_source(&roots, &filesystem, input)?;
            Ok(StructuredValidationToolResult::from(
                MermaidValidationSource(mermaid.validate(&source)),
            ))
        })
        .await
    }

    #[tool(
        name = "mermaid_render",
        description = "Parse, lay out, and render a Mermaid diagram from a vault note's fenced ```mermaid block or inline source to SVG"
    )]
    async fn render_mermaid(
        &self,
        Parameters(input): Parameters<MermaidSourceInput>,
    ) -> Result<CallToolResult, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let mermaid = Arc::clone(&self.mermaid);
        evaluate(move || {
            let source = resolve_mermaid_source(&roots, &filesystem, input)?;
            CallToolResult::try_from(MermaidRenderOutcome(mermaid.render(&source)))
        })
        .await
    }
}

/// Input shared by `mermaid_validate` and `mermaid_render`: exactly one of
/// `path` (a note containing a fenced ` ```mermaid ` block) or `source` (an
/// inline diagram) must be supplied.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MermaidSourceInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

/// Resolves a [`MermaidSourceInput`] to the raw diagram text: reads the note
/// and extracts its first fenced ` ```mermaid ` block for the `path`
/// variant, or returns `source` unchanged.
fn resolve_mermaid_source(
    roots: &VaultSet,
    filesystem: &Filesystem,
    input: MermaidSourceInput,
) -> Result<String, ToolError> {
    match (input.path, input.source) {
        (Some(_), Some(_)) => Err(ToolError::Invalid(
            "mermaid tools accept exactly one of 'path' or 'source', not both",
        )),
        (None, None) => Err(ToolError::Invalid(
            "mermaid tools require either 'path' or 'source'",
        )),
        (Some(raw), None) => {
            let path = VaultPath::try_from(VaultPathInput { roots, raw: &raw })?;
            let text = filesystem.read_text(&ReadTextRequest { path, limit: None })?;
            extract_mermaid_fence(&text.content).ok_or(ToolError::MermaidFenceMissing)
        }
        (None, Some(source)) => Ok(source),
    }
}

/// Returns the content of the first ` ```mermaid ` fenced code block in
/// `content`, or `None` if no such block exists or it is never closed.
fn extract_mermaid_fence(content: &str) -> Option<String> {
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let Some(info) = line.trim_start().strip_prefix("```") else {
            continue;
        };
        if info.trim().eq_ignore_ascii_case("mermaid") {
            let mut block_lines = Vec::new();
            for body_line in lines.by_ref() {
                if body_line.trim_start().starts_with("```") {
                    return Some(block_lines.join("\n"));
                }
                block_lines.push(body_line);
            }
            return None;
        }
        // A differently-fenced block (e.g. ```text documenting Mermaid
        // syntax): consume its body as opaque content, so a literal
        // "```mermaid" line inside it is never mistaken for a real fence.
        for body_line in lines.by_ref() {
            if body_line.trim_start().starts_with("```") {
                break;
            }
        }
    }
    None
}

impl From<MermaidDiagnostic> for StructuredDiagnosticToolResult {
    fn from(value: MermaidDiagnostic) -> Self {
        Self {
            code: value.code,
            path: value.path,
            message: value.message,
        }
    }
}

struct MermaidValidationSource(Vec<MermaidDiagnostic>);

impl From<MermaidValidationSource> for StructuredValidationToolResult {
    fn from(value: MermaidValidationSource) -> Self {
        let diagnostics = value
            .0
            .into_iter()
            .map(StructuredDiagnosticToolResult::from)
            .collect::<Vec<_>>();
        Self {
            valid: diagnostics.is_empty(),
            diagnostics,
        }
    }
}

/// Outcome of [`RendersMermaid::render`]: either the rendered SVG bytes, or
/// the same diagnostics [`MermaidValidationSource`] would report.
struct MermaidRenderOutcome(Result<Vec<u8>, Vec<MermaidDiagnostic>>);

impl TryFrom<MermaidRenderOutcome> for CallToolResult {
    type Error = ToolError;

    fn try_from(value: MermaidRenderOutcome) -> Result<Self, Self::Error> {
        match value.0 {
            Ok(bytes) => {
                let svg = String::from_utf8(bytes).map_err(|_| ToolError::MermaidRenderNotUtf8)?;
                Ok(Self::success(vec![ContentBlock::resource(
                    ResourceContents::TextResourceContents {
                        uri: "contextos:///mermaid/render.svg".to_owned(),
                        mime_type: Some("image/svg+xml".to_owned()),
                        text: svg,
                        meta: None,
                    },
                )]))
            }
            Err(diagnostics) => {
                let result =
                    StructuredValidationToolResult::from(MermaidValidationSource(diagnostics));
                let value = serde_json::to_value(result)
                    .map_err(ToolError::MermaidDiagnosticSerialisation)?;
                Ok(Self::structured(value))
            }
        }
    }
}
