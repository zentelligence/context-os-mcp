mod support;

use std::collections::HashSet;
use std::fmt::Write as _;

use contextos_search::{Chunk, ChunkSource, chunk_document, estimate_tokens};
use proptest::prelude::*;
use support::vault_note;

fn chunks_for(
    vault: &tempfile::TempDir,
    relative: &str,
    content: &str,
) -> Result<Vec<Chunk>, Box<dyn std::error::Error>> {
    let (_roots, path) = vault_note(vault, relative, content)?;
    Ok(chunk_document(ChunkSource { path: &path, content }))
}

#[test]
fn empty_document_produces_no_chunks() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "empty.md", "")?;
    assert!(chunks.is_empty());
    Ok(())
}

#[test]
fn whitespace_only_document_produces_no_chunks() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let chunks = chunks_for(&vault, "blank.md", "\n\n   \n\t\n")?;
    assert!(chunks.is_empty());
    Ok(())
}

#[test]
fn document_with_no_headings_yields_one_chunk_with_empty_heading_context() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "Plain prose with no structure at all.\n";
    let chunks = chunks_for(&vault, "plain.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].ordinal(), 0);
    assert!(chunks[0].heading_context().is_empty());
    assert_eq!(chunks[0].path(), "plain.md");
    assert_eq!(chunks[0].text(), "Plain prose with no structure at all.");
    Ok(())
}

#[test]
fn single_heading_carries_its_own_text_as_heading_context() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# Overview\n\nSome introductory prose.\n";
    let chunks = chunks_for(&vault, "single.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_context(), ["Overview"]);
    assert_eq!(chunks[0].text(), "Some introductory prose.");
    Ok(())
}

#[test]
fn preamble_before_first_heading_forms_its_own_chunk() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "Preamble text.\n\n# First Heading\n\nBody one.\n";
    let chunks = chunks_for(&vault, "preamble.md", content)?;

    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].heading_context().is_empty());
    assert_eq!(chunks[0].text(), "Preamble text.");
    assert_eq!(chunks[0].ordinal(), 0);
    assert_eq!(chunks[1].heading_context(), ["First Heading"]);
    assert_eq!(chunks[1].text(), "Body one.");
    assert_eq!(chunks[1].ordinal(), 1);
    Ok(())
}

#[test]
fn nested_headings_produce_a_correct_heading_trail() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "\
# Guide

## Setup

### Prerequisites

Install the toolchain first.

## Usage

Run the command.
";
    let chunks = chunks_for(&vault, "nested.md", content)?;

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].heading_context(), ["Guide", "Setup", "Prerequisites"]);
    assert_eq!(chunks[0].text(), "Install the toolchain first.");
    assert_eq!(chunks[1].heading_context(), ["Guide", "Usage"]);
    assert_eq!(chunks[1].text(), "Run the command.");
    Ok(())
}

#[test]
fn heading_with_no_body_before_the_next_heading_produces_no_chunk() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# Empty Section\n\n## Actual Content\n\nHere is the prose.\n";
    let chunks = chunks_for(&vault, "empty-section.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_context(), ["Empty Section", "Actual Content"]);
    Ok(())
}

#[test]
fn code_block_hash_lines_are_never_treated_as_headings() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "\
# Real Heading

Some prose before the fence.

```
# not a heading
## also not a heading
```

More prose after the fence.
";
    let chunks = chunks_for(&vault, "fenced.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_context(), ["Real Heading"]);
    assert!(chunks[0].text().contains("not a heading"));
    assert!(chunks[0].text().contains("More prose after the fence."));
    Ok(())
}

#[test]
fn chunk_text_is_byte_exact_for_a_section_with_a_fenced_code_block() -> Result<(), Box<dyn std::error::Error>> {
    // The `chunk` field is returned verbatim to MCP clients and its content
    // hash gates re-embedding, so whitespace-normalised reconstruction is
    // unacceptable: a code block must survive with its newlines and
    // indentation intact, not collapse to one space-joined line.
    let vault = tempfile::tempdir()?;
    let content = "\
# Heading

Intro line.

```
fn example() {
    let x = 1;
}
```

Trailing prose with  double   spaces and\ta tab.
";
    let chunks = chunks_for(&vault, "byte-exact.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert!(
        content.contains(chunks[0].text()),
        "chunk text must be a byte-exact substring of the source"
    );
    let expected = "Intro line.\n\n```\nfn example() {\n    let x = 1;\n}\n```\n\nTrailing prose with  double   spaces and\ta tab.";
    assert_eq!(chunks[0].text(), expected);
    Ok(())
}

