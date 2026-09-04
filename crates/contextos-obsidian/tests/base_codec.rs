use contextos_obsidian::{BaseDocument, BaseOperation};
use serde_json::{Map, json};

const EVERY_FEATURE: &str = include_str!("../../../fixtures/obsidian-formats/bases/every-feature.base");

#[test]
fn base_round_trip_preserves_every_schema_feature_and_expression_value() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = BaseDocument::try_from(EVERY_FEATURE)?;

    assert!(document.diagnostics().is_empty());
    document.apply(vec![BaseOperation::AddFormula {
        name: "torture".to_owned(),
        expression: r#"if(title == "O'Reilly: #1", "[ready]", "a,b {c}")"#.to_owned(),
    }])?;
    let rendered = String::try_from(&document)?;
    let reparsed = BaseDocument::try_from(rendered.as_str())?;

    assert!(reparsed.diagnostics().is_empty());
    assert_eq!(reparsed.definition(), document.definition());
    assert_eq!(
        reparsed
            .definition()
            .get("formulas")
            .and_then(serde_json::Value::as_object)
            .and_then(|formulas| formulas.get("torture")),
        Some(&serde_json::json!(
            r#"if(title == "O'Reilly: #1", "[ready]", "a,b {c}")"#
        ))
    );
    Ok(())
}

#[test]
fn base_apply_is_transactional_when_a_formula_removal_would_dangle_references() -> Result<(), Box<dyn std::error::Error>>
{
    let mut document = BaseDocument::try_from(EVERY_FEATURE)?;
    let original = document.definition().clone();

    let result = document.apply(vec![
        BaseOperation::SetSummary {
            name: "temporary".to_owned(),
            expression: "values.length".to_owned(),
        },
        BaseOperation::RemoveFormula {
            name: "display_status".to_owned(),
        },
    ]);

    assert!(result.is_err());
    assert_eq!(document.definition(), &original);
    Ok(())
}

#[test]
fn every_base_operation_composes_in_one_valid_transaction() -> Result<(), Box<dyn std::error::Error>> {
    let mut initial = Map::new();
    initial.insert(
        "views".to_owned(),
        json!([{"type": "table", "name": "Permanent", "order": ["file.name"]}]),
    );
    let mut document = BaseDocument::try_from(initial)?;
    document.apply(vec![
        BaseOperation::SetFilters {
            filters: json!({"and": ["file.ext == \"md\"", "status != 'archived'"]}),
        },
        BaseOperation::AddFormula {
            name: "temporary".to_owned(),
            expression: "price * quantity".to_owned(),
        },
        BaseOperation::SetProperty {
            name: "status".to_owned(),
            definition: serde_json::from_value(json!({"displayName": "Current status"}))?,
        },
        BaseOperation::AddView {
            name: "Temporary view".to_owned(),
            definition: serde_json::from_value(json!({
                "type": "table",
                "order": ["file.name", "status"]
            }))?,
        },
        BaseOperation::UpdateView {
            name: "Temporary view".to_owned(),
            patch: serde_json::from_value(json!({
                "limit": 12,
                "sort": [{"property": "file.name", "direction": "ASC"}]
            }))?,
        },
        BaseOperation::SetSummary {
            name: "precise_total".to_owned(),
            expression: "values.sum().round(2)".to_owned(),
        },
        BaseOperation::RemoveView {
            name: "Temporary view".to_owned(),
        },
        BaseOperation::RemoveFormula {
            name: "temporary".to_owned(),
        },
    ])?;

    assert!(document.diagnostics().is_empty());
    assert_eq!(document.definition().get("formulas"), Some(&json!({})));
    assert_eq!(
        document.definition().get("views"),
        Some(&json!([{
            "type": "table", "name": "Permanent", "order": ["file.name"]
        }]))
    );
    let definition = serde_json::Value::Object(document.definition().clone());
    assert_eq!(
        definition.pointer("/properties/status/displayName"),
        Some(&json!("Current status"))
    );
    assert_eq!(
        definition.pointer("/summaries/precise_total"),
        Some(&json!("values.sum().round(2)"))
    );
    let reparsed = BaseDocument::try_from(String::try_from(&document)?.as_str())?;
    assert_eq!(reparsed.definition(), document.definition());
    Ok(())
}

#[test]
fn bracket_quoted_formula_references_are_validated_transactionally() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = BaseDocument::try_from(
        r#"
formulas:
  total cost: 'price * quantity'
views:
  - type: table
    name: Costed
    filters: 'formula["total cost"] > 0'
"#,
    )?;
    assert!(document.diagnostics().is_empty());
    let before = document.clone();

    let result = document.apply(vec![BaseOperation::RemoveFormula {
        name: "total cost".to_owned(),
    }]);

    assert!(result.is_err());
    assert_eq!(document, before);
    Ok(())
}

