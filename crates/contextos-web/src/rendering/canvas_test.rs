use super::*;

fn text_node(id: &str, x: f64, y: f64, text: &str) -> CanvasNode {
    CanvasNode::Text {
        id: id.to_owned(),
        x,
        y,
        width: 160.0,
        height: 70.0,
        text: text.to_owned(),
        color: None,
    }
}

#[test]
fn renders_a_text_node_at_its_given_position_with_no_auto_layout() {
    let nodes = vec![text_node("a1", 40.0, 50.0, "Hello **world**")];
    let svg = render_svg(&nodes, &[], "example-vault");
    assert!(svg.contains("data-id=\"a1\""));
    assert!(svg.contains("x=\"40\""));
    assert!(svg.contains("y=\"50\""));
    assert!(svg.contains("<strong>world</strong>"));
}

#[test]
fn renders_a_file_node_as_a_link_into_the_vault_route() {
    let nodes = vec![CanvasNode::File {
        id: "f1".to_owned(),
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 50.0,
        file: "notes/example.md".to_owned(),
        subpath: None,
        color: None,
    }];
    let svg = render_svg(&nodes, &[], "example-vault");
    assert!(svg.contains("href=\"/example-vault/notes/example.md\""));
}

#[test]
fn renders_a_link_node_as_an_external_preview_never_fetching_it() {
    let nodes = vec![CanvasNode::Link {
        id: "l1".to_owned(),
        x: 0.0,
        y: 0.0,
        width: 100.0,
        height: 50.0,
        url: "https://jsoncanvas.org/spec/1.0/".to_owned(),
        color: None,
    }];
    let svg = render_svg(&nodes, &[], "example-vault");
    assert!(svg.contains("https://jsoncanvas.org/spec/1.0/"));
    assert!(svg.contains("target=\"_blank\""));
}

#[test]
fn renders_a_group_node_beneath_its_members_with_its_label() {
    let nodes = vec![CanvasNode::Group {
        id: "g1".to_owned(),
        x: 0.0,
        y: 0.0,
        width: 200.0,
        height: 100.0,
        label: Some("Group label".to_owned()),
        background: None,
        background_style: None,
        color: None,
    }];
    let svg = render_svg(&nodes, &[], "example-vault");
    assert!(svg.contains("canvas-group-label"));
    assert!(svg.contains("Group label"));
}

#[test]
fn renders_an_edge_between_two_node_anchor_points() {
    let nodes = vec![
        text_node("a", 0.0, 0.0, "A"),
        CanvasNode::Text {
            id: "b".to_owned(),
            x: 300.0,
            y: 0.0,
            width: 160.0,
            height: 70.0,
            text: "B".to_owned(),
            color: None,
        },
    ];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: Some("right".to_owned()),
        from_end: None,
        to_node: "b".to_owned(),
        to_side: Some("left".to_owned()),
        to_end: Some("arrow".to_owned()),
        color: None,
        label: Some("edge label".to_owned()),
    }];
    let svg = render_svg(&nodes, &edges, "example-vault");
    assert!(svg.contains("canvas-edge"));
    assert!(svg.contains("marker-end"));
    assert!(svg.contains("edge label"));
}

#[test]
fn an_edge_with_to_end_none_carries_no_arrowhead_marker() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A"), text_node("b", 300.0, 0.0, "B")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "b".to_owned(),
        to_side: None,
        to_end: Some("none".to_owned()),
        color: None,
        label: None,
    }];
    let svg = render_svg(&nodes, &edges, "example-vault");
    assert!(!svg.contains("marker-end"));
}

#[test]
fn an_edge_referencing_a_missing_node_is_skipped_not_a_panic() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "does-not-exist".to_owned(),
        to_side: None,
        to_end: None,
        color: None,
        label: None,
    }];
    let svg = render_svg(&nodes, &edges, "example-vault");
    assert!(!svg.contains("canvas-edge"));
}

#[test]
fn rendering_is_deterministic_across_repeated_calls() {
    let nodes = vec![text_node("a", 0.0, 0.0, "A"), text_node("b", 300.0, 0.0, "B")];
    let edges = vec![CanvasEdge {
        id: "e1".to_owned(),
        from_node: "a".to_owned(),
        from_side: None,
        from_end: None,
        to_node: "b".to_owned(),
        to_side: None,
        to_end: None,
        color: None,
        label: None,
    }];
    let first = render_svg(&nodes, &edges, "example-vault");
    let second = render_svg(&nodes, &edges, "example-vault");
    assert_eq!(first, second);
}

#[test]
fn a_preset_colour_resolves_to_a_theme_hex_value() {
    assert_eq!(resolve_color(Some("1"), "#000"), "#a13a3a");
}

#[test]
fn a_literal_hex_colour_passes_through_unchanged() {
    assert_eq!(resolve_color(Some("#123abc"), "#000"), "#123abc");
}

#[test]
fn no_colour_uses_the_fallback() {
    assert_eq!(resolve_color(None, "#fallback"), "#fallback");
}