#[test]
fn chunk_text_preserves_multiline_indentation() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "\
# Notes

- Item one
  - Nested item
- Item two

    Indented paragraph continuation.
";
    let chunks = chunks_for(&vault, "indented.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert!(
        content.contains(chunks[0].text()),
        "chunk text must be a byte-exact substring of the source"
    );
    let expected = "- Item one\n  - Nested item\n- Item two\n\n    Indented paragraph continuation.";
    assert_eq!(chunks[0].text(), expected);
    Ok(())
}

#[test]
fn tilde_fenced_hash_lines_are_never_treated_as_headings() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "\
# Heading

~~~
# still not a heading
~~~

Tail prose.
";
    let chunks = chunks_for(&vault, "tilde-fenced.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_context(), ["Heading"]);
    assert!(chunks[0].text().contains("still not a heading"));
    Ok(())
}

#[test]
fn setext_style_titles_are_treated_as_plain_text() -> Result<(), Box<dyn std::error::Error>> {
    // The crate has no CommonMark parser in its dependency graph and its
    // existing heading extraction (`document::extract_headings`) recognises
    // only ATX headings; chunking deliberately matches that behaviour rather
    // than hand-rolling setext support. This test locks the decision down.
    let vault = tempfile::tempdir()?;
    let content = "Setext Title\n============\n\nBody prose.\n";
    let chunks = chunks_for(&vault, "setext.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].heading_context().is_empty());
    assert!(chunks[0].text().contains("Setext Title"));
    assert!(chunks[0].text().contains("============"));
    Ok(())
}

#[test]
fn unicode_content_is_chunked_correctly() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# 見出し\n\n日本語のテキストと emoji 🎉 と café.\n";
    let chunks = chunks_for(&vault, "unicode.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].heading_context(), ["見出し"]);
    assert!(chunks[0].text().contains("🎉"));
    assert!(chunks[0].text().contains("café"));
    // The token estimate counts Unicode scalar values, not bytes: a
    // byte-based estimate would be roughly three times higher for this
    // mostly multi-byte CJK/emoji text.
    let tokens = estimate_tokens(chunks[0].text());
    let byte_based_tokens = chunks[0].text().len().div_ceil(4);
    assert!(
        tokens < byte_based_tokens,
        "expected a char-based estimate ({tokens}) well under the byte-based one ({byte_based_tokens})"
    );
    Ok(())
}

#[test]
fn short_section_stays_a_single_unpadded_chunk() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# Tiny\n\nJust a few words.\n";
    let chunks = chunks_for(&vault, "tiny.md", content)?;

    assert_eq!(chunks.len(), 1);
    assert!(estimate_tokens(chunks[0].text()) < 200);
    Ok(())
}

#[test]
fn oversized_section_splits_with_overlap() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let words: Vec<String> = (0..1200).map(|index| format!("word{index}")).collect();
    let body = words.join(" ");
    let content = format!("# Big Section\n\n{body}\n");
    let chunks = chunks_for(&vault, "big.md", &content)?;

    assert!(chunks.len() > 1, "an oversized section must split into multiple chunks");
    for chunk in &chunks {
        assert_eq!(chunk.heading_context(), ["Big Section"]);
    }
    // Ordinals are sequential from zero.
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.ordinal(), index);
    }
    // Adjacent chunks within the section overlap: the tail of one chunk's
    // words reappears at the head of the next.
    for window in chunks.windows(2) {
        let first_words: Vec<&str> = window[0].text().split_whitespace().collect();
        let second_words: Vec<&str> = window[1].text().split_whitespace().collect();
        assert!(!first_words.is_empty(), "chunk text must not be empty");
        let last_of_first = first_words[first_words.len() - 1];
        assert!(
            second_words.contains(&last_of_first),
            "expected overlap: {last_of_first} missing from next chunk"
        );
    }
    Ok(())
}

