use super::*;

#[test]
fn a_lowercase_hyphenated_slug_is_url_safe() {
    assert!(is_url_safe_slug("task-register"));
    assert!(is_url_safe_slug("a1-b2"));
}

#[test]
fn an_empty_slug_is_not_url_safe() {
    assert!(!is_url_safe_slug(""));
}

#[test]
fn a_slug_with_uppercase_or_symbols_is_not_url_safe() {
    assert!(!is_url_safe_slug("Task-Register"));
    assert!(!is_url_safe_slug("task_register"));
    assert!(!is_url_safe_slug("task register"));
    assert!(!is_url_safe_slug("task/register"));
}

#[test]
fn manifest_kind_and_target_parse_from_their_documented_toml_values() -> Result<(), Box<dyn std::error::Error>> {
    let raw: ManifestRaw = toml::from_str(
        r#"
            name = "Task Register Dashboard"
            slug = "task-register"
            kind = "spa"
            entry = "index.html"
            target = "_blank"
            mcp_servers = ["contextos"]
        "#,
    )?;
    assert_eq!(raw.name, "Task Register Dashboard");
    assert_eq!(raw.slug.as_deref(), Some("task-register"));
    assert_eq!(raw.kind, AppKind::Spa);
    assert_eq!(raw.entry, "index.html");
    assert_eq!(raw.target, AppTarget::Blank);
    assert_eq!(raw.mcp_servers, vec!["contextos".to_owned()]);
    Ok(())
}

#[test]
fn manifest_kind_htmx_and_target_embed_parse() -> Result<(), Box<dyn std::error::Error>> {
    let raw: ManifestRaw = toml::from_str(
        r#"
            name = "Live Widget"
            kind = "htmx"
            entry = "widget.html"
            target = "embed"
        "#,
    )?;
    assert_eq!(raw.kind, AppKind::Htmx);
    assert_eq!(raw.target, AppTarget::Embed);
    assert_eq!(raw.slug, None);
    assert!(raw.mcp_servers.is_empty());
    Ok(())
}

#[test]
fn an_unknown_manifest_field_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "spa"
            entry = "index.html"
            target = "_blank"
            surprise = "field"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn an_invalid_kind_value_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "electron"
            entry = "index.html"
            target = "_blank"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn a_missing_required_field_is_a_schema_violation() {
    let result: Result<ManifestRaw, _> = toml::from_str(
        r#"
            name = "App"
            kind = "spa"
            target = "_blank"
        "#,
    );
    assert!(result.is_err());
}
