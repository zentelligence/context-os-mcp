use contextos_obsidian::{FrontmatterDocument, NoteCreateInput, NoteDocument};
use serde_json::{Map, json};

#[test]
fn note_creation_uses_resolved_defaults_in_stable_key_order() -> Result<(), Box<dyn std::error::Error>> {
    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Daily Reflection",
        frontmatter: Map::new(),
        content: "# Daily Reflection\n\nToday was productive.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;
    let keys = parsed.frontmatter().keys().map(String::as_str).collect::<Vec<_>>();

    assert_eq!(
        keys,
        [
            "type", "title", "entity", "status", "created", "updated", "tags", "aliases"
        ]
    );
    assert_eq!(parsed.frontmatter().get("type"), Some(&json!("note")));
    assert_eq!(parsed.frontmatter().get("title"), Some(&json!("Daily Reflection")));
    assert_eq!(parsed.frontmatter().get("entity"), Some(&json!("personal")));
    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("new")));
    assert_eq!(parsed.frontmatter().get("tags"), Some(&json!([])));
    assert_eq!(parsed.frontmatter().get("aliases"), Some(&json!([])));
    assert_eq!(parsed.body(), "# Daily Reflection\n\nToday was productive.\n");
    Ok(())
}

#[test]
fn supplied_frontmatter_overrides_defaults_and_keeps_additional_keys() -> Result<(), Box<dyn std::error::Error>> {
    let supplied = serde_json::from_value(json!({
        "entity": "business",
        "status": "active",
        "tags": ["review"],
        "priority": 2
    }))?;

    let note = NoteDocument::try_from(NoteCreateInput {
        title: "Review",
        frontmatter: supplied,
        content: "Review body.\n",
        timestamp: "2026-07-18T18:30:00+10:00",
    })?;
    let rendered = String::try_from(note)?;
    let parsed = FrontmatterDocument::try_from(rendered.as_str())?;

    assert_eq!(parsed.frontmatter().get("entity"), Some(&json!("business")));
    assert_eq!(parsed.frontmatter().get("status"), Some(&json!("active")));
    assert_eq!(parsed.frontmatter().get("tags"), Some(&json!(["review"])));
    assert_eq!(parsed.frontmatter().get("priority"), Some(&json!(2)));
    Ok(())
}
