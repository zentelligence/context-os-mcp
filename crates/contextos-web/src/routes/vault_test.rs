use super::*;

#[test]
fn extension_of_returns_the_lowercase_extension() {
    assert_eq!(extension_of("notes/example.md"), "md");
    assert_eq!(extension_of("registry/apps/tasks.base"), "base");
}

#[test]
fn extension_of_returns_empty_for_a_path_with_no_extension() {
    assert_eq!(extension_of("notes/example"), "");
}

#[test]
fn extension_of_ignores_dots_in_earlier_path_segments() {
    assert_eq!(extension_of("v1.2/notes/example.canvas"), "canvas");
}
