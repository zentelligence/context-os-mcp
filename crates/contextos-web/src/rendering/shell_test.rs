use super::*;

#[test]
fn breadcrumb_for_settings_has_no_vault_segment() {
    assert_eq!(breadcrumb_for(None, None), "settings");
}

#[test]
fn breadcrumb_for_a_vault_root_is_just_the_vault_name() {
    assert_eq!(breadcrumb_for(Some("vault"), Some("")), "vault");
    assert_eq!(breadcrumb_for(Some("vault"), None), "vault");
}

#[test]
fn breadcrumb_for_a_nested_path_joins_segments_with_a_separator() {
    assert_eq!(
        breadcrumb_for(Some("vault"), Some("docs/guides/note.md")),
        "vault / docs / guides / note.md"
    );
}

#[test]
fn directory_scope_of_a_directory_is_itself() {
    assert_eq!(directory_scope("docs/guides", true), "docs/guides");
    assert_eq!(directory_scope("", true), "");
}

#[test]
fn directory_scope_of_a_file_is_its_containing_directory() {
    assert_eq!(directory_scope("docs/guides/note.md", false), "docs/guides");
}

#[test]
fn directory_scope_of_a_root_level_file_is_the_vault_root() {
    assert_eq!(directory_scope("note.md", false), "");
}
