//! Obsidian-style callout parsing (`web-rendering.md` §1 stage 4):
//! `> [!type] Title` blockquote runs, rendered per their type.
//!
//! A callout is a contiguous run of `>`-prefixed lines whose first line
//! carries a `[!type]` marker (optionally followed by `+`/`-` fold state,
//! ignored here: `contextos-web` renders every callout expanded, since it
//! has no client-side fold interaction in v1). The run ends at the first
//! line that is not itself `>`-prefixed.

use crate::rendering::escape_html;

/// One parsed callout opening line: `> [!type]` optionally followed by a
/// title on the same line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalloutOpen {
    pub kind: String,
    pub title: Option<String>,
}

/// Parses a callout's opening line. Returns `None` when `line` is not a
/// `> [!type]` line at all.
#[must_use]
pub fn parse_open_line(line: &str) -> Option<CalloutOpen> {
    let rest = line.trim_start().strip_prefix('>')?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let rest = rest.strip_prefix("[!")?;
    let (kind, after) = rest.split_once(']')?;
    let kind = kind.trim();
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    // Fold-state marker (`+`/`-`), if present, is not rendered: v1 has no
    // client-side fold interaction, so every callout renders expanded.
    let after = after
        .strip_prefix('+')
        .or_else(|| after.strip_prefix('-'))
        .unwrap_or(after);
    let title = after.trim();
    Some(CalloutOpen {
        kind: kind.to_ascii_lowercase(),
        title: if title.is_empty() { None } else { Some(title.to_owned()) },
    })
}

/// One extracted callout block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalloutBlock {
    pub open: CalloutOpen,
    /// Body lines with their leading `>` (and one following space, when
    /// present) already stripped.
    pub body: String,
}

/// The result of one [`extract`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractResult {
    pub text: String,
    pub blocks: Vec<CalloutBlock>,
}

#[must_use]
pub fn placeholder(index: usize) -> String {
    format!("\u{E000}CALLOUT:{index}\u{E000}")
}

fn strip_quote_prefix(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// Scans `text` for `> [!type]` callout runs, replacing each with its
/// placeholder.
#[must_use]
pub fn extract(text: &str) -> ExtractResult {
    let lines: Vec<&str> = text.lines().collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(open) = parse_open_line(line) {
            let mut body_lines = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                if let Some(stripped) = strip_quote_prefix(lines[j]) {
                    body_lines.push(stripped.to_owned());
                    j += 1;
                } else {
                    break;
                }
            }
            let index = blocks.len();
            blocks.push(CalloutBlock {
                open,
                body: body_lines.join("\n"),
            });
            if out_lines.last().is_some_and(|l: &String| !l.is_empty()) {
                out_lines.push(String::new());
            }
            out_lines.push(placeholder(index));
            out_lines.push(String::new());
            i = j;
            continue;
        }
        out_lines.push(line.to_owned());
        i += 1;
    }
    ExtractResult {
        text: out_lines.join("\n"),
        blocks,
    }
}

/// Renders one callout block to HTML, given `body_html` (its own body,
/// already rendered through the caller's recursive pass).
#[must_use]
pub fn render(open: &CalloutOpen, body_html: &str) -> String {
    let title = open.title.clone().unwrap_or_else(|| titlecase(&open.kind));
    format!(
        "<div class=\"callout callout-{kind}\" data-callout=\"{kind}\">\
<div class=\"callout-title\">{title}</div>\
<div class=\"callout-body\">{body_html}</div>\
</div>",
        kind = escape_html(&open.kind),
        title = escape_html(&title),
    )
}

fn titlecase(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
#[path = "callouts_test.rs"]
mod tests;
