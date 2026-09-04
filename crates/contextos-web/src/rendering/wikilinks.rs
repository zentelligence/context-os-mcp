//! Wikilink and embed scanning (`web-rendering.md` §1 stage 3):
//! `[[target|display]]` and `![[target]]` syntax. This module only finds
//! occurrences in raw text and renders a resolved/dead result to HTML; it
//! never itself decides whether a target resolves (that is an MCP round
//! trip the caller, `rendering::markdown`, performs via `links_read`).
//!
//! Occurrences inside a fenced code block (` ``` `) or an inline code span
//! (`` ` ``) are never treated as wikilinks, matching how a wikilink inside
//! literal code is inert in Obsidian itself.

use crate::rendering::escape_html;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSyntax {
    Link,
    Embed,
}

/// One `[[...]]`/`![[...]]` occurrence, parsed into its `target#heading^block`
/// pieces plus an optional `|display` override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOccurrence {
    pub syntax: LinkSyntax,
    pub target: String,
    pub heading: Option<String>,
    pub block: Option<String>,
    pub display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractResult {
    pub text: String,
    pub occurrences: Vec<LinkOccurrence>,
}

/// Inline placeholder for a `[[...]]` link: substituted in place within
/// whatever paragraph or list item already surrounds it.
#[must_use]
pub fn placeholder(index: usize) -> String {
    format!("\u{E000}LINK:{index}\u{E000}")
}

/// Block-level placeholder for a `![[...]]` embed: emitted on its own line
/// so the caller can treat it as one block-level unit, matching an embed's
/// own block-level rendered content (`web-rendering.md` §1 stage 3).
#[must_use]
pub fn embed_placeholder(index: usize) -> String {
    format!("\u{E000}EMBED:{index}\u{E000}")
}

/// Scans `text` for wikilink and embed occurrences, replacing each with its
/// placeholder (inline for a link, its own blank-line-delimited paragraph
/// for an embed) and returning the occurrences in document order.
#[must_use]
pub fn extract(text: &str) -> ExtractResult {
    let mut occurrences = Vec::new();
    let mut out_lines: Vec<String> = Vec::new();
    let mut in_fenced_code = false;
    for line in text.lines() {
        let fence_toggle = line.trim_start().starts_with("```");
        if fence_toggle {
            in_fenced_code = !in_fenced_code;
            out_lines.push(line.to_owned());
            continue;
        }
        if in_fenced_code {
            out_lines.push(line.to_owned());
            continue;
        }
        out_lines.push(scan_line(line, &mut occurrences));
    }
    ExtractResult {
        text: out_lines.join("\n"),
        occurrences,
    }
}

fn scan_line(line: &str, occurrences: &mut Vec<LinkOccurrence>) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut in_inline_code = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_inline_code = !in_inline_code;
            out.push(c);
            i += 1;
            continue;
        }
        if !in_inline_code {
            let is_embed = c == '!' && matches_at(&chars, i + 1, "[[");
            let is_link = c == '[' && matches_at(&chars, i, "[[");
            if is_embed || is_link {
                let open_at = if is_embed { i + 1 } else { i };
                if let Some((occurrence, next_i)) = parse_bracket(&chars, open_at) {
                    let syntax = if is_embed { LinkSyntax::Embed } else { LinkSyntax::Link };
                    let index = occurrences.len();
                    occurrences.push(LinkOccurrence { syntax, ..occurrence });
                    if is_embed {
                        // Block-level: own paragraph, matching fences/callouts.
                        if !out.trim().is_empty() {
                            out.push('\n');
                        }
                        out.push_str(&embed_placeholder(index));
                    } else {
                        out.push_str(&placeholder(index));
                    }
                    i = next_i;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn matches_at(chars: &[char], at: usize, pat: &str) -> bool {
    let pat_chars: Vec<char> = pat.chars().collect();
    if at + pat_chars.len() > chars.len() {
        return false;
    }
    chars[at..at + pat_chars.len()] == pat_chars[..]
}

/// Parses a `[[...]]` starting at `chars[open]` (the first `[` of the
/// double bracket). Returns the parsed occurrence (with `syntax` left as a
/// placeholder value the caller overwrites) and the index just past the
/// closing `]]`, or `None` when no matching `]]` exists on this line.
fn parse_bracket(chars: &[char], open: usize) -> Option<(LinkOccurrence, usize)> {
    let inner_start = open + 2;
    let mut i = inner_start;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            let inner: String = chars[inner_start..i].iter().collect();
            return Some((parse_inner(&inner), i + 2));
        }
        i += 1;
    }
    None
}

fn parse_inner(inner: &str) -> LinkOccurrence {
    let (target_part, display) = match inner.split_once('|') {
        Some((t, d)) => (t, Some(d.to_owned())),
        None => (inner, None),
    };
    // A block reference is `#^block-id` (heading marker immediately
    // followed by a caret, the real Obsidian syntax, `EMBEDS.md`) or a
    // bare `^block-id`; a plain `#heading` with no caret at all is a
    // heading reference. The two are mutually exclusive on one target, so
    // whichever matches first wins outright rather than both firing (a
    // bare `split_once('^')` before `split_once('#')` would otherwise
    // leave a spurious empty `heading: Some("")` behind for `Note#^id`,
    // since the `^` split runs first and swallows the `#` with it).
    let (target, heading, block) = if let Some((t, b)) = target_part.split_once("#^") {
        (t, None, Some(b.to_owned()))
    } else if let Some((t, b)) = target_part.split_once('^') {
        (t, None, Some(b.to_owned()))
    } else if let Some((t, h)) = target_part.split_once('#') {
        (t, Some(h.to_owned()), None)
    } else {
        (target_part, None, None)
    };
    LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: target.trim().to_owned(),
        heading,
        block,
        display,
    }
}

/// Renders a resolved link/embed reference to its `<a>` HTML.
#[must_use]
pub fn render_link(occurrence: &LinkOccurrence, href: &str) -> String {
    let text = occurrence.display.clone().unwrap_or_else(|| occurrence.target.clone());
    format!(
        "<a class=\"wikilink\" href=\"{href}\">{text}</a>",
        href = escape_html(href),
        text = escape_html(&text)
    )
}

/// Renders an unresolved (dead) link/embed reference as a visually distinct
/// span, never a broken anchor.
#[must_use]
pub fn render_dead_link(occurrence: &LinkOccurrence) -> String {
    let text = occurrence.display.clone().unwrap_or_else(|| occurrence.target.clone());
    format!(
        "<span class=\"wikilink dead\" data-target=\"{target}\">{text}</span>",
        target = escape_html(&occurrence.target),
        text = escape_html(&text)
    )
}

#[cfg(test)]
#[path = "wikilinks_test.rs"]
mod tests;
