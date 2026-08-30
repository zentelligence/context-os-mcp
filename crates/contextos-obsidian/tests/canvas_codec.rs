use contextos_obsidian::{CanvasDocument, CanvasOperation};
use serde_json::{Map, json};

const EVERY_FEATURE: &str =
    include_str!("../../../fixtures/obsidian-formats/canvases/every-feature.canvas");
const GROUP_NESTING: &str =
    include_str!("../../../fixtures/obsidian-formats/canvases/group-nesting.canvas");
const DANGLING_EDGE: &str =
    include_str!("../../../fixtures/obsidian-formats/canvases/dangling-edge.canvas");

#[test]
fn fr_45_json_canvas_1_round_trip_covers_every_node_edge_and_nested_group_feature()
-> Result<(), Box<dyn std::error::Error>> {
    for source in [EVERY_FEATURE, GROUP_NESTING] {
        let document = CanvasDocument::try_from(source)?;
        assert!(document.diagnostics().is_empty());

        let rendered = String::try_from(&document)?;
        let reparsed = CanvasDocument::try_from(rendered.as_str())?;

        assert_eq!(reparsed.nodes(), document.nodes());
        assert_eq!(reparsed.edges(), document.edges());
    }
    Ok(())
}

#[test]
fn fr_46_canvas_validation_reports_a_dangling_edge_at_the_specific_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let document = CanvasDocument::try_from(DANGLING_EDGE)?;

    assert_eq!(document.diagnostics().len(), 1);
    assert_eq!(document.diagnostics()[0].code, "canvas/dangling-edge");
    assert_eq!(document.diagnostics()[0].path, "edges[0].toNode");
    Ok(())
}

#[test]
fn fr_45_canvas_apply_auto_positions_nodes_groups_members_and_rolls_back_invalid_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let mut document = CanvasDocument::try_from(EVERY_FEATURE)?;
    let mut node = Map::new();
    node.insert("type".to_owned(), json!("text"));
    node.insert("width".to_owned(), json!(200));
    node.insert("height".to_owned(), json!(100));
    node.insert("text".to_owned(), json!("Auto-positioned"));
    document.apply(vec![CanvasOperation::AddNode { node }])?;
    let added = document
        .nodes()
        .last()
        .and_then(serde_json::Value::as_object)
        .ok_or("added node missing")?;
    let added_id = added
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or("generated node ID missing")?
        .to_owned();
    assert_eq!(added_id.len(), 16);
    assert!(
        added_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert_eq!(added.get("y"), Some(&json!(0)));

    document.apply(vec![CanvasOperation::AddNode {
        node: serde_json::from_value(json!({
            "type": "text", "text": "Partially positioned", "x": 10,
            "width": 180, "height": 90
        }))?,
    }])?;
    let partially_positioned = document
        .nodes()
        .last()
        .and_then(serde_json::Value::as_object)
        .ok_or("partially positioned node missing")?;
    assert_eq!(partially_positioned.get("x"), Some(&json!(10)));
    assert_eq!(partially_positioned.get("y"), Some(&json!(0)));

    let mut group = Map::new();
    group.insert("type".to_owned(), json!("group"));
    group.insert("label".to_owned(), json!("Generated group"));
    document.apply(vec![CanvasOperation::Group {
        group,
        members: vec!["1000000000000002".to_owned(), added_id],
    }])?;
    let generated_group = document
        .nodes()
        .iter()
        .find_map(|node| {
            node.as_object()
                .filter(|node| node.get("label") == Some(&json!("Generated group")))
        })
        .ok_or("generated group missing")?;
    assert_eq!(generated_group.get("x"), Some(&json!(-40)));
    assert!(
        generated_group
            .get("width")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|width| width > 280)
    );

    let before_failure = document.clone();
    let result = document.apply(vec![CanvasOperation::AddEdge {
        edge: serde_json::from_value(json!({
            "fromNode": "1000000000000002",
            "toNode": "absent"
        }))?,
    }]);
    assert!(result.is_err());
    assert_eq!(document, before_failure);
    Ok(())
}

