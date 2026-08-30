use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    FAST, Facet, FacetOptions, Field, IndexRecordOption, JsonObjectOptions, OwnedValue, STORED,
    STRING, Schema, TEXT, TantivyDocument, Term, TextFieldIndexing, Value as _,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{DateTime, Index, IndexReader, IndexWriter, ReloadPolicy};
use time::OffsetDateTime;

use crate::document::strip_closing_hashes;
use crate::{IndexedDocument, SearchError};

/// One ranked full-text search request.
#[derive(Clone, Copy, Debug)]
pub struct TextQuery<'a> {
    pub query: &'a str,
    pub path_prefix: Option<&'a str>,
    /// Forward-slash relative path prefixes to exclude, matching whole path
    /// segments with the same semantics as `path_prefix`:
    /// `exclude_paths = ["old"]` excludes `"old"` itself and everything
    /// under `"old/"`, never `"oldstuff.md"`. A hit is excluded if it
    /// matches any entry. Composable with `path_prefix`: both are applied.
    pub exclude_paths: &'a [String],
    pub tags: &'a [String],
    pub fields: &'a Map<String, Value>,
    pub limit: usize,
}

/// One ranked search hit with a highlighted, heading-aware snippet.
#[derive(Clone, Debug, PartialEq)]
pub struct TextHit {
    pub path: String,
    pub score: f32,
    pub title: String,
    pub snippet: String,
    pub modified: OffsetDateTime,
}

/// Stored identity of one indexed document for freshness comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub path: String,
    /// Lowercase SHA-256 hex of the complete source content.
    pub content_hash: String,
    pub modified: OffsetDateTime,
}

/// Port for the ranked full-text index.
pub trait IndexesText: Send + Sync {
    /// Upserts the given documents in one committed batch.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be written.
    fn index(&self, documents: &[IndexedDocument]) -> Result<(), SearchError>;

    /// Removes the document stored under the given relative path.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be written.
    fn remove(&self, path: &str) -> Result<(), SearchError>;

    /// Returns ranked hits for the request.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unparsable syntax or filter
    /// values, and a storage error when the index cannot be read.
    fn query(&self, request: &TextQuery<'_>) -> Result<Vec<TextHit>, SearchError>;

    /// Returns every stored document identity for freshness comparison.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index cannot be read.
    fn entries(&self) -> Result<Vec<IndexEntry>, SearchError>;
}

/// Trusted location of the tantivy index directory.
#[derive(Clone, Debug)]
pub struct TextIndexConfig {
    pub directory: PathBuf,
}

#[derive(Clone, Copy, Debug)]
struct TextFields {
    path: Field,
    path_facet: Field,
    title: Field,
    headings: Field,
    body: Field,
    tags: Field,
    frontmatter: Field,
    modified: Field,
    content_hash: Field,
}

/// Default `IndexesText` implementation over a tantivy directory.
///
/// The exclusive tantivy `IndexWriter` lock is acquired only for the
/// duration of a write (`index`/`remove`), never held for the lifetime of
/// this handle: an MCP stdio connection is inherently one process per
/// client, so a permanently-held writer lock serves no exclusion purpose
/// while idle and only prevents a second, otherwise independent connection
/// to the same vault from starting at all.
pub struct TantivyIndex {
    index: Index,
    reader: IndexReader,
    write_gate: Mutex<()>,
    fields: TextFields,
}

impl TryFrom<TextIndexConfig> for TantivyIndex {
    type Error = SearchError;

    fn try_from(value: TextIndexConfig) -> Result<Self, Self::Error> {
        std::fs::create_dir_all(&value.directory).map_err(|source| {
            SearchError::IndexDirectory {
                path: value.directory.display().to_string(),
                source,
            }
        })?;
        let (schema, fields) = build_schema();
        let directory = tantivy::directory::MmapDirectory::open(&value.directory)
            .map_err(tantivy::TantivyError::from)?;
        let index = Index::open_or_create(directory, schema)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            reader,
            write_gate: Mutex::new(()),
            fields,
        })
    }
}

impl IndexesText for TantivyIndex {
    fn index(&self, documents: &[IndexedDocument]) -> Result<(), SearchError> {
        if documents.is_empty() {
            return Ok(());
        }
        self.with_writer(|writer| {
            for document in documents {
                writer.delete_term(Term::from_field_text(self.fields.path, document.path()));
                writer.add_document(stored_document(self.fields, document))?;
            }
            Ok(())
        })?;
        self.reader.reload()?;
        Ok(())
    }

