use contextos_mermaid::{MermanParser, RendersMermaid};

#[test]
fn render_produces_svg_for_a_well_formed_flowchart() -> Result<(), Box<dyn std::error::Error>> {
    let parser = MermanParser::new();
    let svg = parser
        .render("flowchart TD\n  A[Start] --> B[End]\n")
        .map_err(|diagnostics| format!("{diagnostics:?}"))?;
    let svg = String::from_utf8(svg)?;

    assert!(svg.trim_start().starts_with("<svg"), "{svg}");
    assert!(!svg.contains("<script"), "{svg}");
    assert!(!svg.contains("<foreignObject"), "{svg}");
    Ok(())
}

#[test]
fn render_returns_diagnostics_instead_of_an_svg_for_invalid_source() {
    let parser = MermanParser::new();
    let result = parser.render("flowchart TD\n  A[Start] -->\n");

    assert!(
        matches!(&result, Err(diagnostics) if diagnostics.len() == 1),
        "{result:?}"
    );
    if let Err(diagnostics) = result {
        assert_eq!(diagnostics[0].code, "mermaid/diagram-parse");
    }
}

#[test]
fn render_ignores_a_diagram_directive_that_tries_to_re_enable_html_labels() -> Result<(), Box<dyn std::error::Error>> {
    let parser = MermanParser::new();
    let source = "%%{init: {\"flowchart\": {\"htmlLabels\": true}}}%%\nflowchart TD\n  A[Start] --> B[End]\n";

    let svg = parser
        .render(source)
        .map_err(|diagnostics| format!("{diagnostics:?}"))?;
    let svg = String::from_utf8(svg)?;

    assert!(!svg.contains("<foreignObject"), "{svg}");
    Ok(())
}

#[test]
fn render_rejects_oversized_source_before_parsing_or_layout() {
    let parser = MermanParser::new();
    let oversized = "x".repeat(2 * 1024 * 1024 + 1);

    let result = parser.render(&oversized);

    assert!(
        matches!(&result, Err(diagnostics) if diagnostics.len() == 1),
        "{result:?}"
    );
    if let Err(diagnostics) = result {
        assert_eq!(diagnostics[0].code, "mermaid/resource-limit");
    }
}

#[test]
fn render_is_deterministic_across_repeated_calls() -> Result<(), Box<dyn std::error::Error>> {
    let parser = MermanParser::new();
    let source = "flowchart TD\n  A[Start] --> B{Decision}\n  B -->|Yes| C[End]\n  B -->|No| A\n";

    let first = parser
        .render(source)
        .map_err(|diagnostics| format!("{diagnostics:?}"))?;
    let second = parser
        .render(source)
        .map_err(|diagnostics| format!("{diagnostics:?}"))?;

    assert_eq!(first, second);
    Ok(())
}