#[test]
fn undersized_trailing_remainder_is_folded_into_the_previous_chunk() -> Result<(), Box<dyn std::error::Error>> {
    // Uniform 3-character words, joined by single spaces, make the token
    // arithmetic exact: `tokens_between(a, b) == b - a` (each word costs
    // precisely one token because a 3-char word plus its separator is
    // exactly `CHARS_PER_TOKEN` characters). With 500 such words the greedy
    // fill takes a first chunk of 400 tokens/words, the 40-token overlap
    // walk-back lands the second range's start at word 360, and the
    // remaining `500 - 360 = 140` words/tokens are under `TARGET_MIN_TOKENS`
    // (200): precisely the under-sized trailing remainder the fold exists
    // to absorb. Without folding this would surface as a second chunk whose
    // token estimate (140) is below the target minimum; folding instead
    // merges it into the previous chunk, leaving exactly one chunk covering
    // all 500 tokens.
    let vault = tempfile::tempdir()?;
    let words: Vec<&str> = std::iter::repeat_n("abc", 500).collect();
    let body = words.join(" ");
    let content = format!("# Big Section\n\n{body}\n");
    let chunks = chunks_for(&vault, "remainder.md", &content)?;

    assert_eq!(
        chunks.len(),
        1,
        "the under-sized remainder must be folded into the previous chunk, not left standalone"
    );
    let tokens = estimate_tokens(chunks[0].text());
    assert!(
        tokens >= 200,
        "folded final chunk must reach the target minimum, got {tokens} tokens"
    );
    Ok(())
}

#[test]
fn ordinals_are_stable_across_identical_reruns() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# One\n\nFirst body.\n\n# Two\n\nSecond body.\n";
    let first_run = chunks_for(&vault, "stable.md", content)?;
    let second_run = chunks_for(&vault, "stable.md", content)?;

    assert_eq!(first_run, second_run);
    Ok(())
}

#[test]
fn hash_is_stable_when_chunk_text_is_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let content = "# Heading\n\nUnchanged body text.\n";
    let first = chunks_for(&vault, "same.md", content)?;
    let second = chunks_for(&vault, "same.md", content)?;

    assert_eq!(first[0].content_hash(), second[0].content_hash());
    Ok(())
}

#[test]
fn hash_changes_when_chunk_text_changes() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let before = chunks_for(&vault, "before.md", "# Heading\n\nOriginal body text.\n")?;
    let after = chunks_for(&vault, "after.md", "# Heading\n\nChanged body text.\n")?;

    assert_ne!(before[0].content_hash(), after[0].content_hash());
    Ok(())
}

#[test]
fn estimate_tokens_is_a_documented_chars_over_four_approximation() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
    assert_eq!(estimate_tokens(&"a".repeat(400)), 100);
}

#[test]
fn estimate_tokens_counts_unicode_scalar_values_not_bytes() {
    // "日本語日" is 4 Unicode scalar values but 12 UTF-8 bytes (3 bytes per
    // CJK character): a byte-based estimate would report 3 tokens
    // (12 bytes / 4), but the documented approximation counts `char`s, so it
    // must report 1 token (4 chars / 4).
    let text = "日本語日";
    assert_eq!(text.chars().count(), 4);
    assert_eq!(text.len(), 12, "sanity check: this text is 12 UTF-8 bytes");
    assert_eq!(estimate_tokens(text), 1);
}

/// Builds a heading-bounded markdown document with one H1 section per entry
/// in `word_counts`. Each section's words are globally unique
/// (`s{section}w{index}`), so property assertions can check exact set
/// membership rather than approximate text matching.
fn build_sectioned_document(word_counts: &[usize]) -> (String, Vec<Vec<String>>) {
    let mut content = String::new();
    let mut expected_words = Vec::with_capacity(word_counts.len());
    for (section_index, count) in word_counts.iter().enumerate() {
        let _ = writeln!(content, "# Section{section_index}\n");
        let words: Vec<String> = (0..*count)
            .map(|word_index| format!("s{section_index}w{word_index}"))
            .collect();
        if !words.is_empty() {
            content.push_str(&words.join(" "));
            content.push_str("\n\n");
        }
        expected_words.push(words);
    }
    (content, expected_words)
}

fn compute_chunks(content: &str) -> Result<Vec<Chunk>, String> {
    let vault = tempfile::tempdir().map_err(|error| error.to_string())?;
    let (_roots, path) = vault_note(&vault, "property.md", content).map_err(|error| error.to_string())?;
    Ok(chunk_document(ChunkSource { path: &path, content }))
}

