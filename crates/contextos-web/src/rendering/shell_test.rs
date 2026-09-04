use super::*;

#[test]
fn settings_breadcrumb_has_no_vault_segment() {
    let segments = settings_breadcrumb();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].label, "settings");
    assert_eq!(segments[0].href, None);
}

#[test]
fn breadcrumb_segments_for_a_vault_root_is_a_single_unlinked_segment() {
    let segments = breadcrumb_segments("vault", "");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].label, "vault");
    assert_eq!(segments[0].href, None);
}

#[test]
fn breadcrumb_segments_for_a_nested_path_links_every_ancestor_but_the_last() {
    let segments = breadcrumb_segments("vault", "docs/guides/note.md");
    let labels: Vec<&str> = segments.iter().map(|segment| segment.label.as_str()).collect();
    assert_eq!(labels, ["vault", "docs", "guides", "note.md"]);
    let hrefs: Vec<Option<&str>> = segments.iter().map(|segment| segment.href.as_deref()).collect();
    assert_eq!(
        hrefs,
        [Some("/vault/"), Some("/vault/docs/"), Some("/vault/docs/guides/"), None,]
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
