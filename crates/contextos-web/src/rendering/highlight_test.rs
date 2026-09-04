use super::*;

#[test]
fn wraps_a_highlighted_run_in_mark() {
    assert_eq!(
        apply("This is ==important== text."),
        "This is <mark>important</mark> text."
    );
}

#[test]
fn leaves_an_unmatched_delimiter_as_literal_text() {
    assert_eq!(apply("No closing == here."), "No closing == here.");
}

#[test]
fn a_highlight_inside_a_fenced_code_block_is_left_untouched() {
    let doc = "```\n==not a highlight==\n```";
    assert_eq!(apply(doc), doc);
}

#[test]
fn a_highlight_inside_an_inline_code_span_is_left_untouched() {
    assert_eq!(apply("Use `==literal==` in code."), "Use `==literal==` in code.");
}

#[test]
fn nested_inline_markdown_survives_inside_a_highlight() {
    assert_eq!(apply("==**bold** highlight=="), "<mark>**bold** highlight</mark>");
}

#[test]
fn an_empty_highlight_run_is_left_as_literal_text() {
    assert_eq!(apply("Nothing between ==== here."), "Nothing between ==== here.");
}

#[test]
fn a_highlight_run_does_not_cross_a_line_break() {
    let doc = "==opens here\nbut this line is plain==";
    assert_eq!(apply(doc), doc);
}
