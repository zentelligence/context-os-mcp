//! Obsidian highlight syntax (`web-rendering.md` §1 stage 5, the
//! `obsidian-markdown` skill's own `==highlighted text==` convention):
//! rewrites every `==text==` run into a raw `<mark>text</mark>` tag before
//! the general Markdown pass runs, since `CommonMark` has no native notion of
//! this syntax and would otherwise leave the `==` characters as literal
//! text. The wrapped content itself is left as unescaped Markdown source
//! (not HTML): `CommonMark` treats `<mark>`/`</mark>` as ordinary raw inline
//! HTML and keeps parsing normal inline syntax (bold, links, wikilink
//! placeholders already substituted in by an earlier stage) between them,
//! so `==**bold** highlight==` still bolds correctly.
//!
//! Runs as a raw-text scan, a `fences`/`wikilinks` sibling: a `==` run
//! inside a fenced code block or inline code span is left untouched,
//! matching how the construct is inert inside literal code in Obsidian
//! itself. A highlight run never crosses a line: this mirrors
//! `wikilinks::extract`'s own single-line scope for `[[...]]`, not a
//! separate limitation invented here.

/// Rewrites every `==text==` run in `text` into `<mark>text</mark>`,
/// skipping fenced code blocks and inline code spans.
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
        if !in_inline_code
            && c == '='
            && chars.get(i + 1) == Some(&'=')
            && let Some(close) = find_close(&chars, i + 2)
        {
            let inner: String = chars[i + 2..close].iter().collect();
            if !inner.is_empty() {
                out.push_str("<mark>");
                out.push_str(&inner);
                out.push_str("</mark>");
                i = close + 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Finds the closing `==` starting at or after `from`, on the same line.
fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '=' && chars[i + 1] == '=' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
#[path = "highlight_test.rs"]
mod tests;
