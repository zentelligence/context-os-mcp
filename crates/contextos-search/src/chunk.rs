use contextos_core::{ContentHash, VaultPath};
use sha2::{Digest, Sha256};

use crate::document::{detect_atx_heading, relative_display};

/// Target lower bound, in estimated tokens, for a heading-bounded chunk
/// (services specification §4 "Vectors"). A heuristic band, not a contract:
/// the final chunk of a section, and any section shorter than the band, are
/// intentionally allowed to fall under this bound rather than being padded
/// or merged across a heading boundary.
const TARGET_MIN_TOKENS: usize = 200;

/// Target upper bound, in estimated tokens, for a heading-bounded chunk.
const TARGET_MAX_TOKENS: usize = 400;

/// Overlap, in estimated tokens, carried from the end of one chunk into the
/// start of the next chunk within the same section.
const OVERLAP_TOKENS: usize = 40;

/// Documented approximation used by [`estimate_tokens`]: roughly four UTF-8
/// characters per token. This mirrors the common rule of thumb for
/// English-language byte-pair-encoded tokenisers and needs no model or
/// network access, but it is only an estimate. See the module documentation
/// for why the 200 to 400 token band is a heuristic, not a contract.
const CHARS_PER_TOKEN: usize = 4;

/// Estimates the token count of `text` without a model tokeniser.
///
/// # Approximation
///
/// This counts Unicode scalar values (`char`s, not bytes) and divides by
/// [`CHARS_PER_TOKEN`], rounding up. It is a deterministic, dependency-free
/// stand-in for a real tokeniser, calibrated loosely against typical
/// English-language byte-pair encodings. It will disagree with any specific
/// embedding model's actual tokenisation, sometimes significantly for dense
/// non-Latin scripts or code; callers must treat the 200 to 400 token
/// chunking band as a heuristic target, never an exact contract.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// Inputs for deriving heading-bounded chunks from one vault markdown
/// document (services specification §4 "Vectors").
///
/// `content` is the complete markdown source to chunk. Callers that also
/// index the document for text search (see [`crate::DocumentSource`])
/// typically pass the same frontmatter-stripped body used there, so heading
/// context and chunk text describe the same prose; chunking itself performs
/// no frontmatter handling.
#[derive(Clone, Copy, Debug)]
pub struct ChunkSource<'a> {
    pub path: &'a VaultPath,
    pub content: &'a str,
}

/// One heading-bounded chunk of a vault markdown document, ready for
/// embedding.
///
/// Chunk identity is the pair `(path, ordinal)`; the content hash lets a
/// caller skip re-embedding a chunk whose text has not changed since the
/// last build, even when other chunks in the same document did change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    path: String,
    ordinal: usize,
    heading_context: Vec<String>,
    text: String,
    content_hash: ContentHash,
}

impl Chunk {
    /// Returns the forward-slash relative vault path of the source document.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the zero-based position of this chunk within its document.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Returns the heading trail bounding this chunk, outermost heading
    /// first (for example `["H1 title", "H2 subsection"]`). Empty when the
    /// chunk precedes the document's first heading.
    #[must_use]
    pub fn heading_context(&self) -> &[String] {
        &self.heading_context
    }

    /// Returns the chunk's prose as a byte-exact substring of the source
    /// section: internal whitespace, newlines, and indentation (for example
    /// inside a fenced code block) are preserved exactly as written. Word
    /// boundaries only decide where a chunk starts and ends; they never
    /// rewrite what is between them.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the SHA-256 identity of this chunk's text, used to gate
    /// re-embedding on content change.
    #[must_use]
    pub const fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }

    /// Builds a one-off chunk wrapping a `query_semantic` search string, so
    /// [`crate::EmbedsText::embed`] never needs a second, chunk-shaped-versus-
    /// raw-text entry point: the architecture trait catalogue fixes
    /// `embed(chunks) -> vectors` as the only embedding surface. `path` and
    /// `ordinal` are empty and `0`: this chunk is never stored, only embedded
    /// and discarded, so it needs no real chunk identity.
    pub(crate) fn query(text: &str) -> Self {
        let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        Self {
            path: String::new(),
            ordinal: 0,
            heading_context: Vec::new(),
            text: text.to_owned(),
            content_hash: ContentHash::from(digest),
        }
    }
}

/// One heading-bounded region of a document: the heading trail active when
/// the section opened, and the section's own prose (excluding heading
/// lines).
struct Section {
    heading_context: Vec<String>,
    body: String,
}

/// Splits `content` into heading-bounded sections in document order.
///
/// Reuses [`detect_atx_heading`] (see its documentation for the setext
/// exclusion rationale) so heading recognition never drifts from the text
/// index's own heading extraction. A fenced code block (`` ``` `` or `~~~`)
/// suspends heading detection for any `#` lines inside it, matching
/// `CommonMark` and the existing text-index behaviour.
fn parse_sections(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut trail: Vec<(usize, String)> = Vec::new();
    let mut body = String::new();
    let mut in_fence = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            body.push_str(line);
            body.push('\n');
            continue;
        }
        if !in_fence && let Some((level, text)) = detect_atx_heading(trimmed) {
            flush_section(&trail, &mut body, &mut sections);
            trail.retain(|(existing_level, _)| *existing_level < level);
            trail.push((level, text.to_owned()));
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    flush_section(&trail, &mut body, &mut sections);
    sections
}

fn flush_section(trail: &[(usize, String)], body: &mut String, sections: &mut Vec<Section>) {
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        sections.push(Section {
            heading_context: trail.iter().map(|(_, text)| text.clone()).collect(),
            body: trimmed.to_owned(),
        });
    }
    body.clear();
}