#[test]
fn fr_45_every_canvas_operation_composes_and_node_removal_cascades_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let mut document = CanvasDocument::try_from(
        json!({
            "nodes": [
                {"id": "a", "type": "text", "text": "A", "x": 0, "y": 0, "width": 100, "height": 100},
                {"id": "b", "type": "text", "text": "B", "x": 200, "y": 0, "width": 100, "height": 100}
            ],
            "edges": [{"id": "a-b", "fromNode": "a", "toNode": "b"}]
        })
        .to_string()
        .as_str(),
    )?;
    document.apply(vec![
        CanvasOperation::AddNode {
            node: serde_json::from_value(json!({
                "id": "c", "type": "text", "text": "C", "x": 400, "y": 0,
                "width": 100, "height": 100
            }))?,
        },
        CanvasOperation::UpdateNode {
            id: "c".to_owned(),
            patch: serde_json::from_value(json!({"text": "Updated C", "color": "2"}))?,
        },
        CanvasOperation::AddEdge {
            edge: serde_json::from_value(json!({
                "id": "c-a", "fromNode": "c", "toNode": "a", "label": "before"
            }))?,
        },
        CanvasOperation::UpdateEdge {
            id: "c-a".to_owned(),
            patch: serde_json::from_value(json!({"label": "after", "toEnd": "arrow"}))?,
        },
        CanvasOperation::RemoveEdge {
            id: "c-a".to_owned(),
        },
        CanvasOperation::Group {
            group: serde_json::from_value(json!({
                "id": "a-c", "type": "group", "label": "A and C"
            }))?,
            members: vec!["a".to_owned(), "c".to_owned()],
        },
        CanvasOperation::RemoveNode { id: "b".to_owned() },
    ])?;

    assert!(document.diagnostics().is_empty());
    assert!(document.edges().is_empty());
    assert!(document.nodes().iter().any(|node| {
        node.pointer("/id") == Some(&json!("c"))
            && node.pointer("/text") == Some(&json!("Updated C"))
    }));
    assert_eq!(document.nodes()[0].pointer("/id"), Some(&json!("a-c")));
    let rendered = String::try_from(&document)?;
    let reparsed = CanvasDocument::try_from(rendered.as_str())?;
    assert_eq!(reparsed, document);
    Ok(())
}

#[test]
fn fr_46_json_canvas_validation_reaches_every_documented_schema_error_class()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            json!({
                "nodes": [{"id": "same", "type": "text", "text": "A", "x": 0, "y": 0, "width": 100, "height": 100}],
                "edges": [{"id": "same", "fromNode": "same", "toNode": "same"}]
            }),
            "edges[0].id",
        ),
        (
            json!({"nodes": [{"id": "n", "type": "text", "x": 0, "y": 0, "width": 100, "height": 100}]}),
            "nodes[0].text",
        ),
        (
            json!({"nodes": [{"id": "n", "type": "file", "x": 0, "y": 0, "width": 100, "height": 100}]}),
            "nodes[0].file",
        ),
        (
            json!({"nodes": [{"id": "n", "type": "link", "x": 0, "y": 0, "width": 100, "height": 100}]}),
            "nodes[0].url",
        ),
        (
            json!({"nodes": [{"id": "n", "type": "timeline", "x": 0, "y": 0, "width": 100, "height": 100}]}),
            "nodes[0].type",
        ),
        (
            json!({
                "nodes": [
                    {"id": "a", "type": "text", "text": "A", "x": 0, "y": 0, "width": 100, "height": 100},
                    {"id": "b", "type": "text", "text": "B", "x": 200, "y": 0, "width": 100, "height": 100}
                ],
                "edges": [{"id": "e", "fromNode": "a", "fromSide": "centre", "toNode": "b"}]
            }),
            "edges[0].fromSide",
        ),
        (
            json!({
                "nodes": [
                    {"id": "a", "type": "text", "text": "A", "x": 0, "y": 0, "width": 100, "height": 100},
                    {"id": "b", "type": "text", "text": "B", "x": 200, "y": 0, "width": 100, "height": 100}
                ],
                "edges": [{"id": "e", "fromNode": "a", "toNode": "b", "toEnd": "circle"}]
            }),
            "edges[0].toEnd",
        ),
        (
            json!({"nodes": [{"id": "n", "type": "text", "text": "A", "x": 0, "y": 0, "width": 100, "height": 100, "color": "7"}]}),
            "nodes[0].color",
        ),
    ];

    for (definition, expected_path) in cases {
        let source = definition.to_string();
        let document = CanvasDocument::try_from(source.as_str())?;
        assert!(
            document
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.path == expected_path),
            "missing diagnostic at {expected_path} for {definition}"
        );
    }
    Ok(())
}
