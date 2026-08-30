use std::collections::BTreeSet;

use contextos_index::{IndexDocument, IndexEntry, IndexEntryKind, IndexReconcileInput};
use proptest::prelude::*;

#[test]
fn fr_20_reconciles_membership_order_and_preserves_operator_content()
-> Result<(), Box<dyn std::error::Error>> {
    let source = concat!(
        "# Notes: Index\n\n",
        "Operator introduction.\n\n",
        "<!-- contextos:index:begin -->\n",
        "| Item | Summary |\n",
        "| --- | --- |\n",
        "| [keep.md](keep.md) | Hand-edited summary |\n",
        "| [gone.md](gone.md) | Remove me |\n",
        "<!-- contextos:index:end -->\n\n",
        "Operator footer.\n",
    );
    let input = IndexReconcileInput {
        source: Some(source),
        directory_name: "notes",
        entries: vec![
            IndexEntry {
                name: "keep.md".to_owned(),
                kind: IndexEntryKind::File,
                suggested_summary: "Generated replacement".to_owned(),
            },
            IndexEntry {
                name: "sub-folder".to_owned(),
                kind: IndexEntryKind::Directory,
                suggested_summary: "New directory".to_owned(),
            },
        ],
    };

    let document = IndexDocument::try_from(input)?;
    let rendered = String::from(document);

    assert!(rendered.starts_with("# Notes: Index\n\nOperator introduction.\n\n"));
    assert!(rendered.ends_with("\n\nOperator footer.\n"));
    assert!(
        rendered.contains("| [sub-folder/](sub-folder/index.md) | New directory <!-- auto --> |")
    );
    assert!(rendered.contains("| [keep.md](keep.md) | Hand-edited summary |"));
    assert!(!rendered.contains("gone.md"));
    let directory_position = rendered
        .find("sub-folder/")
        .ok_or("directory row missing")?;
    let file_position = rendered.find("keep.md").ok_or("file row missing")?;
    assert!(directory_position < file_position);
    Ok(())
}

#[test]
fn fr_21_appends_a_managed_block_without_restructuring_existing_prose()
-> Result<(), Box<dyn std::error::Error>> {
    let source = "# Bespoke landing page\n\nOperator prose without a final newline.";
    let input = IndexReconcileInput {
        source: Some(source),
        directory_name: "custom",
        entries: vec![IndexEntry {
            name: "note.md".to_owned(),
            kind: IndexEntryKind::File,
            suggested_summary: "A note".to_owned(),
        }],
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert!(rendered.starts_with(source));
    assert!(rendered.contains("\n\n## Contents\n\n<!-- contextos:index:begin -->"));
    Ok(())
}

#[test]
fn fr_20_creates_a_complete_index_when_none_exists() -> Result<(), Box<dyn std::error::Error>> {
    let input = IndexReconcileInput {
        source: None,
        directory_name: "client-projects",
        entries: Vec::new(),
    };

    let rendered = String::from(IndexDocument::try_from(input)?);

    assert_eq!(
        rendered,
        concat!(
            "# Client Projects: Index\n\n",
            "<!-- contextos:index:begin -->\n",
            "| Item | Summary |\n",
            "| --- | --- |\n",
            "<!-- contextos:index:end -->\n",
        )
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn fr_21_operator_bytes_and_hand_edited_summaries_survive_random_sequences(
        introduction in "[A-Za-z0-9 .,;:!?\\n]{0,160}",
        footer in "[A-Za-z0-9 .,;:!?\\n]{0,160}",
        operations in prop::collection::vec((0_u8..16, any::<bool>()), 0..64),
    ) {
        let prefix = format!("# Generated Test: Index\n\n{introduction}\n\n");
        let suffix = format!("\n\n{footer}\n");
        let mut source = format!(
            "{prefix}<!-- contextos:index:begin -->\n\
             | Item | Summary |\n\
             | --- | --- |\n\
             | [anchor.md](anchor.md) | Operator-owned summary |\n\
             <!-- contextos:index:end -->{suffix}"
        );
        let mut dynamic_entries = BTreeSet::new();

        for (entry_number, present) in operations {
            let name = format!("generated-{entry_number}.md");
            if present {
                dynamic_entries.insert(name);
            } else {
                dynamic_entries.remove(&name);
            }
            let mut entries = vec![IndexEntry {
                name: "anchor.md".to_owned(),
                kind: IndexEntryKind::File,
                suggested_summary: "Must never replace operator text".to_owned(),
            }];
            entries.extend(dynamic_entries.iter().map(|name| IndexEntry {
                name: name.clone(),
                kind: IndexEntryKind::File,
                suggested_summary: format!("Summary for {name}"),
            }));
            let document = IndexDocument::try_from(IndexReconcileInput {
                source: Some(&source),
                directory_name: "generated-test",
                entries,
            }).map_err(|error| TestCaseError::fail(error.to_string()))?;
            source = String::from(document);

            prop_assert!(source.starts_with(&prefix));
            prop_assert!(source.ends_with(&suffix));
            prop_assert!(source.contains(
                "| [anchor.md](anchor.md) | Operator-owned summary |"
            ));
        }
    }
}
