use super::*;

#[test]
fn extracts_a_plain_wikilink() {
    let result = extract("See [[target-note]] for details.");
    assert_eq!(result.occurrences.len(), 1);
    let occ = &result.occurrences[0];
    assert_eq!(occ.syntax, LinkSyntax::Link);
    assert_eq!(occ.target, "target-note");
    assert_eq!(occ.display, None);
    assert!(result.text.contains(&placeholder(0)));
    assert!(!result.text.contains("[[target-note]]"));
}

#[test]
fn extracts_a_wikilink_with_a_display_override() {
    let result = extract("[[target-note|Custom display]]");
    assert_eq!(result.occurrences[0].target, "target-note");
    assert_eq!(result.occurrences[0].display.as_deref(), Some("Custom display"));
}

#[test]
fn extracts_a_wikilink_with_a_heading_and_block_reference() {
    let heading = extract("[[note#Heading]]");
    assert_eq!(heading.occurrences[0].target, "note");
    assert_eq!(heading.occurrences[0].heading.as_deref(), Some("Heading"));

    let block = extract("[[note^abc123]]");
    assert_eq!(block.occurrences[0].target, "note");
    assert_eq!(block.occurrences[0].block.as_deref(), Some("abc123"));
}

#[test]
fn extracts_a_block_reference_using_the_documented_hash_caret_syntax() {
    // `EMBEDS.md`'s own documented form: `#^block-id`, not a bare `^`.
    let result = extract("[[note#^abc123]]");
    let occ = &result.occurrences[0];
    assert_eq!(occ.target, "note");
    assert_eq!(occ.block.as_deref(), Some("abc123"));
    assert_eq!(
        occ.heading, None,
        "a block reference must not also carry a spurious empty heading"
    );
}

#[test]
fn extracts_an_embed_as_a_block_level_placeholder() {
    let result = extract("before\n\n![[embedded-note]]\n\nafter");
    assert_eq!(result.occurrences.len(), 1);
    assert_eq!(result.occurrences[0].syntax, LinkSyntax::Embed);
    assert!(result.text.contains(&embed_placeholder(0)));
}

#[test]
fn extracts_multiple_occurrences_in_document_order() {
    let result = extract("[[first]] and [[second]]");
    assert_eq!(result.occurrences.len(), 2);
    assert_eq!(result.occurrences[0].target, "first");
    assert_eq!(result.occurrences[1].target, "second");
}

#[test]
fn a_wikilink_inside_an_inline_code_span_is_not_extracted() {
    let result = extract("Use `[[not-a-link]]` literally.");
    assert!(result.occurrences.is_empty());
    assert!(result.text.contains("[[not-a-link]]"));
}

#[test]
fn a_wikilink_inside_a_fenced_code_block_is_not_extracted() {
    let doc = "```\n[[not-a-link]]\n```";
    let result = extract(doc);
    assert!(result.occurrences.is_empty());
    assert_eq!(result.text, doc);
}

#[test]
fn an_unclosed_double_bracket_is_left_as_literal_text() {
    let result = extract("[[never closed");
    assert!(result.occurrences.is_empty());
    assert!(result.text.contains("[[never closed"));
}

#[test]
fn render_link_uses_the_display_text_when_present() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: "target-note".to_owned(),
        heading: None,
        block: None,
        display: Some("Custom".to_owned()),
    };
    let html = render_link(&occurrence, "/example-vault/target-note.md");
    assert!(html.contains(">Custom<"));
    assert!(html.contains("wikilink"));
    assert!(!html.contains("dead"));
}

#[test]
fn render_dead_link_is_visually_distinct_and_not_a_broken_anchor() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: "missing-note".to_owned(),
        heading: None,
        block: None,
        display: None,
    };
    let html = render_dead_link(&occurrence);
    assert!(html.contains("dead"));
    assert!(!html.contains("<a "));
    assert!(html.contains(">missing-note<"));
}
