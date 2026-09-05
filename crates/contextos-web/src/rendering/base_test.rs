use std::collections::HashMap;

use serde_json::json;

use super::*;

fn map(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn no_display_names() -> HashMap<String, String> {
    HashMap::new()
}

#[test]
fn a_row_with_a_file_path_column_links_to_its_own_vault_route() {
    let row = map(&json!({"file.path": "tasks/one.md", "status": "active"}));
    let columns = vec!["file.path".to_owned(), "status".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert!(view.has_link);
    assert_eq!(view.href, "/example-vault/tasks/one.md");
    assert_eq!(view.note_path.as_deref(), Some("tasks/one.md"));
    assert_eq!(view.title, "one");
    assert_eq!(view.columns.len(), 1);
    assert_eq!(view.columns[0].name, "status");
    assert_eq!(view.columns[0].value, "active");
}

#[test]
fn a_row_with_no_file_path_column_and_no_link_column_renders_untitled_without_a_link() {
    let row = map(&json!({"status": "active"}));
    let columns = vec!["status".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert!(!view.has_link);
    assert_eq!(view.href, "");
    assert_eq!(view.note_path, None);
    // The title is never guessed from an arbitrary column's own value
    // (that was the source of a real bug: a Link-shaped column's raw JSON
    // rendered as the title); with no file identity available at all, it
    // degrades honestly to "(untitled)" instead.
    assert_eq!(view.title, "(untitled)");
}

#[test]
fn a_row_with_no_file_path_column_still_links_via_any_link_columns_own_target() {
    // The real bug this guards: a view whose own `order` never includes
    // `file.path` (a link-typed formula column is enough on its own) must
    // still produce a working card link and title, regardless of where
    // that link column sits in `order`.
    let row = map(&json!({
        "entity": "altorum",
        "formula.id_link": { "type": "link", "target": "tasks/a-002.md", "display": "A-002" },
    }));
    let columns = vec!["formula.id_link".to_owned(), "entity".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert!(view.has_link);
    assert_eq!(view.href, "/example-vault/tasks/a-002.md");
    assert_eq!(view.note_path.as_deref(), Some("tasks/a-002.md"));
    assert_eq!(view.title, "a-002");
}

#[test]
fn a_columns_own_display_name_is_used_as_its_label_but_never_its_data_field() {
    let row = map(&json!({"status": "active"}));
    let columns = vec!["status".to_owned()];
    let mut display_names = no_display_names();
    display_names.insert("status".to_owned(), "Status".to_owned());
    let view = row_view(&row, &columns, "example-vault", &display_names);
    assert_eq!(view.columns[0].label, "Status");
    assert_eq!(view.columns[0].name, "status");
}

#[test]
fn a_column_with_no_display_name_falls_back_to_its_raw_name_as_its_label() {
    let row = map(&json!({"status": "active"}));
    let columns = vec!["status".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert_eq!(view.columns[0].label, "status");
}

#[test]
fn stringify_renders_every_json_value_kind() {
    assert_eq!(stringify(&json!("text")), "text");
    assert_eq!(stringify(&json!(42)), "42");
    assert_eq!(stringify(&json!(true)), "true");
    assert_eq!(stringify(&json!(null)), "");
    assert_eq!(stringify(&json!(["a", "b"])), "a, b");
}

#[test]
fn a_formula_link_column_renders_as_an_anchor_to_the_vault_route_and_is_excluded_from_editing() {
    let row = map(&json!({
        "file.path": "tasks/one.md",
        "formula.id_link": { "type": "link", "target": "tasks/one.md", "display": "a-001" },
    }));
    let columns = vec!["file.path".to_owned(), "formula.id_link".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert_eq!(view.columns.len(), 1);
    let link_column = &view.columns[0];
    assert_eq!(link_column.name, "formula.id_link");
    assert_eq!(link_column.value, "a-001");
    assert_eq!(link_column.href.as_deref(), Some("/example-vault/tasks/one.md"));
    assert!(link_column.is_formula);
}

#[test]
fn an_ordinary_column_has_no_href_and_is_not_marked_a_formula() {
    let row = map(&json!({"status": "active"}));
    let columns = vec!["status".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert_eq!(view.columns[0].href, None);
    assert!(!view.columns[0].is_formula);
}

#[test]
fn an_unevaluated_formula_marker_renders_as_plain_text_not_a_link() {
    let row = map(&json!({"formula.days_old": "formula.days_old (not evaluated)"}));
    let columns = vec!["formula.days_old".to_owned()];
    let view = row_view(&row, &columns, "example-vault", &no_display_names());
    assert_eq!(view.columns[0].value, "formula.days_old (not evaluated)");
    assert_eq!(view.columns[0].href, None);
    assert!(view.columns[0].is_formula);
}
