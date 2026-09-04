use super::*;

#[test]
fn html_from_commonmark_renders_basic_markdown() {
    let html = html_from_commonmark("# Heading\n\nBody **text**.");
    assert!(html.contains("<h1 id=\"heading\">Heading</h1>"));
    assert!(html.contains("<strong>text</strong>"));
}

#[test]
fn html_from_commonmark_renders_inline_and_display_math() {
    let html = html_from_commonmark("Inline $e^{i\\pi}$ and:\n\n$$a = b$$\n");
    assert!(html.contains(r#"<span class="math math-inline">"#));
    assert!(html.contains(r#"<span class="math math-display">"#));
    assert!(!html.contains('$'), "math delimiters should not leak into the output");
}

#[test]
fn parse_image_size_reads_a_width_only_hint() {
    assert_eq!(parse_image_size("300"), Some((300, None)));
}

#[test]
fn parse_image_size_reads_a_width_and_height_hint() {
    assert_eq!(parse_image_size("640x480"), Some((640, Some(480))));
}

#[test]
fn parse_image_size_is_none_for_an_unparseable_hint() {
    assert_eq!(parse_image_size("not-a-size"), None);
}

#[test]
fn render_image_embed_applies_a_width_only_size_hint() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Embed,
        target: "photo.jpg".to_owned(),
        heading: None,
        block: None,
        display: Some("300".to_owned()),
    };
    let html = render_image_embed(&occurrence, "/vault/photo.jpg");
    assert!(html.contains("width=\"300\""));
    assert!(!html.contains("height"));
}

#[test]
fn render_image_embed_applies_a_width_and_height_size_hint() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Embed,
        target: "photo.jpg".to_owned(),
        heading: None,
        block: None,
        display: Some("640x480".to_owned()),
    };
    let html = render_image_embed(&occurrence, "/vault/photo.jpg");
    assert!(html.contains("width=\"640\""));
    assert!(html.contains("height=\"480\""));
}

#[test]
fn render_image_embed_with_no_size_hint_omits_size_attributes() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Embed,
        target: "photo.jpg".to_owned(),
        heading: None,
        block: None,
        display: None,
    };
    let html = render_image_embed(&occurrence, "/vault/photo.jpg");
    assert!(!html.contains("width"));
    assert!(!html.contains("height"));
}

#[test]
fn is_audio_path_recognises_common_audio_extensions() {
    assert!(is_audio_path("/vault/audio.mp3"));
    assert!(is_audio_path("/vault/audio.OGG"));
    assert!(!is_audio_path("/vault/note.md"));
}

#[test]
fn is_pdf_path_recognises_the_pdf_extension() {
    assert!(is_pdf_path("/vault/document.pdf"));
    assert!(!is_pdf_path("/vault/document.md"));
}