    fn remove(&self, path: &str) -> Result<(), SearchError> {
        self.with_writer(|writer| {
            writer.delete_term(Term::from_field_text(self.fields.path, path));
            Ok(())
        })?;
        self.reader.reload()?;
        Ok(())
    }

    fn query(&self, request: &TextQuery<'_>) -> Result<Vec<TextHit>, SearchError> {
        let searcher = self.reader.searcher();
        let mut parser = QueryParser::for_index(
            &self.index,
            vec![self.fields.title, self.fields.headings, self.fields.body],
        );
        parser.set_field_boost(self.fields.title, 2.0);
        parser.set_field_boost(self.fields.headings, 1.5);
        let parsed =
            parser
                .parse_query(request.query)
                .map_err(|error| SearchError::InvalidQuery {
                    query: request.query.to_owned(),
                    reason: error.to_string(),
                })?;

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, parsed)];
        if let Some(prefix) = request.path_prefix {
            clauses.push(facet_clause(self.fields.path_facet, prefix, Occur::Must));
        }
        for excluded in request.exclude_paths {
            clauses.push(facet_clause(
                self.fields.path_facet,
                excluded,
                Occur::MustNot,
            ));
        }
        for tag in request.tags {
            clauses.push(facet_clause(self.fields.tags, tag, Occur::Must));
        }
        for (field, value) in request.fields {
            let term = json_term(self.fields.frontmatter, field, value)?;
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
        }
        let query: Box<dyn Query> = Box::new(BooleanQuery::new(clauses));

        let collector = TopDocs::with_limit(request.limit.max(1)).order_by_score();
        let top_docs = searcher.search(&query, &collector)?;
        let mut generator = SnippetGenerator::create(&searcher, &*query, self.fields.body)?;
        generator.set_max_num_chars(240);

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, address) in top_docs {
            let stored: TantivyDocument = searcher.doc(address)?;
            let body = stored_text(&stored, self.fields.body);
            hits.push(TextHit {
                path: stored_text(&stored, self.fields.path),
                score,
                title: stored_text(&stored, self.fields.title),
                snippet: render_snippet(&generator, &body),
                modified: stored_date(&stored, self.fields.modified),
            });
        }
        Ok(hits)
    }

    fn entries(&self) -> Result<Vec<IndexEntry>, SearchError> {
        let searcher = self.reader.searcher();
        let addresses = searcher.search(
            &tantivy::query::AllQuery,
            &tantivy::collector::DocSetCollector,
        )?;
        let mut entries = Vec::with_capacity(addresses.len());
        for address in addresses {
            let stored: TantivyDocument = searcher.doc(address)?;
            entries.push(IndexEntry {
                path: stored_text(&stored, self.fields.path),
                content_hash: stored_text(&stored, self.fields.content_hash),
                modified: stored_date(&stored, self.fields.modified),
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }
}

impl TantivyIndex {
    /// Acquires the exclusive tantivy `IndexWriter` lock, runs `mutate`,
    /// commits, then drops the writer before returning: the OS-level lock
    /// is held only for this call, not for the life of `self`. `write_gate`
    /// serialises acquisition attempts from other threads in this same
    /// process; it does not itself provide cross-process exclusion, tantivy's
    /// own directory lock does that.
    fn with_writer(
        &self,
        mutate: impl FnOnce(&mut IndexWriter) -> Result<(), SearchError>,
    ) -> Result<(), SearchError> {
        let _gate = self
            .write_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut writer = self.index.writer_with_num_threads(1, 32_000_000)?;
        mutate(&mut writer)?;
        writer.commit()?;
        Ok(())
    }
}

fn build_schema() -> (Schema, TextFields) {
    let mut builder = Schema::builder();
    let path = builder.add_text_field("path", STRING | STORED);
    let path_facet = builder.add_facet_field("path_facet", FacetOptions::default());
    let title = builder.add_text_field("title", TEXT | STORED);
    let headings = builder.add_text_field("headings", TEXT);
    let body = builder.add_text_field("body", TEXT | STORED);
    let tags = builder.add_facet_field("tags", FacetOptions::default());
    let frontmatter = builder.add_json_field(
        "frontmatter",
        JsonObjectOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        ),
    );
    let modified = builder.add_date_field("modified", STORED | FAST);
    let content_hash = builder.add_text_field("content_hash", STRING | STORED);
    let schema = builder.build();
    (
        schema,
        TextFields {
            path,
            path_facet,
            title,
            headings,
            body,
            tags,
            frontmatter,
            modified,
            content_hash,
        },
    )
}

fn stored_document(fields: TextFields, document: &IndexedDocument) -> TantivyDocument {
    let mut stored = TantivyDocument::new();
    stored.add_text(fields.path, document.path());
    stored.add_facet(fields.path_facet, segment_facet(document.path()));
    stored.add_text(fields.title, document.title());
    for heading in document.headings() {
        stored.add_text(fields.headings, heading);
    }
    stored.add_text(fields.body, document.body());
    for tag in document.tags() {
        stored.add_facet(fields.tags, segment_facet(tag));
    }
    if !document.frontmatter().is_empty() {
        let object: BTreeMap<String, OwnedValue> = document
            .frontmatter()
            .iter()
            .map(|(key, value)| (key.clone(), OwnedValue::from(value.clone())))
            .collect();
        stored.add_object(fields.frontmatter, object);
    }
    stored.add_date(fields.modified, DateTime::from_utc(document.modified()));
    let hash: &str = document.content_hash().into();
    stored.add_text(fields.content_hash, hash);
    stored
}

/// Builds a facet from forward-slash segments, escaping each component.
fn segment_facet(path: &str) -> Facet {
    Facet::from_path(path.split('/'))
}

fn facet_clause(field: Field, path: &str, occur: Occur) -> (Occur, Box<dyn Query>) {
    let term = Term::from_facet(field, &segment_facet(path));
    (
        occur,
        Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
    )
}

fn json_term(field: Field, path: &str, value: &Value) -> Result<Term, SearchError> {
    let mut term = Term::from_field_json_path(field, path, false);
    match value {
        Value::String(text) => term.append_type_and_str(text),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                term.append_type_and_fast_value(int);
            } else if let Some(float) = number.as_f64() {
                term.append_type_and_fast_value(float);
            } else {
                return Err(SearchError::InvalidFieldFilter {
                    field: path.to_owned(),
                });
            }
        }
        Value::Bool(flag) => term.append_type_and_fast_value(*flag),
        Value::Null | Value::Array(_) | Value::Object(_) => {
            return Err(SearchError::InvalidFieldFilter {
                field: path.to_owned(),
            });
        }
    }
    Ok(term)
}

