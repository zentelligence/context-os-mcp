use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn parses_a_callout_with_no_title() -> TestResult {
    let open = parse_open_line("> [!note]").ok_or("expected a parsed open line")?;
    assert_eq!(open.kind, "note");
    assert_eq!(open.title, None);
    Ok(())
}

#[test]
fn parses_a_callout_with_a_title() -> TestResult {
    let open = parse_open_line("> [!warning] Breaking change").ok_or("expected a parsed open line")?;
    assert_eq!(open.kind, "warning");
    assert_eq!(open.title.as_deref(), Some("Breaking change"));
    Ok(())
}

#[test]
fn parses_a_foldable_callout_ignoring_the_fold_marker() -> TestResult {
    let open = parse_open_line("> [!tip]- Collapsed by default").ok_or("expected a parsed open line")?;
    assert_eq!(open.kind, "tip");
    assert_eq!(open.title.as_deref(), Some("Collapsed by default"));
    Ok(())
}

#[test]
fn a_plain_blockquote_line_is_not_a_callout() {
    assert_eq!(parse_open_line("> just a quote"), None);
}

#[test]
fn a_non_blockquote_line_is_not_a_callout() {
    assert_eq!(parse_open_line("[!note] not a blockquote"), None);
}

#[test]
fn extracts_a_multi_line_callout_body() {
    let doc = "> [!note]\n> Line one.\n> Line two.\n\nAfter.";
    let result = extract(doc);
    assert_eq!(result.blocks.len(), 1);
    assert_eq!(result.blocks[0].body, "Line one.\nLine two.");
    assert!(result.text.contains("After."));
    assert!(!result.text.contains("Line one."));
}

#[test]
fn a_line_without_the_quote_prefix_ends_the_callout() {
    let doc = "> [!note]\n> Inside.\nOutside.";
    let result = extract(doc);
    assert_eq!(result.blocks[0].body, "Inside.");
    assert!(result.text.contains("Outside."));
}

#[test]
fn rendering_uses_a_titlecased_kind_when_no_title_is_given() {
    let open = CalloutOpen {
        kind: "warning".to_owned(),
        title: None,
    };
    let html = render(&open, "<p>Body.</p>");
    assert!(html.contains(">Warning<"));
    assert!(html.contains("callout-warning"));
}

#[test]
fn rendering_prefers_an_explicit_title() {
    let open = CalloutOpen {
        kind: "note".to_owned(),
        title: Some("Custom".to_owned()),
    };
    let html = render(&open, "");
    assert!(html.contains(">Custom<"));
}

#[test]
fn a_documented_alias_is_styled_as_its_canonical_type() {
    let open = CalloutOpen {
        kind: "hint".to_owned(),
        title: None,
    };
    let html = render(&open, "");
    // Styling uses the canonical type ("tip")...
    assert!(html.contains("class=\"callout callout-tip\""));
    // ...but the author's own literal kind is preserved for CSS overrides
    // and the default title still reflects what they actually typed.
    assert!(html.contains("data-callout=\"hint\""));
    assert!(html.contains(">Hint<"));
}

#[test]
fn every_documented_alias_maps_to_its_canonical_type() {
    let cases = [
        ("summary", "abstract"),
        ("tldr", "abstract"),
        ("hint", "tip"),
        ("important", "tip"),
        ("check", "success"),
        ("done", "success"),
        ("help", "question"),
        ("faq", "question"),
        ("caution", "warning"),
        ("attention", "warning"),
        ("fail", "failure"),
        ("missing", "failure"),
        ("error", "danger"),
        ("cite", "quote"),
    ];
    for (alias, canonical) in cases {
        assert_eq!(canonical_kind(alias), canonical, "alias {alias:?}");
    }
}

#[test]
fn an_already_canonical_or_undocumented_kind_maps_to_itself() {
    assert_eq!(canonical_kind("note"), "note");
    assert_eq!(canonical_kind("some-custom-type"), "some-custom-type");
}
