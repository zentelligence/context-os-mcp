//! Frontmatter strip (`web-rendering.md` §1 stage 1): YAML frontmatter is
//! not rendered inline as part of the body; a page may display it via a
//! separate `frontmatter_read` call and its own presentation, a decision
//! left to the Askama template, not this pipeline.

/// Strips a leading `---\n...\n---` YAML frontmatter block from `source`,
/// returning the remaining body text. A document with no frontmatter block,
/// or an unterminated one, is returned unchanged: this stage never guesses
/// at malformed input.
#[must_use]
pub fn strip(source: &str) -> &str {
    let Some(rest) = source.strip_prefix("---\n") else {
        return source;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return &rest[offset + line.len()..];
        }
        offset += line.len();
    }
    source
}

#[cfg(test)]
#[path = "frontmatter_test.rs"]
mod tests;
