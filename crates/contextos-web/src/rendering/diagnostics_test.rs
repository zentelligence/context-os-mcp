use super::*;

#[test]
fn renders_every_diagnostic_naming_its_code_path_and_message() {
    let diagnostics = vec![Diagnostic {
        code: "format/base-schema".to_owned(),
        path: "$.filters".to_owned(),
        message: "unexpected token".to_owned(),
    }];
    let html = render_diagnostic_panel(&diagnostics);
    assert!(html.contains("format/base-schema"));
    assert!(html.contains("$.filters"));
    assert!(html.contains("unexpected token"));
    assert!(html.contains("diagnostic-panel"));
}

#[test]
fn an_empty_diagnostic_list_still_renders_a_panel_shell() {
    let html = render_diagnostic_panel(&[]);
    assert!(html.contains("diagnostic-panel"));
}

#[test]
fn escapes_html_special_characters_in_a_diagnostic_message() {
    let diagnostics = vec![Diagnostic {
        code: "format/frontmatter".to_owned(),
        path: "note.md".to_owned(),
        message: "<script>alert(1)</script>".to_owned(),
    }];
    let html = render_diagnostic_panel(&diagnostics);
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&#60;script"));
}
