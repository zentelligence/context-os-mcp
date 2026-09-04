//! Obsidian block reference definitions (`web-rendering.md` §1 stage 5,
//! the `obsidian-markdown` skill's own `^block-id` convention: appended to
//! a paragraph's own last line inline, or standing alone on its own line
//! immediately after a list or quote, `EMBEDS.md`'s "Embed Lists"
//! section): a `^block-id` marks the block just before it as addressable
//! by `[[Note#^block-id]]`.
//!
//! Rewrites each recognised marker into an invisible anchor
//! (`<span id="block-id"></span>`) at the point it appears and removes the
//! marker's own visible text, matching how Obsidian hides the marker
//! itself in reading view; `markdown::append_fragment` appends the
//! matching `#block-id` fragment to a resolved link/embed's own href.
//!
//! Runs as a raw-text scan, a `fences`/`wikilinks`/`highlight` sibling: a
//! `^block-id`-shaped line inside a fenced code block is left untouched. A
//! trailing `^word` is recognised only at a genuine line end (after
//! trimming trailing whitespace) and only when preceded by whitespace or
//! the start of the line, matching Obsidian's own behaviour: there is no
//! extra disambiguation beyond that, since Obsidian itself has none either
//! (a paragraph that happens to end in `^` followed by a word is always a
//! block reference, full stop).

fn is_block_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-'
}

/// Rewrites every recognised trailing `^block-id` marker in `text` into an
/// invisible anchor span.
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
    let trimmed_end = line.trim_end();
    let Some(caret) = trimmed_end.rfind('^') else {
        return line.to_owned();
    };
    let id_candidate = &trimmed_end[caret + 1..];
    if id_candidate.is_empty() || !id_candidate.chars().all(is_block_id_char) {
        return line.to_owned();
    }
    let preceding_boundary = caret == 0 || trimmed_end[..caret].ends_with(char::is_whitespace);
    if !preceding_boundary {
        return line.to_owned();
    }
    let before = trimmed_end[..caret].trim_end();
    let anchor = format!("<span id=\"{}\"></span>", crate::rendering::escape_html(id_candidate));
    if before.is_empty() {
        anchor
    } else {
        format!("{before} {anchor}")
    }
}

/// Extracts the raw text of the block immediately preceding a trailing
/// `^block_id` marker matching `block_id`, from `source` (a target note's
/// own raw content, before any of this pipeline's own rewriting runs on
/// it): the contiguous run of non-blank lines ending at the marker (a
/// paragraph's own inline marker) or immediately before it (the
/// standalone-line-after-a-list/quote form, `EMBEDS.md`'s "Embed Lists"
/// example), skipping the blank line(s) between the block and a standalone
/// marker. `None` when no marker matching `block_id` exists.
///
/// A heuristic bounded to "the contiguous non-blank run," not a full
/// `CommonMark` block parse: correct for the plain-paragraph and
/// plain-list/quote cases the skill documents, not guaranteed for more
/// irregular structure (nested lists, a table immediately above the
/// marker, and so on). `markdown::render_embed_occurrence` uses this for a
/// `![[Note#^block-id]]` embed, so it inlines just the referenced block
/// rather than the whole target note.
#[must_use]
pub fn extract_block_by_id(source: &str, block_id: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let marker_idx = lines.iter().position(|line| {
        let trimmed_end = line.trim_end();
        trimmed_end
            .rfind('^')
            .is_some_and(|caret| &trimmed_end[caret + 1..] == block_id)
    })?;
    let marker_line = lines[marker_idx].trim_end();
    let caret = marker_line.rfind('^')?;
    let before_marker = marker_line[..caret].trim_end();

    let (content_end, last_line) = if before_marker.is_empty() {
        let mut idx = marker_idx.checked_sub(1)?;
        while lines.get(idx).is_some_and(|l| l.trim().is_empty()) {
            idx = idx.checked_sub(1)?;
        }
        (idx, lines[idx].to_owned())
    } else {
        (marker_idx, before_marker.to_owned())
    };
    let mut start = content_end;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let mut block_lines: Vec<String> = lines[start..content_end].iter().map(|l| (*l).to_owned()).collect();
    block_lines.push(last_line);
    Some(block_lines.join("\n"))
}

#[cfg(test)]
#[path = "block_ids_test.rs"]
mod tests;
