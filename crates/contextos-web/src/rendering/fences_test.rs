use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn parses_a_bare_fence_name() -> TestResult {
    let open = parse_open_line(":::note").ok_or("expected a parsed open line")?;
    assert_eq!(open.name, "note");
    assert!(open.attributes.is_empty());
    Ok(())
}

#[test]
fn parses_attributes_with_quoted_and_bare_values() -> TestResult {
    let open = parse_open_line(r#":::note{title="Why this matters" cols=3}"#)
        .ok_or("expected a parsed open line with attributes")?;
    assert_eq!(open.name, "note");
    assert_eq!(
        open.attributes,
        vec![
            ("title".to_owned(), "Why this matters".to_owned()),
            ("cols".to_owned(), "3".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn a_bare_close_marker_is_not_an_open_line() {
    assert_eq!(parse_open_line(":::"), None);
}

#[test]
fn a_non_fence_line_is_not_an_open_line() {
    assert_eq!(parse_open_line("plain text"), None);
}

#[test]
fn a_name_with_invalid_characters_is_rejected() {
    assert_eq!(parse_open_line(":::not a valid name"), None);
}

#[test]
fn extracts_one_recognised_fence_block() {
    let doc = ":::warning\nThis may break consumers.\n:::\n";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 1);
    assert_eq!(result.blocks[0].open.name, "warning");
    assert_eq!(result.blocks[0].inner, "This may break consumers.");
    assert!(result.text.contains(&placeholder(0)));
    assert!(!result.text.contains(":::warning"));
}

#[test]
fn extracts_multiple_fence_blocks_in_document_order() {
    let doc = ":::note\nFirst.\n:::\n\nSome prose.\n\n:::decision\nSecond.\n:::\n";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 2);
    assert_eq!(result.blocks[0].open.name, "note");
    assert_eq!(result.blocks[1].open.name, "decision");
    assert!(result.text.contains("Some prose."));
}

#[test]
fn a_fence_containing_nested_fences_of_the_same_notation_captures_its_full_extent() {
    // The convention document's own canonical `:::grid` example: the
    // outer fence's own close must not be confused with an inner card's.
    let doc = ":::grid{cols=3}\n\n:::card{title=\"Build\"}\nCompile.\n:::\n\n:::card{title=\"Test\"}\nRun tests.\n:::\n\n:::\n";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 1, "the outer grid is the only top-level block");
    assert_eq!(result.blocks[0].open.name, "grid");
    // The outer block's own raw inner text still contains both nested
    // cards verbatim, unprocessed: recursing into it is the caller's job.
    assert!(result.blocks[0].inner.contains(":::card{title=\"Build\"}"));
    assert!(result.blocks[0].inner.contains("Compile."));
    assert!(result.blocks[0].inner.contains(":::card{title=\"Test\"}"));
    assert!(result.blocks[0].inner.contains("Run tests."));
}

#[test]
fn a_self_closing_fence_nested_inside_another_does_not_confuse_depth_counting() {
    let doc = ":::note\nBefore.\n\n:::page-break\n\nAfter.\n:::\n";
    let result = extract(doc);
    assert_eq!(
        result.blocks.len(),
        1,
        "the page-break is captured raw inside note's own inner text"
    );
    assert_eq!(result.blocks[0].open.name, "note");
    assert!(result.blocks[0].inner.contains("Before."));
    assert!(result.blocks[0].inner.contains(":::page-break"));
    assert!(result.blocks[0].inner.contains("After."));
}

#[test]
fn an_unclosed_fence_is_left_as_literal_text() {
    let doc = ":::note\nnever closed";
    let result = extract(doc);
    assert!(result.blocks.is_empty());
    assert_eq!(result.text, doc);
}

#[test]
fn page_break_is_self_closing_with_no_required_matching_close() {
    let doc = "before\n\n:::page-break\n\nafter";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 1);
    assert_eq!(result.blocks[0].open.name, "page-break");
    assert_eq!(result.blocks[0].inner, "");
    assert!(result.text.contains("before"));
    assert!(result.text.contains("after"));
}

#[test]
fn page_break_consumes_an_optional_trailing_close_marker() {
    let doc = ":::page-break\n:::\nafter";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 1);
    assert!(result.text.contains("after"));
    assert!(!result.text.contains(":::\nafter"));
}

#[test]
fn recognised_fence_names_are_recognised() {
    assert!(is_recognised("warning"));
    assert!(is_recognised("decision"));
    assert!(is_recognised("page-break"));
}

#[test]
fn an_unrecognised_fence_name_is_not_recognised() {
    assert!(!is_recognised("mystery-fence"));
}

#[test]
fn rendering_a_recognised_fence_carries_its_name_as_a_label() {
    let open = FenceOpen {
        name: "warning".to_owned(),
        attributes: Vec::new(),
    };
    let html = render(&open, "<p>Body.</p>");
    assert!(html.contains("fence-recognised"));
    assert!(html.contains(">warning<"));
    assert!(html.contains("<p>Body.</p>"));
}

#[test]
fn rendering_an_unrecognised_fence_still_shows_its_literal_name_never_dropped() {
    let open = FenceOpen {
        name: "mystery-fence".to_owned(),
        attributes: Vec::new(),
    };
    let html = render(&open, "<p>Body.</p>");
    assert!(html.contains("fence-unrecognised"));
    assert!(html.contains(">mystery-fence<"));
}

#[test]
fn a_title_attribute_overrides_the_label_text() {
    let open = FenceOpen {
        name: "note".to_owned(),
        attributes: vec![("title".to_owned(), "Custom title".to_owned())],
    };
    let html = render(&open, "");
    assert!(html.contains(">Custom title<"));
}

#[test]
fn page_break_renders_as_an_empty_invisible_block() {
    let open = FenceOpen {
        name: "page-break".to_owned(),
        attributes: Vec::new(),
    };
    let html = render(&open, "");
    assert!(html.contains("aria-hidden=\"true\""));
}
