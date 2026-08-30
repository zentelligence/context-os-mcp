mod support;

use contextos_search::{DocumentSource, IndexedDocument};
use sha2::{Digest, Sha256};
use support::{document, timestamp, vault_note};

#[test]
fn fr_50_derives_title_headings_tags_and_body_from_note() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempfile::tempdir()?;
    let content = "---\ntitle: Alpha Note\ntags:\n  - project/alpha\n  - review\n---\n\n# Alpha Overview\n\nIntro with an inline #status/active tag.\n\n## Details\n\n```\n# not a heading\n#not-a-tag\n```\n\nMore prose.\n";
    let (_roots, path) = vault_note(&vault, "notes/alpha.md", content)?;
    let modified = timestamp()?;

    let document = IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified,
    });

    assert_eq!(document.path(), "notes/alpha.md");
    assert_eq!(document.title(), "Alpha Note");
    assert_eq!(document.headings(), ["Alpha Overview", "Details"]);
    assert_eq!(
        document.tags(),
        ["project/alpha", "review", "status/active"]
    );
    assert!(document.body().contains("# Alpha Overview"));
    assert!(!document.body().contains("title: Alpha Note"));
    assert_eq!(
        document
            .frontmatter()
            .get("title")
            .and_then(|value| value.as_str()),
        Some("Alpha Note")
    );
    assert_eq!(document.modified(), modified);
    let digest: [u8; 32] = Sha256::digest(content.as_bytes()).into();
    let expected_hash = contextos_core::ContentHash::from(digest);
    let expected: &str = (&expected_hash).into();
    let actual: &str = document.content_hash().into();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn fr_50_title_falls_back_to_first_heading_then_filename() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempfile::tempdir()?;
    let modified = timestamp()?;

    let heading_only = "## Quarterly Planning ##\n\nBody prose.\n";
    let (_roots, heading_path) = vault_note(&vault, "planning.md", heading_only)?;
    let heading_document = IndexedDocument::from(DocumentSource {
        path: &heading_path,
        content: heading_only,
        modified,
    });
    assert_eq!(heading_document.title(), "Quarterly Planning");

    let plain = "No headings at all.\n";
    let (_roots, plain_path) = vault_note(&vault, "meeting-notes.md", plain)?;
    let plain_document = IndexedDocument::from(DocumentSource {
        path: &plain_path,
        content: plain,
        modified,
    });
    assert_eq!(plain_document.title(), "meeting notes");
    Ok(())
}

#[test]
fn fr_50_invalid_frontmatter_degrades_to_body_only_document()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "---\ntitle: [unclosed\n---\n\n# Recovered Heading\n\nProse.\n";
    let (_roots, path) = vault_note(&vault, "broken.md", content)?;

    let document = IndexedDocument::from(DocumentSource {
        path: &path,
        content,
        modified: timestamp()?,
    });

    assert!(document.frontmatter().is_empty());
    assert_eq!(document.body(), content);
    assert_eq!(document.title(), "Recovered Heading");
    Ok(())
}

#[test]
fn fr_50_inline_tags_respect_code_and_numeric_rules() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "---\ntags: solo\n---\nText #alpha and `#inline-code` and #2024 and #a1.\n\n~~~\n#fenced\n~~~\n\nRepeat #alpha once.\n";

    let parsed = document(&vault, "tags.md", content)?;

    assert_eq!(parsed.tags(), ["solo", "alpha", "a1"]);
    Ok(())
}
