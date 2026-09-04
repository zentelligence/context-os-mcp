//! Obsidian inline tag syntax (`web-rendering.md` §1 stage 5, the
//! `obsidian-markdown` skill's own `#tag`/`#nested/tag` convention): wraps
//! every inline tag occurrence in a styled, non-interactive span (this
//! application has no tag-search page yet to link one to; the wrapper is
//! visual only).
//!
//! Runs as a raw-text scan, a `fences`/`wikilinks`/`highlight` sibling: a
//! `#` inside a fenced code block or inline code span is left untouched.
//! A tag is only recognised at a natural text boundary (line start, after
//! whitespace, or immediately after an opening bracket/quote): this
//! deliberately excludes a `#` reached via `/` (a URL fragment such as
//! `https://example.com/#section`) or any other word character (`page#123`
//! mid-identifier), both of which are not tags even though they share the
//! `#` character. The first character of the tag body itself must not be
//! a digit, matching the skill's own "numbers (not first character)" rule
//! (also, incidentally, keeping a Markdown ATX heading's `# ` — always
//! followed by a space — from ever being mistaken for a tag).

fn is_tag_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_tag_body(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

fn is_boundary(preceding: Option<char>) -> bool {
    match preceding {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''),
    }
}

/// Rewrites every recognised `#tag` occurrence in `text` into
/// `<span class="tag">#tag</span>`.
#[must_use]
pub fn apply(text: &str) -> String {
    let mut in_fenced_code = false;
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fenced_code = !in_fenced_code;
            out_lines.push(line.to_owned());
            continue;
        }
        if in_fenced_code {
            out_lines.push(line.to_owned());
            continue;
        }
        out_lines.push(apply_line(line));
    }
    out_lines.join("\n")
}

fn apply_line(line: &str) -> String {
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
        let preceding = if i == 0 { None } else { Some(chars[i - 1]) };
        if !in_inline_code && c == '#' && is_boundary(preceding) && chars.get(i + 1).is_some_and(|&n| is_tag_start(n)) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_tag_body(chars[end]) {
                end += 1;
            }
            let tag: String = chars[start..end].iter().collect();
            out.push_str("<span class=\"tag\">#");
            out.push_str(&crate::rendering::escape_html(&tag));
            out.push_str("</span>");
            i = end;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "tags_test.rs"]
mod tests;
