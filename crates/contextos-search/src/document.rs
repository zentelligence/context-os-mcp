use contextos_core::{ContentHash, VaultPath, extract_tags};
use contextos_obsidian::FrontmatterDocument;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Inputs for deriving one searchable document from vault markdown.
#[derive(Clone, Copy, Debug)]
pub struct DocumentSource<'a> {
    pub path: &'a VaultPath,
    pub content: &'a str,
    pub modified: OffsetDateTime,
}

/// One vault markdown document prepared for text indexing.
///
/// Extraction never fails: a note with invalid frontmatter degrades to a
/// body-only document so the vault's real content remains searchable.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedDocument {
    path: String,
    title: String,
    headings: Vec<String>,
    body: String,
    tags: Vec<String>,
    frontmatter: Map<String, Value>,
    modified: OffsetDateTime,
    content_hash: ContentHash,
}

impl From<DocumentSource<'_>> for IndexedDocument {
    fn from(value: DocumentSource<'_>) -> Self {
        let digest: [u8; 32] = Sha256::digest(value.content.as_bytes()).into();
        let (frontmatter, body) = match FrontmatterDocument::try_from(value.content) {
            Ok(parsed) => (parsed.frontmatter().clone(), parsed.body().to_owned()),
            Err(_) => (Map::new(), value.content.to_owned()),
        };
        let headings = extract_headings(&body);
        let title = derive_title(&frontmatter, &headings, value.path);
        let tags = extract_tags(&frontmatter, &body);
        Self {
            path: relative_display(value.path),
            title,
            headings,
            body,
            tags,
            frontmatter,
            modified: value.modified,
            content_hash: ContentHash::from(digest),
        }
    }
}

impl IndexedDocument {
    /// Returns the forward-slash relative vault path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the derived document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns heading text in source order.
    #[must_use]
    pub fn headings(&self) -> &[String] {
        &self.headings
    }

    /// Returns the note body without frontmatter.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns frontmatter and inline tags, deduplicated in source order.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Returns parsed frontmatter properties in source order.
    #[must_use]
    pub const fn frontmatter(&self) -> &Map<String, Value> {
        &self.frontmatter
    }

    /// Returns the caller-observed modification time.
    #[must_use]
    pub const fn modified(&self) -> OffsetDateTime {
        self.modified
    }

    /// Returns the SHA-256 identity of the complete source content.
    #[must_use]
    pub const fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }
}

pub(crate) fn relative_display(path: &VaultPath) -> String {
    let components: Vec<String> = path
        .relative()
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    components.join("/")
}

fn extract_headings(body: &str) -> Vec<String> {
    let mut headings = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((_, text)) = detect_atx_heading(trimmed) {
            headings.push(text.to_owned());
        }
    }
    headings
}

/// Returns the ATX heading level (1 to 6) and trimmed heading text for one
/// already left-trimmed, non-fenced line, or `None` when the line is not a
/// `CommonMark` ATX heading.
///
/// Shared by heading extraction ([`extract_headings`]) and heading-bounded
/// chunking (`crate::chunk`) so the two consumers never disagree on what
/// counts as a heading. Only ATX headings (`#` through `######` followed by
/// whitespace) are recognised; setext headings (an underline of `=` or `-`
/// beneath a title line) are deliberately out of scope because no markdown
/// parser is present in this crate's dependency graph to recognise them
/// reliably, and a second hand-rolled parser would duplicate this one. A
/// setext-style title is therefore treated as plain body text.
pub(crate) fn detect_atx_heading(trimmed: &str) -> Option<(usize, &str)> {
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let text = trimmed.get(hashes..)?;
    if !text.starts_with(' ') && !text.starts_with('\t') {
        return None;
    }
    let text = strip_closing_hashes(text.trim());
    if text.is_empty() { None } else { Some((hashes, text)) }
}

/// Removes a `CommonMark` closing-hash sequence, which must follow whitespace.
pub(crate) fn strip_closing_hashes(text: &str) -> &str {
    let without_hashes = text.trim_end_matches('#');
    if without_hashes.len() == text.len() || !without_hashes.ends_with([' ', '\t']) {
        return text;
    }
    without_hashes.trim_end()
}

fn derive_title(frontmatter: &Map<String, Value>, headings: &[String], path: &VaultPath) -> String {
    if let Some(title) = frontmatter
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        return title.to_owned();
    }
    if let Some(heading) = headings.first() {
        return heading.clone();
    }
    humanised_stem(path)
}

fn humanised_stem(path: &VaultPath) -> String {
    path.relative()
        .file_stem()
        .map(|stem| stem.to_string_lossy().replace(['-', '_'], " "))
        .unwrap_or_default()
        .trim()
        .to_owned()
}
