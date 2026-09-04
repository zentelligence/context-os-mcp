use super::*;

#[test]
fn wraps_a_simple_tag() {
    assert_eq!(
        apply("Tagged as #project today."),
        "Tagged as <span class=\"tag\">#project</span> today."
    );
}

#[test]
fn wraps_a_nested_tag_with_slashes() {
    assert_eq!(
        apply("See #area/topic for detail."),
        "See <span class=\"tag\">#area/topic</span> for detail."
    );
}

#[test]
fn a_tag_at_the_start_of_a_line_is_recognised() {
    assert_eq!(
        apply("#todo needs review"),
        "<span class=\"tag\">#todo</span> needs review"
    );
}

#[test]
fn a_tag_may_not_start_with_a_digit() {
    assert_eq!(apply("Issue #123 was fixed."), "Issue #123 was fixed.");
}

#[test]
fn a_hash_mid_word_is_not_a_tag() {
    assert_eq!(apply("page#anchor stays plain"), "page#anchor stays plain");
}

#[test]
fn a_url_fragment_is_not_a_tag() {
    assert_eq!(
        apply("See https://example.com/#section for detail."),
        "See https://example.com/#section for detail."
    );
}

#[test]
fn an_atx_heading_marker_is_not_a_tag() {
    assert_eq!(apply("# Heading text"), "# Heading text");
}

#[test]
fn a_tag_inside_a_fenced_code_block_is_left_untouched() {
    let doc = "```\n#not-a-tag\n```";
    assert_eq!(apply(doc), doc);
}

#[test]
fn a_tag_inside_an_inline_code_span_is_left_untouched() {
    assert_eq!(apply("Use `#not-a-tag` in code."), "Use `#not-a-tag` in code.");
}
