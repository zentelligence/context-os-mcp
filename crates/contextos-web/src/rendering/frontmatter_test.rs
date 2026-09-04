use super::*;

#[test]
fn strips_a_leading_frontmatter_block() {
    let source = "---\ntype: note\nstatus: active\n---\nBody text.";
    assert_eq!(strip(source), "Body text.");
}

#[test]
fn a_document_with_no_frontmatter_is_returned_unchanged() {
    let source = "# Heading\n\nBody text.";
    assert_eq!(strip(source), source);
}

#[test]
fn an_unterminated_frontmatter_block_is_returned_unchanged() {
    let source = "---\ntype: note\nnever closed";
    assert_eq!(strip(source), source);
}

#[test]
fn an_empty_frontmatter_block_strips_to_the_remaining_body() {
    let source = "---\n---\nBody.";
    assert_eq!(strip(source), "Body.");
}
