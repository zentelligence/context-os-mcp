//! Triple-colon fence parsing (`standards/markdown-fence-conventions.md`,
//! FR-241): `:::name{attrs}` ... `:::` blocks resolve to their semantic
//! container before general Markdown parsing runs, since triple-colon
//! fences are not core Markdown and a generic parser would otherwise
//! mangle or ignore them. An unrecognised fence name still renders as a
//! generically bordered block carrying its literal name as a label, never
//! silently dropped (FR-241's explicit requirement).
//!
//! Fences do not nest in this pipeline (the convention document itself
//! recommends "keep nested fences shallow"): a fence's inner text is
//! rendered through the caller's own recursive pass, but this module's own
//! scan never looks for a nested `:::` open inside an already-open fence,
//! only the next `:::` close.

use crate::rendering::escape_html;

/// The full vocabulary `standards/markdown-fence-conventions.md` documents
/// across every category it lists (admonition, reasoning and workflow,
/// layout, technical documentation, status, AI workflow), plus its two
/// document-render fences. Kept as one flat recognised-name set: FR-241
/// distinguishes only "recognised" from "unrecognised", not category, so a
/// category type would be structure with no behavioural use yet.
const RECOGNISED_FENCE_NAMES: &[&str] = &[
    // Admonition
    "note",
    "info",
    "tip",
    "hint",
    "important",
    "warning",
    "caution",
    "danger",
    "error",
    "success",
    // Reasoning and workflow
    "summary",
    "context",
    "analysis",
    "insight",
    "recommendation",
    "decision",
    "rationale",
    "assumption",
    "constraint",
    "risk",
    "mitigation",
    "tradeoff",
    "question",
    "todo",
    "next",
    // Layout
    "stack",
    "row",
    "columns",
    "grid",
    "cards",
    "card",
    "panel",
    "section",
    "hero",
    "banner",
    "cta",
    "aside",
    "sidebar",
    "footer",
    // Technical documentation
    "definition",
    "example",
    "counterexample",
    "principle",
    "pattern",
    "antipattern",
    "procedure",
    "checklist",
    "reference",
    "implementation",
    "api",
    "schema",
    "migration",
    "test",
    // Status
    "draft",
    "pending",
    "partial",
    "blocked",
    "pass",
    "fail",
    "unknown",
    "deprecated",
    "experimental",
    "stable",
    // AI workflow
    "input",
    "output",
    "prompt",
    "response",
    "critique",
    "revision",
    "instruction",
    "memory",
    "source",
    "extraction",
    "synthesis",
    "validation",
];

/// Document-render fences (`standards/markdown-fence-conventions.md`'s
/// final table): self-closing (no required matching `:::`), rendered here
/// as an empty, invisible block, matching Obsidian's own documented
/// "ignored, renders nothing visible" behaviour, since this pipeline
/// targets OFM fidelity (`web-rendering.md` §1 stage 5).
const SELF_CLOSING_FENCE_NAMES: &[&str] = &["section-break", "page-break"];

#[must_use]
pub fn is_recognised(name: &str) -> bool {
    RECOGNISED_FENCE_NAMES.contains(&name) || SELF_CLOSING_FENCE_NAMES.contains(&name)
}

/// One parsed `:::name{attrs}` opening line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceOpen {
    pub name: String,
    pub attributes: Vec<(String, String)>,
}

/// Parses a fence's opening line. Returns `None` when `line` does not open
/// a fence at all (not a `:::`-prefixed line, a bare `:::` close marker, or
/// a name containing characters outside `[a-zA-Z0-9-]`).
#[must_use]
pub fn parse_open_line(line: &str) -> Option<FenceOpen> {
    let rest = line.trim_start().strip_prefix(":::")?;
    let rest = rest.trim_end();
    if rest.is_empty() {
        return None;
    }
    let (name_part, attr_part) = match rest.split_once('{') {
        Some((name, attrs)) => (name.trim(), attrs.strip_suffix('}').unwrap_or(attrs)),
        None => (rest.trim(), ""),
    };
    if name_part.is_empty()
        || !name_part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    Some(FenceOpen {
        name: name_part.to_owned(),
        attributes: parse_attributes(attr_part),
    })
}

