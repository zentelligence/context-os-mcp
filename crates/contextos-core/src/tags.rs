use serde_json::{Map, Value};

/// Returns frontmatter and inline tags, deduplicated in source order.
///
/// Shared by `contextos-search` (`IndexedDocument::tags`) and
/// `contextos-obsidian` (`base_query`'s `file.hasTag()` filter leaf) so both
/// consumers apply exactly the same Obsidian tag rules: the `tags`
/// frontmatter key (array or single string) contributes first, then inline
/// `#tag` occurrences in the body, skipping fenced code blocks and inline
/// code spans, and rejecting all-numeric candidates (Obsidian does not treat
/// `#2024` as a tag).
#[must_use]
pub fn extract_tags(frontmatter: &Map<String, Value>, body: &str) -> Vec<String> {
    let mut tags = Vec::new();
    match frontmatter.get("tags") {
        Some(Value::Array(values)) => {
            for value in values {
                if let Some(tag) = value.as_str() {
                    push_tag(&mut tags, tag);
                }
            }
        }
        Some(Value::String(value)) => push_tag(&mut tags, value),
        _ => {}
    }
    collect_inline_tags(body, &mut tags);
    tags
}

fn push_tag(tags: &mut Vec<String>, raw: &str) {
    let tag = raw.trim().trim_start_matches('#');
    if is_valid_tag(tag) && !tags.iter().any(|existing| existing == tag) {
        tags.push(tag.to_owned());
    }
}

/// An Obsidian tag needs one non-numeric character from its allowed set.
fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '/'))
        && tag.chars().any(|character| !character.is_ascii_digit())
}

fn collect_inline_tags(body: &str, tags: &mut Vec<String>) {
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line_tags(line, tags);
    }
}

fn scan_line_tags(line: &str, tags: &mut Vec<String>) {
    let mut previous: Option<char> = None;
    let mut inline_ticks: Option<usize> = None;
    let mut characters = line.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if character == '`' {
            let mut run = 1_usize;
            while characters.peek().is_some_and(|(_, next)| *next == '`') {
                characters.next();
                run = run.saturating_add(1);
            }
            inline_ticks = match inline_ticks {
                Some(opening) if opening == run => None,
                Some(opening) => Some(opening),
                None => Some(run),
            };
            previous = Some('`');
            continue;
        }
        if inline_ticks.is_none() && character == '#' && previous.is_none_or(char::is_whitespace) {
            let candidate: String = line[offset..]
                .chars()
                .skip(1)
                .take_while(|next| next.is_alphanumeric() || matches!(next, '-' | '_' | '/'))
                .collect();
            if is_valid_tag(&candidate) {
                push_tag(tags, &candidate);
            }
        }
        previous = Some(character);
    }
}

#[cfg(test)]
mod tests {
    use super::extract_tags;
    use serde_json::{Map, Value, json};

    fn frontmatter(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    #[test]
    fn extract_tags_reads_a_frontmatter_array_before_inline_tags() {
        let frontmatter = frontmatter(json!({ "tags": ["project/alpha", "review"] }));
        let body = "Intro with an inline #status/active tag.\n";
        assert_eq!(
            extract_tags(&frontmatter, body),
            ["project/alpha", "review", "status/active"]
        );
    }

    #[test]
    fn extract_tags_reads_a_single_string_frontmatter_tag() {
        let frontmatter = frontmatter(json!({ "tags": "solo" }));
        assert_eq!(extract_tags(&frontmatter, ""), ["solo"]);
    }

    #[test]
    fn extract_tags_skips_fenced_and_inline_code_and_all_numeric_candidates() {
        let frontmatter = frontmatter(json!({ "tags": "solo" }));
        let body = "Text #alpha and `#inline-code` and #2024 and #a1.\n\n~~~\n#fenced\n~~~\n\nRepeat #alpha once.\n";
        assert_eq!(extract_tags(&frontmatter, body), ["solo", "alpha", "a1"]);
    }

    #[test]
    fn extract_tags_ignores_a_missing_or_non_string_tags_key() {
        let frontmatter = frontmatter(json!({ "tags": 42 }));
        assert!(extract_tags(&frontmatter, "no inline tags here").is_empty());
    }
}
