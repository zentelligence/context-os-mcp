//! Obsidian comment syntax (`web-rendering.md` §1 stage 5, the
//! `obsidian-markdown` skill's own inline `%%hidden%%` and block
//! `%%\n...\n%%` convention): strips both forms entirely before any later
//! stage sees them, since Obsidian hides comment content completely in
//! reading view, never rendering it in any form (unlike, say, a triple-colon
//! fence, which always renders *something*).
//!
//! Runs first among this pipeline's raw-text stages (ahead of fence,
//! callout, and wikilink scanning, `markdown::compile`), so a comment's own
//! content, whatever it happens to contain, never gets a chance to be
//! misread as one of those constructs. A `%%` run inside a fenced code
//! block or inline code span is left untouched, matching how the construct
//! is inert inside literal code in Obsidian itself. An unterminated block
//! comment (a bare `%%` line with no matching close) is left as literal
//! text: this stage never guesses at malformed input by swallowing the
//! rest of the document.

/// Strips every comment in `text`, block and inline, returning what
/// remains.
#[must_use]
pub fn strip(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_fenced_code = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            in_fenced_code = !in_fenced_code;
            out_lines.push(line.to_owned());
            i += 1;
            continue;
        }
        if in_fenced_code {
            out_lines.push(line.to_owned());
            i += 1;
            continue;
        }
        if line.trim() == "%%" {
            if let Some(close_offset) = lines[i + 1..].iter().position(|l| l.trim() == "%%") {
                // Open, body, and close lines are all dropped entirely.
                i += 1 + close_offset + 1;
                continue;
            }
            out_lines.push(line.to_owned());
            i += 1;
            continue;
        }
        out_lines.push(strip_inline(line));
        i += 1;
    }
    out_lines.join("\n")
}

fn strip_inline(line: &str) -> String {
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
        if !in_inline_code
            && c == '%'
            && chars.get(i + 1) == Some(&'%')
            && let Some(close) = find_close(&chars, i + 2)
        {
            i = close + 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Finds the closing `%%` starting at or after `from`, on the same line.
fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '%' && chars[i + 1] == '%' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
#[path = "comments_test.rs"]
mod tests;