fn parse_attributes(raw: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut chars = raw.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while matches!(chars.peek(), Some(c) if *c != '=' && !c.is_whitespace()) {
            if let Some(c) = chars.next() {
                key.push(c);
            }
        }
        if key.is_empty() {
            break;
        }
        if chars.peek() == Some(&'=') {
            chars.next();
            let mut value = String::new();
            if chars.peek() == Some(&'"') {
                chars.next();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    value.push(c);
                }
            } else {
                while matches!(chars.peek(), Some(c) if !c.is_whitespace()) {
                    if let Some(c) = chars.next() {
                        value.push(c);
                    }
                }
            }
            attrs.push((key, value));
        } else {
            attrs.push((key, String::new()));
        }
    }
    attrs
}

/// One recognised top-level fence block, extracted from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceBlock {
    pub open: FenceOpen,
    /// Raw, unrendered inner text (empty for a self-closing fence).
    pub inner: String,
}

/// The result of one [`extract`] pass: the document with every recognised
/// top-level fence block replaced by `placeholder(index)`, and the
/// extracted blocks themselves in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractResult {
    pub text: String,
    pub blocks: Vec<FenceBlock>,
}

/// The sentinel placeholder token for the fence block at `index`, injected
/// into its own blank-line-delimited paragraph so a generic Markdown
/// parser treats it as one block-level unit the caller can find and
/// replace with the fence's own rendered HTML afterwards.
#[must_use]
pub fn placeholder(index: usize) -> String {
    format!("\u{E000}FENCE:{index}\u{E000}")
}

/// Scans `text` line by line for triple-colon fence-open lines and their
/// matching close, replacing each recognised block with its placeholder.
/// A line that opens a fence but is never closed (and is not one of the
/// self-closing document-render fences) is left as literal text: this
/// pipeline never guesses at unbalanced input by swallowing the rest of the
/// document.
#[must_use]
pub fn extract(text: &str) -> ExtractResult {
    let lines: Vec<&str> = text.lines().collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(open) = parse_open_line(line) {
            if SELF_CLOSING_FENCE_NAMES.contains(&open.name.as_str()) {
                let index = blocks.len();
                blocks.push(FenceBlock {
                    open,
                    inner: String::new(),
                });
                push_placeholder_paragraph(&mut out_lines, index);
                i += 1;
                // An immediately following bare `:::` close is optional
                // trailing syntax; consume it too when present.
                if lines.get(i).is_some_and(|l| l.trim() == ":::") {
                    i += 1;
                }
                continue;
            }
            if let Some(close_offset) = lines[i + 1..].iter().position(|l| l.trim() == ":::") {
                let inner_start = i + 1;
                let inner_end = inner_start + close_offset; // exclusive
                let inner = lines[inner_start..inner_end].join("\n");
                let index = blocks.len();
                blocks.push(FenceBlock { open, inner });
                push_placeholder_paragraph(&mut out_lines, index);
                i = inner_end + 1;
                continue;
            }
        }
        out_lines.push(line.to_owned());
        i += 1;
    }
    ExtractResult {
        text: out_lines.join("\n"),
        blocks,
    }
}

fn push_placeholder_paragraph(out_lines: &mut Vec<String>, index: usize) {
    if out_lines.last().is_some_and(|l| !l.is_empty()) {
        out_lines.push(String::new());
    }
    out_lines.push(placeholder(index));
    out_lines.push(String::new());
}

/// Renders one fence block to its final HTML, given `inner_html` (the
/// block's own inner text, already rendered through the caller's recursive
/// pass). Both a recognised and an unrecognised fence render bordered with
/// the fence's literal name as a label (FR-241): only the CSS class
/// distinguishes a themed, known semantic container from a generic one.
#[must_use]
pub fn render(open: &FenceOpen, inner_html: &str) -> String {
    if SELF_CLOSING_FENCE_NAMES.contains(&open.name.as_str()) {
        return format!(
            "<div class=\"fence fence-{}\" aria-hidden=\"true\"></div>",
            escape_html(&open.name)
        );
    }
    let recognised_class = if is_recognised(&open.name) {
        "fence-recognised"
    } else {
        "fence-unrecognised"
    };
    let title = open
        .attributes
        .iter()
        .find(|(k, _)| k == "title")
        .map(|(_, v)| v.as_str());
    let label = title.unwrap_or(&open.name);
    format!(
        "<div class=\"fence fence-{name} {recognised_class}\" data-fence=\"{name}\">\
<div class=\"fence-label\">{label}</div>\
<div class=\"fence-body\">{inner_html}</div>\
</div>",
        name = escape_html(&open.name),
        label = escape_html(label),
    )
}

#[cfg(test)]
#[path = "fences_test.rs"]
mod tests;
