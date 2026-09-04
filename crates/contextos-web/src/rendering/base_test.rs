use serde_json::json;

use super::*;

fn map(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[test]
fn a_row_with_a_file_path_column_links_to_its_own_vault_route() {
    let row = map(&json!({"file.path": "tasks/one.md", "status": "active"}));
    let columns = vec!["file.path".to_owned(), "status".to_owned()];
    let view = row_view(&row, &columns, "example-vault");
    assert!(view.has_link);
    assert_eq!(view.href, "/example-vault/tasks/one.md");
    assert_eq!(view.note_path.as_deref(), Some("tasks/one.md"));
    assert_eq!(view.title, "one.md");
    assert_eq!(view.columns.len(), 1);
    assert_eq!(view.columns[0].name, "status");
    assert_eq!(view.columns[0].value, "active");
}

#[test]
fn a_row_with_no_file_path_column_renders_without_a_link() {
    let row = map(&json!({"status": "active"}));
    let columns = vec!["status".to_owned()];
    let view = row_view(&row, &columns, "example-vault");
    assert!(!view.has_link);
    assert_eq!(view.href, "");
    assert_eq!(view.note_path, None);
    assert_eq!(view.title, "active");
}

#[test]
fn stringify_renders_every_json_value_kind() {
    assert_eq!(stringify(&json!("text")), "text");
    assert_eq!(stringify(&json!(42)), "42");
    assert_eq!(stringify(&json!(true)), "true");
    assert_eq!(stringify(&json!(null)), "");
    assert_eq!(stringify(&json!(["a", "b"])), "a, b");
}