/// Returns the byte span (start, end) of every whitespace-delimited word in
/// `body`, in source order. A "word" is defined identically to
/// [`str::split_whitespace`] (a maximal run of non-whitespace `char`s), so
/// `&body[start..end]` for one span always equals the corresponding entry
/// `body.split_whitespace()` would yield. Callers use these byte offsets to
/// slice `body` directly rather than rejoining word substrings, which keeps
/// [`Chunk::text`] a byte-exact substring of the source: internal
/// whitespace, newlines, and indentation between the first and last word of
/// a range survive untouched.
fn word_spans(body: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (index, character) in body.char_indices() {
        if character.is_whitespace() {
            if let Some(word_start) = start.take() {
                spans.push((word_start, index));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(word_start) = start {
        spans.push((word_start, body.len()));
    }
    spans
}

/// Splits one section's prose into word-index ranges sized to the target
/// token band, overlapping adjacent ranges by [`OVERLAP_TOKENS`].
///
/// Splitting on whitespace-delimited words (rather than raw characters)
/// keeps every chunk boundary word-aligned; [`chunk_document`] then slices
/// the original section text between the first and last word of each range
/// (see [`word_spans`]), so a range boundary only ever falls between two
/// words, never inside one, while everything the range spans (including
/// internal whitespace) is preserved byte-for-byte. One accepted edge case:
/// if a single word at a chunk boundary is itself longer than roughly
/// `OVERLAP_TOKENS * CHARS_PER_TOKEN` (about 160 characters), the overlap
/// walk-back below cannot include even that one word within the 40-token
/// budget, so the overlap for that boundary degrades to zero; this is a
/// heuristic trade-off, not a correctness bug. Each range's token estimate
/// is computed from a prefix-sum of word lengths, so the search for both the
/// target-sized end and the overlap-sized start is linear in the number of
/// ranges produced, not the number of words in the section.
fn split_section_words(words: &[&str]) -> Vec<(usize, usize)> {
    if words.is_empty() {
        return Vec::new();
    }

    // prefix[i] is the length, in characters, of words[0..i] joined by
    // single spaces (no leading or trailing space).
    let mut prefix = Vec::with_capacity(words.len().saturating_add(1));
    prefix.push(0_usize);
    let mut running = 0_usize;
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            running = running.saturating_add(1);
        }
        running = running.saturating_add(word.chars().count());
        prefix.push(running);
    }
    let chars_between = |start: usize, end: usize| -> usize {
        if end <= start {
            return 0;
        }
        let separator = usize::from(start > 0);
        prefix[end] - prefix[start] - separator
    };
    let tokens_between =
        |start: usize, end: usize| -> usize { chars_between(start, end).div_ceil(CHARS_PER_TOKEN) };

    let mut ranges = Vec::new();
    let mut start = 0_usize;
    while start < words.len() {
        let mut end = start.saturating_add(1);
        while end < words.len() && tokens_between(start, end.saturating_add(1)) <= TARGET_MAX_TOKENS
        {
            end = end.saturating_add(1);
        }
        ranges.push((start, end));
        if end >= words.len() {
            break;
        }
        let mut new_start = end;
        while new_start > start.saturating_add(1)
            && tokens_between(new_start.saturating_sub(1), end) <= OVERLAP_TOKENS
        {
            new_start = new_start.saturating_sub(1);
        }
        start = new_start;
    }

    // A trailing remainder under the target minimum is folded into the
    // previous chunk rather than left as an under-sized final chunk; the
    // previous chunk grows beyond the target maximum in that case, which is
    // an accepted trade-off for the heuristic band (see module docs).
    if ranges.len() > 1 {
        let last_tokens = ranges
            .last()
            .map_or(0, |&(start, end)| tokens_between(start, end));
        if last_tokens < TARGET_MIN_TOKENS
            && let Some((_, last_end)) = ranges.pop()
            && let Some(previous) = ranges.last_mut()
        {
            previous.1 = last_end;
        }
    }
    ranges
}

/// Produces heading-bounded chunks for one vault markdown document.
///
/// The document is split into sections at each `CommonMark` ATX heading
/// (see [`detect_atx_heading`]); a chunk never spans two sections. A section
/// within the [`TARGET_MIN_TOKENS`] to [`TARGET_MAX_TOKENS`] band becomes
/// one chunk; an oversized section is split into overlapping,
/// target-sized chunks; a section with no prose (for example a heading
/// immediately followed by another heading) produces no chunk. Ordinals are
/// assigned sequentially over the whole document, in section order.
///
/// This is a pure function: no I/O, no clock, and the same input always
/// produces the same ordinals, heading context, text, and content hashes.
/// `Chunk::text` is always a byte-exact substring of `source.content`,
/// including internal whitespace and newlines (see [`word_spans`]).
#[must_use]
pub fn chunk_document(source: ChunkSource<'_>) -> Vec<Chunk> {
    let path = relative_display(source.path);
    let mut chunks = Vec::new();
    let mut ordinal = 0_usize;
    for section in parse_sections(source.content) {
        let spans = word_spans(&section.body);
        let words: Vec<&str> = spans
            .iter()
            .map(|&(start, end)| &section.body[start..end])
            .collect();
        for (start, end) in split_section_words(&words) {
            let text_start = spans[start].0;
            let text_end = spans[end.saturating_sub(1)].1;
            let text = section.body[text_start..text_end].to_owned();
            let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
            chunks.push(Chunk {
                path: path.clone(),
                ordinal,
                heading_context: section.heading_context.clone(),
                text,
                content_hash: ContentHash::from(digest),
            });
            ordinal = ordinal.saturating_add(1);
        }
    }
    chunks
}
