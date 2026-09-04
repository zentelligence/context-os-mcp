use super::*;

#[test]
fn strips_an_inline_comment() {
    assert_eq!(strip("Visible %%but this is hidden%% text."), "Visible  text.");
}

#[test]
fn strips_a_block_comment_entirely() {
    let doc = "Before.\n%%\nThis entire block is hidden.\nSo is this line.\n%%\nAfter.";
    assert_eq!(strip(doc), "Before.\nAfter.");
}

#[test]
fn an_unterminated_block_comment_is_left_as_literal_text() {
    let doc = "Before.\n%%\nNo closing marker.";
    assert_eq!(strip(doc), doc);
}

#[test]
fn a_comment_marker_inside_a_fenced_code_block_is_left_untouched() {
    let doc = "```\n%%not a comment%%\n```";
    assert_eq!(strip(doc), doc);
}

#[test]
fn a_comment_marker_inside_an_inline_code_span_is_left_untouched() {
    assert_eq!(strip("Use `%%literal%%` in code."), "Use `%%literal%%` in code.");
}

#[test]
fn multiple_inline_comments_on_one_line_are_both_stripped() {
    assert_eq!(strip("A %%one%% B %%two%% C"), "A  B  C");
}