fn stored_text(document: &TantivyDocument, field: Field) -> String {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn stored_date(document: &TantivyDocument, field: Field) -> OffsetDateTime {
    document
        .get_first(field)
        .and_then(|value| value.as_datetime())
        .map_or(OffsetDateTime::UNIX_EPOCH, DateTime::into_utc)
}

fn render_snippet(generator: &SnippetGenerator, body: &str) -> String {
    let snippet = generator.snippet(body);
    if snippet.is_empty() {
        return fallback_snippet(body);
    }
    let html = snippet.to_html();
    match heading_before(body, snippet.fragment()) {
        Some(heading) => format!("{heading} › {html}"),
        None => html,
    }
}

/// Returns a plain leading excerpt when the query matched outside the body.
fn fallback_snippet(body: &str) -> String {
    let trimmed = body.trim();
    let mut end = trimmed.len().min(160);
    while !trimmed.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    trimmed.get(..end).unwrap_or_default().to_owned()
}

/// Finds the closest heading above the snippet fragment for context.
fn heading_before(body: &str, fragment: &str) -> Option<String> {
    if fragment.is_empty() {
        return None;
    }
    let position = body.find(fragment)?;
    let mut heading = None;
    let mut offset = 0_usize;
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        if offset > position {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&hashes)
                && let Some(text) = trimmed.get(hashes..)
                && (text.starts_with(' ') || text.starts_with('\t'))
            {
                let text = strip_closing_hashes(text.trim());
                if !text.is_empty() {
                    heading = Some(text.to_owned());
                }
            }
        }
        offset = offset.saturating_add(line.len());
    }
    heading
}
