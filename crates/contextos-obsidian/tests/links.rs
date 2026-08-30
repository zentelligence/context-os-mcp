use contextos_obsidian::{LinkCollection, MarkdownError, ObsidianLink, ValidatedMarkdown};

#[test]
fn fr_43_reads_wikilinks_and_embeds_in_source_order() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "---\n",
        "related: \"[[Frontmatter Note]]\"\n",
        "---\n",
        "[[Plain]] [[Target|Display]] [[Target#Heading]] [[Target#^block-id]]\n",
        "![[image.png|300]]\n",
    );

    let links = LinkCollection::try_from(source)?;

    assert_eq!(
        links.outgoing(),
        [
            ObsidianLink {
                target: "Frontmatter Note".to_owned(),
                display: None,
                heading: None,
                block: None,
                embed: false,
            },
            ObsidianLink {
                target: "Plain".to_owned(),
                display: None,
                heading: None,
                block: None,
                embed: false,
            },
            ObsidianLink {
                target: "Target".to_owned(),
                display: Some("Display".to_owned()),
                heading: None,
                block: None,
                embed: false,
            },
            ObsidianLink {
                target: "Target".to_owned(),
                display: None,
                heading: Some("Heading".to_owned()),
                block: None,
                embed: false,
            },
            ObsidianLink {
                target: "Target".to_owned(),
                display: None,
                heading: None,
                block: Some("block-id".to_owned()),
                embed: false,
            },
            ObsidianLink {
                target: "image.png".to_owned(),
                display: Some("300".to_owned()),
                heading: None,
                block: None,
                embed: true,
            },
        ]
    );
    Ok(())
}

#[test]
fn fr_43_ignores_code_comments_and_escaped_wikilink_syntax()
-> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "`[[Inline Code]]` and \\[[Escaped]]\n",
        "%% [[Commented]] %%\n",
        "```markdown\n[[Fenced Code]]\n```\n",
        "[[Visible]]\n",
    );

    let links = LinkCollection::try_from(source)?;

    assert_eq!(links.outgoing().len(), 1);
    assert_eq!(links.outgoing()[0].target, "Visible");
    Ok(())
}

#[test]
fn fr_40_rejects_an_unclosed_wikilink_with_its_source_line() {
    let source = "# Note\n\nText ![[broken embed\n";

    let result = LinkCollection::try_from(source);

    assert!(matches!(
        result,
        Err(MarkdownError::UnclosedLink {
            line: 3,
            embed: true
        })
    ));
}

#[test]
fn fr_40_rejects_empty_wikilink_targets() {
    let result = LinkCollection::try_from("Before [[  ]] after\n");

    assert!(matches!(
        result,
        Err(MarkdownError::EmptyLinkTarget { line: 1 })
    ));
}

#[test]
fn fr_40_accepts_nested_foldable_and_custom_callouts() -> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "> [!warning]- Review\n",
        "> Content with [[Target]].\n",
        "> > [!custom-type]+ Nested\n",
        "> > More content.\n",
    );

    let document = ValidatedMarkdown::try_from(source)?;

    assert_eq!(document.links().outgoing().len(), 1);
    Ok(())
}

#[test]
fn fr_40_rejects_broken_callout_syntax_with_its_source_line() {
    let result = ValidatedMarkdown::try_from("# Note\n\n> [!warning Missing close\n");

    assert!(matches!(
        result,
        Err(MarkdownError::InvalidCallout { line: 3 })
    ));
}

#[test]
fn fr_40_rejects_an_empty_callout_type() {
    let result = ValidatedMarkdown::try_from("> [!] Empty type\n");

    assert!(matches!(
        result,
        Err(MarkdownError::InvalidCallout { line: 1 })
    ));
}
