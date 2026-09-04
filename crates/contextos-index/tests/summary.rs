use contextos_index::{IndexSummary, IndexSummaryInput};

#[test]
fn derives_summary_from_frontmatter_title_and_first_body_sentence() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "title: Better Title\n",
        "status: active\n",
        "---\n",
        "# Ignored Heading\n\n",
        "First sentence explains the note. Second sentence is omitted.\n",
    );

    let summary = IndexSummary::try_from(IndexSummaryInput {
        filename: "fallback-name.md",
        source: Some(source),
    })?;

    assert_eq!(
        <&str>::from(&summary),
        "Better Title: First sentence explains the note."
    );
    Ok(())
}

#[test]
fn derives_summary_from_heading_when_frontmatter_title_is_absent() -> Result<(), Box<dyn std::error::Error>> {
    let source = "# Project Alpha\n\nTracks the active delivery work! More detail follows.\n";

    let summary = IndexSummary::try_from(IndexSummaryInput {
        filename: "fallback-name.md",
        source: Some(source),
    })?;

    assert_eq!(
        <&str>::from(&summary),
        "Project Alpha: Tracks the active delivery work!"
    );
    Ok(())
}

#[test]
fn humanises_filename_when_note_has_no_descriptive_content() -> Result<(), Box<dyn std::error::Error>> {
    let summary = IndexSummary::try_from(IndexSummaryInput {
        filename: "client-project_status.md",
        source: Some("\n\n"),
    })?;

    assert_eq!(<&str>::from(&summary), "Client Project Status");
    Ok(())
}