#[test]
fn remove_property_and_remove_summary_remove_the_named_entry_and_leave_others_untouched()
-> Result<(), Box<dyn std::error::Error>> {
    let mut initial = Map::new();
    initial.insert(
        "properties".to_owned(),
        json!({
            "status": {"displayName": "Current status"},
            "priority": {"displayName": "Priority"}
        }),
    );
    initial.insert(
        "summaries".to_owned(),
        json!({
            "counted": "values.length",
            "totalled": "values.sum()"
        }),
    );
    initial.insert(
        "views".to_owned(),
        json!([{"type": "table", "name": "All", "order": ["file.name"]}]),
    );
    let mut document = BaseDocument::try_from(initial)?;

    document.apply(vec![
        BaseOperation::RemoveProperty {
            name: "status".to_owned(),
        },
        BaseOperation::RemoveSummary {
            name: "counted".to_owned(),
        },
    ])?;

    assert!(document.diagnostics().is_empty());
    assert_eq!(
        document.definition().get("properties"),
        Some(&json!({"priority": {"displayName": "Priority"}}))
    );
    assert_eq!(
        document.definition().get("summaries"),
        Some(&json!({"totalled": "values.sum()"}))
    );
    Ok(())
}

#[test]
fn remove_property_and_remove_summary_reject_a_name_that_does_not_exist() -> Result<(), Box<dyn std::error::Error>> {
    let mut initial = Map::new();
    initial.insert(
        "views".to_owned(),
        json!([{"type": "table", "name": "All", "order": ["file.name"]}]),
    );
    let mut document = BaseDocument::try_from(initial)?;
    let before = document.clone();

    let missing_property = document.apply(vec![BaseOperation::RemoveProperty {
        name: "missing".to_owned(),
    }]);
    assert!(missing_property.is_err());
    assert_eq!(document, before);

    let missing_summary = document.apply(vec![BaseOperation::RemoveSummary {
        name: "missing".to_owned(),
    }]);
    assert!(missing_summary.is_err());
    assert_eq!(document, before);
    Ok(())
}

#[test]
fn base_validation_rejects_unknown_sections_missing_views_and_unknown_view_types()
-> Result<(), Box<dyn std::error::Error>> {
    let missing_views = BaseDocument::try_from("unexpected: true\n")?;
    let invalid_view = BaseDocument::try_from("views:\n  - type: timeline\n    name: Unsupported\n")?;

    assert!(
        missing_views
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.path == "unexpected")
    );
    assert!(
        missing_views
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.path == "views")
    );
    assert!(
        invalid_view
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.path == "views[0].type")
    );
    Ok(())
}

#[test]
fn formula_reference_validation_ignores_human_facing_labels() -> Result<(), Box<dyn std::error::Error>> {
    let document = BaseDocument::try_from(
        r#"
properties:
  status:
    displayName: "Explain formula.missing literally"
views:
  - type: table
    name: "formula.also_missing is just a label"
    order: [file.name, status]
"#,
    )?;

    assert!(document.diagnostics().is_empty());
    Ok(())
}

#[test]
fn base_validation_reaches_every_documented_schema_error_class() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        ("filters: {xor: []}\nviews: [{type: table, name: All}]\n", "filters"),
        (
            "formulas: {broken: 4}\nviews: [{type: table, name: All}]\n",
            "formulas.broken",
        ),
        (
            "properties: {status: {displayName: 4}}\nviews: [{type: table, name: All}]\n",
            "properties.status.displayName",
        ),
        (
            "views: [{type: table, name: Same}, {type: list, name: Same}]\n",
            "views[1].name",
        ),
        ("views: [{type: table, name: All, limit: 0}]\n", "views[0].limit"),
        (
            "views: [{type: table, name: All, groupBy: {property: 4, direction: DOWN}}]\n",
            "views[0].groupBy.property",
        ),
        ("views: [{type: table, name: All, sort: wrong}]\n", "views[0].sort"),
        (
            "views: [{type: table, name: All, summaries: {price: Missing}}]\n",
            "views[0].summaries.price",
        ),
    ];

    for (source, expected_path) in cases {
        let document = BaseDocument::try_from(source)?;
        assert!(
            document
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.path == expected_path),
            "missing diagnostic at {expected_path} for {source}"
        );
    }
    Ok(())
}
