use contextos_obsidian::{FrontmatterDocument, FrontmatterError};
use serde_json::json;

#[test]
fn fr_41_reads_ordered_yaml_frontmatter_and_preserves_body()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: Example\nstatus: active\ntags:\n  - project\n---\n\n# Body\nText.\n";

    let document = FrontmatterDocument::try_from(source)?;
    let keys = document
        .frontmatter()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();

    assert_eq!(keys, ["title", "status", "tags"]);
    assert_eq!(document.frontmatter().get("title"), Some(&json!("Example")));
    assert_eq!(
        document.frontmatter().get("tags"),
        Some(&json!(["project"]))
    );
    assert_eq!(document.body(), "\n# Body\nText.\n");
    assert_eq!(document.body_start_line(), 7);
    Ok(())
}

#[test]
fn fr_41_missing_frontmatter_returns_an_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Plain note\n\nBody.\n";

    let document = FrontmatterDocument::try_from(source)?;

    assert!(document.frontmatter().is_empty());
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start_line(), 1);
    Ok(())
}

#[test]
fn fr_42_malformed_yaml_reports_source_line_and_column_without_repair()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "---\ntitle: [broken\n---\nBody\n";

    let result = FrontmatterDocument::try_from(source);

    let (line, column) = match result {
        Err(FrontmatterError::InvalidYaml { line, column, .. }) => (line, column),
        Err(error) => return Err(error.into()),
        Ok(_) => {
            return Err(std::io::Error::other(
                "malformed YAML did not return its precise location",
            )
            .into());
        }
    };
    assert_eq!((line, column), (3, 1));
    Ok(())
}

#[test]
fn fr_41_empty_frontmatter_returns_an_empty_object() -> Result<(), Box<dyn std::error::Error>> {
    let source = "---\n---\nBody\n";

    let document = FrontmatterDocument::try_from(source)?;

    assert!(document.frontmatter().is_empty());
    assert_eq!(document.body(), "Body\n");
    assert_eq!(document.body_start_line(), 3);
    Ok(())
}

#[test]
fn fr_42_unclosed_frontmatter_is_rejected_without_consuming_the_body() {
    let source = "---\ntitle: Example\n# Body mistaken for YAML\n";

    let result = FrontmatterDocument::try_from(source);

    assert!(matches!(
        result,
        Err(FrontmatterError::Unclosed { line: 1 })
    ));
}

#[test]
fn fr_42_merge_patch_preserves_existing_key_order_and_body_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "title: Original\n",
        "status: draft\n",
        "metadata:\n",
        "  owner: Peter\n",
        "  obsolete: true\n",
        "---\n",
        "\n# Body\n\nUntouched.\n",
    );
    let mut document = FrontmatterDocument::try_from(source)?;
    let patch = serde_json::from_value(json!({
        "title": "Revised",
        "status": null,
        "metadata": {"obsolete": null, "priority": 2},
        "entity": "jie"
    }))?;

    document.apply_merge_patch(patch, "2026-07-18T08:30:00+10:00");
    let rendered = String::try_from(&document)?;
    let reparsed = FrontmatterDocument::try_from(rendered.as_str())?;
    let keys = reparsed
        .frontmatter()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();

    assert_eq!(keys, ["title", "metadata", "entity", "updated"]);
    assert_eq!(reparsed.frontmatter().get("title"), Some(&json!("Revised")));
    assert_eq!(
        reparsed.frontmatter().get("metadata"),
        Some(&json!({"owner": "Peter", "priority": 2}))
    );
    assert_eq!(
        reparsed.frontmatter().get("updated"),
        Some(&json!("2026-07-18T08:30:00+10:00"))
    );
    assert_eq!(reparsed.body(), "\n# Body\n\nUntouched.\n");
    Ok(())
}

#[test]
fn fr_42_explicit_updated_value_is_not_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let mut document = FrontmatterDocument::try_from("Body only\n")?;
    let patch = serde_json::from_value(json!({"updated": "operator-value"}))?;

    document.apply_merge_patch(patch, "automatic-value");

    assert_eq!(
        document.frontmatter().get("updated"),
        Some(&json!("operator-value"))
    );
    Ok(())
}