proptest! {
    /// Full coverage: every non-empty content region of the document
    /// appears in at least one chunk.
    #[test]
    fn every_section_word_appears_in_at_least_one_chunk(
        word_counts in prop::collection::vec(0_usize..=600, 1..=4),
    ) {
        let (content, expected_words) = build_sectioned_document(&word_counts);
        let chunks = match compute_chunks(&content) {
            Ok(chunks) => chunks,
            Err(message) => return Err(TestCaseError::fail(message)),
        };
        let all_words: HashSet<&str> = chunks
            .iter()
            .flat_map(|chunk| chunk.text().split_whitespace())
            .collect();
        for words in &expected_words {
            for word in words {
                prop_assert!(
                    all_words.contains(word.as_str()),
                    "word {word} missing from every chunk"
                );
            }
        }
    }

    /// Heading-boundary preservation: no chunk spans across a
    /// heading boundary of its bounding section. Since every word is tagged
    /// with its originating section, a chunk that crossed a boundary would
    /// contain words from more than one section marker.
    #[test]
    fn no_chunk_crosses_a_heading_boundary(
        word_counts in prop::collection::vec(1_usize..=600, 2..=4),
    ) {
        let (content, _expected_words) = build_sectioned_document(&word_counts);
        let chunks = match compute_chunks(&content) {
            Ok(chunks) => chunks,
            Err(message) => return Err(TestCaseError::fail(message)),
        };
        for chunk in &chunks {
            let mut sections_seen = HashSet::new();
            for word in chunk.text().split_whitespace() {
                if let Some(marker_end) = word.find('w') {
                    sections_seen.insert(&word[..marker_end]);
                }
            }
            prop_assert!(
                sections_seen.len() <= 1,
                "chunk mixed words from sections {sections_seen:?}"
            );
        }
    }

    /// Overlap bounds: adjacent chunks within one oversized section
    /// share a non-empty word run, and that run never grows far past the
    /// 40-token overlap target.
    #[test]
    fn adjacent_chunks_within_a_section_overlap_within_bounds(
        word_count in 500_usize..=1500,
    ) {
        let (content, _expected_words) = build_sectioned_document(&[word_count]);
        let chunks = match compute_chunks(&content) {
            Ok(chunks) => chunks,
            Err(message) => return Err(TestCaseError::fail(message)),
        };
        prop_assert!(chunks.len() > 1, "input must be large enough to split");
        for window in chunks.windows(2) {
            let second_words: Vec<&str> = window[1].text().split_whitespace().collect();
            let first_set: HashSet<&str> = window[0].text().split_whitespace().collect();
            let overlap_count = second_words
                .iter()
                .take_while(|word| first_set.contains(*word))
                .count();
            prop_assert!(
                overlap_count > 0,
                "adjacent chunks in the same section must overlap"
            );
            let overlap_text = second_words[..overlap_count].join(" ");
            let overlap_tokens = estimate_tokens(&overlap_text);
            // The implementation only ever extends the overlap walk-back
            // while the accumulated token estimate stays at or under
            // `OVERLAP_TOKENS` (40); this is a provable invariant of
            // `split_section_words`, not an approximation, so the bound is
            // exact rather than a generous slack allowance.
            prop_assert!(
                overlap_tokens <= 40,
                "overlap of {overlap_tokens} tokens exceeds the provable 40-token bound"
            );
        }
    }

    /// Chunk-size band adherence for well-formed inputs: every
    /// non-final chunk of an oversized section lands close to the 200 to
    /// 400 token heuristic target (see `chunk::estimate_tokens` docs for why
    /// this is a band, not a contract). Only the section's last chunk is
    /// exempt, since a small trailing remainder is folded into it rather
    /// than left as an under-sized orphan.
    #[test]
    fn non_final_chunks_hit_the_target_band(
        word_count in 500_usize..=1500,
    ) {
        let (content, _expected_words) = build_sectioned_document(&[word_count]);
        let chunks = match compute_chunks(&content) {
            Ok(chunks) => chunks,
            Err(message) => return Err(TestCaseError::fail(message)),
        };
        prop_assert!(chunks.len() > 1, "input must be large enough to split");
        let last_index = chunks.len() - 1;
        for (index, chunk) in chunks.iter().enumerate() {
            if index == last_index {
                continue;
            }
            let tokens = estimate_tokens(chunk.text());
            prop_assert!(
                (200..=400).contains(&tokens),
                "chunk {index} token estimate {tokens} outside the heuristic band"
            );
        }
    }
}
