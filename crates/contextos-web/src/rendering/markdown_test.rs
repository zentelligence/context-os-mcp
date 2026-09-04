use super::*;

#[test]
fn html_from_commonmark_renders_basic_markdown() {
    let html = html_from_commonmark("# Heading\n\nBody **text**.");
    assert!(html.contains("<h1>Heading</h1>"));
    assert!(html.contains("<strong>text</strong>"));
}

#[test]
fn html_from_commonmark_supports_gfm_tables() {
    let html = html_from_commonmark("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
    assert!(html.contains("<table>"));
}

#[test]
fn compile_strips_frontmatter_before_rendering() {
    let compiled = compile("---\ntype: note\n---\n# Heading");
    assert!(compiled.html.contains("<h1>Heading</h1>"));
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
fn a_wikilink_inside_a_callout_body_is_still_extracted_at_the_top_level() {
    let compiled = compile("> [!note]\n> See [[inner-target]].\n");
    assert_eq!(compiled.occurrences.len(), 1);
    assert_eq!(compiled.occurrences[0].target, "inner-target");
}
