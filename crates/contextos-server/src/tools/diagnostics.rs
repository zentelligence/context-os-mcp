//! Diagnostic/validation result shapes shared by the Obsidian structured
//! formats (`base_*`, `canvas_*`) and Mermaid (`mermaid_validate`,
//! `mermaid_render`): all three report the same
//! `{code, path, message}`-shaped diagnostic and the same
//! `{valid, diagnostics}`-shaped validation result. Each domain owns its
//! own `From<XDiagnostic>`/`From<XValidationSource>` conversion into these
//! shared types (in `tools::obsidian`/`tools::mermaid`), keeping the shape
//! centralised without coupling the domains to each other.

use rmcp::schemars;
use serde::Serialize;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StructuredDiagnosticToolResult {
    pub(crate) code: String,
    pub(crate) path: String,
    pub(crate) message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StructuredValidationToolResult {
    pub(crate) valid: bool,
    pub(crate) diagnostics: Vec<StructuredDiagnosticToolResult>,
}
