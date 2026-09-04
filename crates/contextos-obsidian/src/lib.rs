#![forbid(unsafe_code)]

mod base;
mod base_query;
mod canvas;
mod markdown;

use serde_json::{Map, Value};
use thiserror::Error;

pub use base::{BaseDiagnostic, BaseDocument, BaseError, BaseOperation};
pub use base_query::{
    BaseQueryError, ColumnValue, FileMetadata, QueryDefinition, QueryFormat, RowContext, ScanRootHint, SortKey,
    compare_values, evaluate_filters, render, resolve_column, resolve_property, resolve_row, scan_root_hint,
};
pub use canvas::{CanvasCreateInput, CanvasDiagnostic, CanvasDocument, CanvasError, CanvasOperation};
pub use markdown::{LinkCollection, MarkdownError, ObsidianLink, ValidatedMarkdown};

/// Inputs for one validated Obsidian note document.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteCreateInput<'a> {
    pub title: &'a str,
    pub frontmatter: Map<String, Value>,
    pub content: &'a str,
    pub timestamp: &'a str,
}

/// A validated note with resolved, ordered default frontmatter.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteDocument(FrontmatterDocument);

impl TryFrom<NoteCreateInput<'_>> for NoteDocument {
    type Error = NoteError;

    fn try_from(value: NoteCreateInput<'_>) -> Result<Self, Self::Error> {
        if value.title.trim().is_empty() {
            return Err(NoteError::EmptyTitle);
        }
        if value.timestamp.trim().is_empty() {
            return Err(NoteError::EmptyTimestamp);
        }
        ValidatedMarkdown::try_from(value.content)?;
        let mut frontmatter = Map::new();
        frontmatter.insert("type".to_owned(), Value::String("note".to_owned()));
        frontmatter.insert("title".to_owned(), Value::String(value.title.to_owned()));
        frontmatter.insert("entity".to_owned(), Value::String("personal".to_owned()));
        frontmatter.insert("status".to_owned(), Value::String("new".to_owned()));
        frontmatter.insert("created".to_owned(), Value::String(value.timestamp.to_owned()));
        frontmatter.insert("updated".to_owned(), Value::String(value.timestamp.to_owned()));
        frontmatter.insert("tags".to_owned(), Value::Array(Vec::new()));
        frontmatter.insert("aliases".to_owned(), Value::Array(Vec::new()));
        for (key, property) in value.frontmatter {
            frontmatter.insert(key, property);
        }
        Ok(Self(FrontmatterDocument {
            frontmatter,
            body: value.content.to_owned(),
            body_start_line: 1,
        }))
    }
}

impl TryFrom<NoteDocument> for String {
    type Error = FrontmatterError;

    fn try_from(value: NoteDocument) -> Result<Self, Self::Error> {
        Self::try_from(&value.0)
    }
}

/// Typed note construction failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NoteError {
    #[error("note title must not be empty")]
    EmptyTitle,
    #[error("note timestamp must not be empty")]
    EmptyTimestamp,
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
}

impl NoteError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Markdown(error) => error.code(),
            Self::EmptyTitle | Self::EmptyTimestamp => "format/note",
        }
    }
}

/// An Obsidian note split into ordered JSON frontmatter and untouched body.
#[derive(Clone, Debug, PartialEq)]
pub struct FrontmatterDocument {
    frontmatter: Map<String, Value>,
    body: String,
    body_start_line: usize,
}

impl TryFrom<&str> for FrontmatterDocument {
    type Error = FrontmatterError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let first_line_end = match source.find('\n') {
            Some(index) => index,
            None => source.len(),
        };
        if source[..first_line_end].trim_end_matches('\r') != "---" {
            return Ok(Self {
                frontmatter: Map::new(),
                body: source.to_owned(),
                body_start_line: 1,
            });
        }

        let yaml_start = first_line_end.saturating_add(usize::from(first_line_end < source.len()));
        let mut line_number = 2_usize;
        let mut offset = yaml_start;
        for line in source[yaml_start..].split_inclusive('\n') {
            let content = line.trim_end_matches(['\r', '\n']);
            if content == "---" {
                let yaml = &source[yaml_start..offset];
                let frontmatter = yaml_serde::from_str::<Map<String, Value>>(yaml).map_err(|source| {
                    let (line, column) = source.location().map_or((2, 1), |location| {
                        (location.line().saturating_add(1), location.column())
                    });
                    FrontmatterError::InvalidYaml { line, column, source }
                })?;
                return Ok(Self {
                    frontmatter,
                    body: source[offset.saturating_add(line.len())..].to_owned(),
                    body_start_line: line_number.saturating_add(1),
                });
            }
            offset = offset.saturating_add(line.len());
            line_number = line_number.saturating_add(1);
        }

        Err(FrontmatterError::Unclosed { line: 1 })
    }
}

impl TryFrom<String> for FrontmatterDocument {
    type Error = FrontmatterError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl FrontmatterDocument {
    /// Returns frontmatter properties in their source order.
    #[must_use]
    pub const fn frontmatter(&self) -> &Map<String, Value> {
        &self.frontmatter
    }

    /// Returns the body byte-for-byte from the source note.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns the 1-based source line at which the body begins.
    #[must_use]
    pub const fn body_start_line(&self) -> usize {
        self.body_start_line
    }

    /// Applies RFC 7386 object semantics and stamps `updated` when omitted.
    pub fn apply_merge_patch(&mut self, patch: Map<String, Value>, updated: &str) {
        let explicitly_updates_timestamp = patch.contains_key("updated");
        apply_object_patch(&mut self.frontmatter, patch);
        if !explicitly_updates_timestamp {
            self.frontmatter
                .insert("updated".to_owned(), Value::String(updated.to_owned()));
        }
    }
}

impl TryFrom<&FrontmatterDocument> for String {
    type Error = FrontmatterError;

    fn try_from(value: &FrontmatterDocument) -> Result<Self, Self::Error> {
        let yaml =
            yaml_serde::to_string(&value.frontmatter).map_err(|source| FrontmatterError::Serialise { source })?;
        let mut rendered = String::with_capacity(yaml.len().saturating_add(value.body.len()).saturating_add(8));
        rendered.push_str("---\n");
        rendered.push_str(&yaml);
        rendered.push_str("---\n");
        rendered.push_str(&value.body);
        Ok(rendered)
    }
}

fn apply_object_patch(target: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, patch_value) in patch {
        match patch_value {
            Value::Null => {
                target.shift_remove(&key);
            }
            Value::Object(nested_patch) => {
                let target_value = target.entry(key).or_insert_with(|| Value::Object(Map::new()));
                if !target_value.is_object() {
                    *target_value = Value::Object(Map::new());
                }
                if let Some(target_object) = target_value.as_object_mut() {
                    apply_object_patch(target_object, nested_patch);
                }
            }
            replacement => {
                target.insert(key, replacement);
            }
        }
    }
}

/// Typed frontmatter codec failures.
#[derive(Debug, Error)]
pub enum FrontmatterError {
    #[error("YAML frontmatter opened on line {line} but has no closing delimiter")]
    Unclosed { line: usize },
    #[error("YAML frontmatter is invalid at line {line}, column {column}")]
    InvalidYaml {
        line: usize,
        column: usize,
        #[source]
        source: yaml_serde::Error,
    },
    #[error("frontmatter could not be serialised as YAML")]
    Serialise {
        #[source]
        source: yaml_serde::Error,
    },
}

impl FrontmatterError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/frontmatter"
    }

    /// Returns an actionable correction for the malformed note.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the YAML frontmatter without changing the note body."
    }
}