#[test]
fn html_from_commonmark_supports_gfm_tables() {
    let html = html_from_commonmark("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
    assert!(html.contains("<table>"));
}

#[test]
fn compile_strips_frontmatter_before_rendering() {
    let compiled = compile("---\ntype: note\n---\n# Heading");
    assert!(compiled.html.contains("<h1 id=\"heading\">Heading</h1>"));
    assert!(!compiled.html.contains("type: note"));
}

#[test]
fn compile_extracts_a_mermaid_block_leaving_a_placeholder() {
    let compiled = compile("Before.\n\n```mermaid\ngraph TD\nA-->B\n```\n\nAfter.");
    assert_eq!(compiled.mermaid_sources, vec!["graph TD\nA-->B".to_owned()]);
    assert!(compiled.html.contains(&mermaid_placeholder(0)));
    assert!(!compiled.html.contains("graph TD"));
}

#[test]
fn compile_does_not_mistake_a_documentation_fence_for_mermaid() {
    let doc = "```text\nExample: ```mermaid blocks render diagrams.\n```\n";
    let compiled = compile(doc);
    assert!(compiled.mermaid_sources.is_empty());
    assert!(compiled.html.contains("```mermaid blocks"));
}

#[test]
fn compile_extracts_a_wikilink_placeholder() {
    let compiled = compile("See [[target-note]].");
    assert_eq!(compiled.occurrences.len(), 1);
    assert_eq!(compiled.occurrences[0].syntax, LinkSyntax::Link);
    assert!(compiled.html.contains(&wikilinks::placeholder(0)));
}

#[test]
fn compile_extracts_an_embed_placeholder() {
    let compiled = compile("before\n\n![[embedded-note]]\n\nafter");
    assert_eq!(compiled.occurrences.len(), 1);
    assert_eq!(compiled.occurrences[0].syntax, LinkSyntax::Embed);
    assert!(compiled.html.contains(&wikilinks::embed_placeholder(0)));
}

#[test]
fn compile_renders_a_recognised_triple_colon_fence() {
    let compiled = compile(":::warning\nThis may break things.\n:::\n");
    assert!(compiled.html.contains("fence-recognised"));
    assert!(compiled.html.contains("This may break things."));
}

#[test]
fn compile_renders_an_unrecognised_fence_with_its_name_as_label() {
    let compiled = compile(":::mystery-fence\nBody.\n:::\n");
    assert!(compiled.html.contains("fence-unrecognised"));
    assert!(compiled.html.contains("mystery-fence"));
}

#[test]
fn compile_renders_a_callout() {
    let compiled = compile("> [!warning] Breaking change\n> Details here.\n");
    assert!(compiled.html.contains("callout-warning"));
    assert!(compiled.html.contains("Details here."));
}

#[test]
fn a_wikilink_inside_a_fence_body_is_still_extracted_at_the_top_level() {
    let compiled = compile(":::note\nSee [[inner-target]].\n:::\n");
    assert_eq!(compiled.occurrences.len(), 1);
    assert_eq!(compiled.occurrences[0].target, "inner-target");
    // The fence's own rendered HTML carries the link placeholder, ready for
    // the async resolution pass to substitute.
    assert!(compiled.html.contains(&wikilinks::placeholder(0)));
}

#[test]
fn compile_strips_a_comment_before_any_other_stage_sees_it() {
    let compiled = compile("Visible %%hidden [[not-a-real-link]]%% text.");
    assert!(compiled.html.contains("Visible"));
    assert!(compiled.html.contains("text."));
    assert!(!compiled.html.contains("hidden"));
    assert!(
        compiled.occurrences.is_empty(),
        "a wikilink inside a comment must not be extracted"
    );
}

#[test]
fn compile_renders_a_highlight_run() {
    let compiled = compile("This is ==important==.");
    assert!(compiled.html.contains("<mark>important</mark>"));
}

#[test]
fn compile_renders_an_inline_tag() {
    let compiled = compile("Tagged #project.");
    assert!(compiled.html.contains("<span class=\"tag\">#project</span>"));
}

#[test]
fn compile_renders_a_fence_nested_inside_another_fence_as_its_own_container() {
    // The convention document's own canonical `:::grid` > `:::card` example.
    let doc = ":::grid{cols=3}\n\n:::card{title=\"Build\"}\nCompile.\n:::\n\n:::card{title=\"Test\"}\nRun tests.\n:::\n\n:::\n";
    let compiled = compile(doc);
    assert!(compiled.html.contains("fence-grid"));
    // Both cards render as their own real, typed fence containers, not
    // literal `:::card` text.
    assert!(compiled.html.contains("fence-card"));
    assert!(compiled.html.matches("fence-card").count() >= 2);
    assert!(!compiled.html.contains(":::card"));
    assert!(compiled.html.contains("Compile."));
    assert!(compiled.html.contains("Run tests."));
}

#[test]
fn compile_renders_a_callout_nested_inside_another_callout_as_its_own_container() {
    // CALLOUTS.md's own documented "Nested Callouts" example.
    let doc = "> [!question] Outer callout\n> > [!note] Inner callout\n> > Nested content\n";
    let compiled = compile(doc);
    assert!(compiled.html.contains("callout-question"));
    assert!(compiled.html.contains("callout-note"));
    assert!(compiled.html.contains("Nested content"));
    assert!(
        !compiled.html.contains("[!note]"),
        "the inner marker must not leak as literal text"
    );
}

#[test]
fn html_from_commonmark_disambiguates_duplicate_heading_slugs() {
    let html = html_from_commonmark("# Overview\n\n## Overview\n");
    assert!(html.contains("<h1 id=\"overview\">"));
    assert!(html.contains("<h2 id=\"overview-1\">"));
}

#[test]
fn html_from_commonmark_slugifies_punctuation_in_heading_text() {
    let html = html_from_commonmark("# What's New? (v2)\n");
    assert!(html.contains("id=\"what-s-new-v2\""));
}

#[test]
fn slugify_strips_punctuation_and_collapses_whitespace() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
}

#[test]
fn append_fragment_uses_a_slugified_heading() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: "note".to_owned(),
        heading: Some("My Heading".to_owned()),
        block: None,
        display: None,
    };
    assert_eq!(
        append_fragment("/vault/note.md", &occurrence),
        "/vault/note.md#my-heading"
    );
}

#[test]
fn append_fragment_uses_a_verbatim_block_id() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: "note".to_owned(),
        heading: None,
        block: Some("abc123".to_owned()),
        display: None,
    };
    assert_eq!(append_fragment("/vault/note.md", &occurrence), "/vault/note.md#abc123");
}

#[test]
fn append_fragment_with_neither_leaves_the_href_unchanged() {
    let occurrence = LinkOccurrence {
        syntax: LinkSyntax::Link,
        target: "note".to_owned(),
        heading: None,
        block: None,
        display: None,
    };
    assert_eq!(append_fragment("/vault/note.md", &occurrence), "/vault/note.md");
}

#[test]
fn compile_renders_a_fence_nested_inside_a_callout() {
    let doc = "> [!note]\n> :::warning\n> A nested fence inside a callout.\n> :::\n";
    let compiled = compile(doc);
    assert!(compiled.html.contains("callout-note"));
    assert!(compiled.html.contains("fence-warning"));
    assert!(compiled.html.contains("A nested fence inside a callout."));
}

#[test]
fn a_wikilink_inside_a_callout_body_is_still_extracted_at_the_top_level() {
    let compiled = compile("> [!note]\n> See [[inner-target]].\n");
    assert_eq!(compiled.occurrences.len(), 1);
    assert_eq!(compiled.occurrences[0].target, "inner-target");
}
