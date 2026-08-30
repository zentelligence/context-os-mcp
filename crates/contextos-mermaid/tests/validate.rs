use contextos_mermaid::{MermanParser, ParsesMermaid};

#[test]
fn fr_70_validate_accepts_a_well_formed_flowchart() {
    let parser = MermanParser::new();
    let diagnostics = parser.validate("flowchart TD\n  A[Start] --> B[End]\n");

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn fr_70_validate_rejects_a_dangling_flowchart_edge() {
    let parser = MermanParser::new();
    let diagnostics = parser.validate("flowchart TD\n  A[Start] -->\n");

    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, "mermaid/diagram-parse");
    assert!(!diagnostics[0].message.is_empty());
}

#[test]
fn fr_70_validate_rejects_oversized_source_before_parsing() {
    let parser = MermanParser::new();
    let oversized = "x".repeat(2 * 1024 * 1024 + 1);

    let diagnostics = parser.validate(&oversized);

    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, "mermaid/resource-limit");
}
