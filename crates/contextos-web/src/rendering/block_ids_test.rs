use super::*;

#[test]
fn a_trailing_block_id_on_a_paragraph_becomes_an_invisible_anchor() {
    assert_eq!(
        apply("This paragraph can be linked to. ^my-block-id"),
        "This paragraph can be linked to. <span id=\"my-block-id\"></span>"
    );
}

#[test]
fn a_standalone_block_id_line_becomes_an_invisible_anchor_on_its_own() {
    assert_eq!(apply("^list-id"), "<span id=\"list-id\"></span>");
}

#[test]
fn a_caret_mid_word_is_not_a_block_id() {
    assert_eq!(apply("x^2 is not a reference"), "x^2 is not a reference");
}

#[test]
fn a_caret_not_at_the_end_of_the_line_is_not_a_block_id() {
    assert_eq!(
        apply("^not-at-end followed by more text"),
        "^not-at-end followed by more text"
    );
}

#[test]
fn a_block_id_marker_inside_a_fenced_code_block_is_left_untouched() {
    let doc = "```\n^not-a-block-id\n```";
    assert_eq!(apply(doc), doc);
}

#[test]
fn trailing_whitespace_after_the_marker_is_still_recognised() {
    assert_eq!(apply("Text. ^my-id   "), "Text. <span id=\"my-id\"></span>");
}

#[test]
fn extract_block_by_id_finds_an_inline_paragraph_marker() {
    let source = "# Title\n\nThis paragraph can be linked to. ^my-block-id\n\nAfter.";
    assert_eq!(
        extract_block_by_id(source, "my-block-id"),
        Some("This paragraph can be linked to.".to_owned())
    );
}

#[test]
fn extract_block_by_id_finds_a_list_via_a_standalone_marker_line() {
    // EMBEDS.md's own documented "Embed Lists" example.
    let source = "- Item 1\n- Item 2\n- Item 3\n\n^list-id\n";
    assert_eq!(
        extract_block_by_id(source, "list-id"),
        Some("- Item 1\n- Item 2\n- Item 3".to_owned())
    );
}

#[test]
fn extract_block_by_id_returns_none_when_no_marker_matches() {
    assert_eq!(extract_block_by_id("Some text.\n^other-id", "missing-id"), None);
}
