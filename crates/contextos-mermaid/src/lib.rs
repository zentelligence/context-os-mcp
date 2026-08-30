#![forbid(unsafe_code)]

use merman_core::{Engine, Error as MermanError, MermaidConfig, ParseOptions, ParsedDiagram};
use merman_render::svg::{SvgRenderOptions, render_layouted_svg};
use merman_render::{Error as RenderError, LayoutOptions, layout_parsed};
use serde_json::json;

/// One actionable Mermaid diagnostic, returned by [`ParsesMermaid::validate`]
/// and, on failure, by [`RendersMermaid::render`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MermaidDiagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

/// Parses Mermaid diagram source and reports diagnostics without rendering.
pub trait ParsesMermaid {
    /// Returns an empty vector when `source` parses cleanly; otherwise one or
    /// more diagnostics describing why it does not.
    fn validate(&self, source: &str) -> Vec<MermaidDiagnostic>;
}

/// Parses, lays out, and renders Mermaid diagram source to SVG.
pub trait RendersMermaid {
    /// Returns UTF-8 SVG bytes on success, or the diagnostics describing why
    /// `source` could not be rendered.
    ///
    /// # Errors
    ///
    /// Returns the diagnostics describing why `source` could not be parsed,
    /// laid out, or rendered (never a panic).
    fn render(&self, source: &str) -> Result<Vec<u8>, Vec<MermaidDiagnostic>>;
}

/// [`ParsesMermaid`] and [`RendersMermaid`] backed by `merman-core` and
/// `merman-render`'s headless parser and layout engines.
#[derive(Clone, Debug)]
pub struct MermanParser {
    engine: Engine,
}

impl Default for MermanParser {
    fn default() -> Self {
        Self {
            engine: Engine::new().with_site_config(Self::hardened_site_config()),
        }
    }
}

impl MermanParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Site configuration that forces every diagram to render without HTML labels.
    ///
    /// Mermaid embeds HTML label content in SVG `<foreignObject>` elements when
    /// `htmlLabels` is enabled. Diagram source comes from vault content an
    /// operator did not necessarily author themselves, so it is treated as
    /// untrusted: the output must never carry a `<foreignObject>`/embedded-HTML
    /// surface. Listing
    /// `htmlLabels` in `secure` also stops a diagram's own front-matter or `%%{init}%%`
    /// directive from re-enabling it (`secure_filtered_overrides` strips any override key
    /// named in `secure`, at any nesting depth, before it reaches the effective config).
    fn hardened_site_config() -> MermaidConfig {
        MermaidConfig::from_value(json!({
            "htmlLabels": false,
            "flowchart": { "htmlLabels": false },
            "secure": [
                "secure",
                "securityLevel",
                "startOnLoad",
                "maxTextSize",
                "suppressErrorRendering",
                "maxEdges",
                "fontFamily",
                "altFontFamily",
                "themeCSS",
                "themeVariables",
                "htmlLabels",
            ],
        }))
    }

    fn parse(&self, source: &str) -> Result<ParsedDiagram, Vec<MermaidDiagnostic>> {
        if let Some(diagnostic) = oversized_source_diagnostic(source) {
            return Err(vec![diagnostic]);
        }
        match self
            .engine
            .parse_diagram_sync(source, ParseOptions::strict())
        {
            Ok(Some(diagram)) => Ok(diagram),
            Ok(None) => Err(vec![no_diagram_diagnostic()]),
            Err(error) => Err(vec![MermaidDiagnostic::from(error)]),
        }
    }
}

/// Diagram source larger than this is rejected before parsing or layout even
/// begins (the brief's "must not allocate or lay out past a configured size
/// cap" rule; unbounded diagrams are a CPU/memory `DoS` shape). Matches
/// `merman_render::RenderResourceLimits::interactive`'s own `max_source_bytes`
/// so `validate` and `render` reject the same inputs at the same boundary,
/// rather than `render` alone catching it after an unbounded parse.
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

fn oversized_source_diagnostic(source: &str) -> Option<MermaidDiagnostic> {
    (source.len() > MAX_SOURCE_BYTES).then(|| MermaidDiagnostic {
        code: "mermaid/resource-limit".to_owned(),
        path: String::new(),
        message: format!(
            "Mermaid source is {} bytes, exceeding the {MAX_SOURCE_BYTES}-byte limit",
            source.len()
        ),
    })
}

fn no_diagram_diagnostic() -> MermaidDiagnostic {
    MermaidDiagnostic {
        code: "mermaid/no-diagram".to_owned(),
        path: String::new(),
        message: "no Mermaid diagram was detected in the supplied source".to_owned(),
    }
}

impl ParsesMermaid for MermanParser {
    fn validate(&self, source: &str) -> Vec<MermaidDiagnostic> {
        self.parse(source).err().unwrap_or_default()
    }
}

impl RendersMermaid for MermanParser {
    fn render(&self, source: &str) -> Result<Vec<u8>, Vec<MermaidDiagnostic>> {
        let parsed = self.parse(source)?;
        let layout_options = LayoutOptions::default();
        let layouted = layout_parsed(&parsed, &layout_options)
            .map_err(|error| vec![MermaidDiagnostic::from(error)])?;
        let svg = render_layouted_svg(
            &layouted,
            layout_options.text_measurer.as_ref(),
            &SvgRenderOptions::default(),
        )
        .map_err(|error| vec![MermaidDiagnostic::from(error)])?;
        Ok(svg.into_bytes())
    }
}

impl From<MermanError> for MermaidDiagnostic {
    fn from(error: MermanError) -> Self {
        match error {
            MermanError::DiagramParse {
                ref diagram_type,
                ref diagnostic,
            } => {
                let path = diagnostic.span().map_or_else(
                    || diagram_type.clone(),
                    |span| format!("{diagram_type}@{}..{}", span.start, span.end),
                );
                Self {
                    code: "mermaid/diagram-parse".to_owned(),
                    path,
                    message: diagnostic.message().to_owned(),
                }
            }
            MermanError::UnsupportedDiagram { ref diagram_type } => Self {
                code: "mermaid/unsupported-diagram".to_owned(),
                path: diagram_type.clone(),
                message: error.to_string(),
            },
            MermanError::MalformedFrontMatter
            | MermanError::InvalidDirectiveJson { .. }
            | MermanError::InvalidFrontMatterYaml { .. }
            | MermanError::DetectType(_) => Self {
                code: "mermaid/preprocess".to_owned(),
                path: String::new(),
                message: error.to_string(),
            },
        }
    }
}

impl From<RenderError> for MermaidDiagnostic {
    fn from(error: RenderError) -> Self {
        let code = match &error {
            RenderError::UnsupportedDiagram { .. } => "mermaid/unsupported-diagram",
            RenderError::ResourceLimitExceeded(_) => "mermaid/resource-limit",
            RenderError::InvalidModel { .. }
            | RenderError::SvgPostprocess { .. }
            | RenderError::Json(_) => "mermaid/render",
        };
        Self {
            code: code.to_owned(),
            path: String::new(),
            message: error.to_string(),
        }
    }
}
