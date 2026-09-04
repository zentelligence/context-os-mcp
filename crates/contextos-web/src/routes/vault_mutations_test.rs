use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn md_mutation_body_parses_a_patch_with_no_expected_hash() -> TestResult {
    let parsed: MdMutationBody = serde_json::from_str(r#"{"patch": {"status": "active"}}"#)?;
    assert_eq!(
        parsed.patch.get("status").and_then(Value::as_str),
        Some("active")
    );
    assert_eq!(parsed.expected_hash, None);
    Ok(())
}

#[test]
fn base_mutation_body_row_target_requires_a_note_path_and_patch() -> TestResult {
    let parsed: BaseMutationBody = serde_json::from_str(
        r#"{"target": "row", "note_path": "tasks/one.md", "patch": {"status": "done"}}"#,
    )?;
    let BaseMutationBody::Row {
        note_path, patch, ..
    } = parsed
    else {
        return Err("expected a Row variant".into());
    };
    assert_eq!(note_path, "tasks/one.md");
    assert_eq!(patch.get("status").and_then(Value::as_str), Some("done"));
    Ok(())
}

#[test]
fn base_mutation_body_definition_target_requires_operations() -> TestResult {
    let parsed: BaseMutationBody = serde_json::from_str(
        r#"{"target": "definition", "operations": [{"op": "set_filters", "filter": "status == \"active\""}]}"#,
    )?;
    let BaseMutationBody::Definition { operations, .. } = parsed else {
        return Err("expected a Definition variant".into());
    };
    assert_eq!(operations.len(), 1);
    Ok(())
}

#[test]
fn base_mutation_body_rejects_an_unknown_target() {
    let result: Result<BaseMutationBody, _> =
        serde_json::from_str(r#"{"target": "mystery", "operations": []}"#);
    assert!(result.is_err());
}

#[test]
fn base_mutation_body_row_and_definition_are_never_conflated_by_shape() {
    // A row body's fields (`note_path`, `patch`) never satisfy a
    // Definition variant's required `operations` field, and vice versa:
    // this is what makes `FR-222`'s "never conflated" guarantee structural
    // rather than a runtime check.
    let row: Result<BaseMutationBody, _> =
        serde_json::from_str(r#"{"target": "definition", "note_path": "x.md", "patch": {}}"#);
    assert!(row.is_err());
}
