use serde_json::json;

use super::*;

type BoxError = Box<dyn std::error::Error>;

const BASE: &str = r#"
[server]
bind = "127.0.0.1:7332"
static_dir = "./static"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml", "--stdio"]
"#;

fn entry(name: &str) -> Result<serde_json::Map<String, serde_json::Value>, BoxError> {
    json!({
        "transport": "stdio",
        "name": name,
        "command": "some-other-server",
        "args": ["--flag"],
    })
    .as_object()
    .cloned()
    .ok_or_else(|| "expected a JSON object".into())
}

#[test]
fn add_mcp_server_appends_a_new_entry_and_preserves_existing_comments() -> Result<(), BoxError> {
    let source = "# a hand-written comment\n".to_owned() + BASE;
    let mut document = WebConfigDocument::parse(&source)?;

    document.add_mcp_server(&entry("second")?)?;

    let rendered = document.render();
    assert!(rendered.contains("# a hand-written comment"));
    assert!(rendered.contains("name = \"second\""));
    assert_eq!(
        document.mcp_server_names(),
        vec!["contextos".to_owned(), "second".to_owned()]
    );
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_duplicate_name_and_leaves_the_document_unchanged() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();

    let result = document.add_mcp_server(&entry("contextos")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_missing_required_field() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let mut incomplete = entry("second")?;
    incomplete.remove("command");

    let result = document.add_mcp_server(&incomplete);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn add_mcp_server_rejects_a_null_value() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let mut malformed = entry("second")?;
    malformed.insert("command".to_owned(), serde_json::Value::Null);

    let result = document.add_mcp_server(&malformed);

    assert!(matches!(result, Err(WebConfigWriterError::UnsupportedValue)));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn patch_mcp_server_merges_fields_and_leaves_others_untouched() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "command".to_owned(),
        serde_json::Value::String("patched-command".to_owned()),
    );

    document.patch_mcp_server("contextos", &patch)?;

    let rendered = document.render();
    assert!(rendered.contains("command = \"patched-command\""));
    assert!(rendered.contains("name = \"contextos\""));
    Ok(())
}

#[test]
fn patch_mcp_server_on_an_unknown_name_is_rejected() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();
    let patch = serde_json::Map::new();

    let result = document.patch_mcp_server("does-not-exist", &patch);

    assert!(matches!(
        result,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) if name == "does-not-exist"
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn replace_mcp_server_swaps_the_full_entry_including_its_name() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;

    document.replace_mcp_server("contextos", &entry("renamed")?)?;

    assert_eq!(document.mcp_server_names(), vec!["renamed".to_owned()]);
    let rendered = document.render();
    assert!(rendered.contains("command = \"some-other-server\""));
    Ok(())
}

#[test]
fn replace_mcp_server_rejects_a_result_with_a_duplicate_name() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    document.add_mcp_server(&entry("second")?)?;
    let before = document.render();

    let result = document.replace_mcp_server("second", &entry("contextos")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn remove_mcp_server_drops_the_named_entry() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    document.add_mcp_server(&entry("second")?)?;

    document.remove_mcp_server("second")?;

    assert_eq!(document.mcp_server_names(), vec!["contextos".to_owned()]);
    Ok(())
}

#[test]
fn remove_mcp_server_on_an_unknown_name_is_rejected_and_leaves_the_document_unchanged() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let before = document.render();

    let result = document.remove_mcp_server("does-not-exist");

    assert!(matches!(
        result,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) if name == "does-not-exist"
    ));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn patch_ui_creates_and_merges_the_server_ui_table() -> Result<(), BoxError> {
    let mut document = WebConfigDocument::parse(BASE)?;
    let mut patch = serde_json::Map::new();
    patch.insert("theme".to_owned(), serde_json::Value::String("dark".to_owned()));

    document.patch_ui(&patch)?;

    let rendered = document.render();
    assert!(rendered.contains("[server.ui]"));
    assert!(rendered.contains("theme = \"dark\""));
    // Existing `[server]` keys survive the nested-table insertion.
    assert!(rendered.contains("bind = \"127.0.0.1:7332\""));
    Ok(())
}

#[test]
fn an_edit_is_rejected_when_the_document_already_carries_an_invalid_bind_address() -> Result<(), BoxError> {
    let corrupt = BASE.replace(r#"bind = "127.0.0.1:7332""#, r#"bind = "not-a-socket-address""#);
    let mut document = WebConfigDocument::parse(&corrupt)?;
    let before = document.render();

    let result = document.add_mcp_server(&entry("second")?);

    assert!(matches!(result, Err(WebConfigWriterError::Invalid { .. })));
    assert_eq!(document.render(), before);
    Ok(())
}

#[test]
fn parse_rejects_invalid_toml() {
    let result = WebConfigDocument::parse("not = [valid");
    assert!(matches!(result, Err(WebConfigWriterError::Toml { .. })));
}
