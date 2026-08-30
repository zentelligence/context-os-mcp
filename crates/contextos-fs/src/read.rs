use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

use contextos_core::{ContentHash, VaultPath, VaultSet};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{FsError, FsErrorInfo};

/// Limits applied to filesystem operations for a vault set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsLimits {
    pub max_read_bytes: u64,
    pub max_batch_files: usize,
}

/// Trusted construction input for the native filesystem adapter.
#[derive(Clone, Debug)]
pub struct FilesystemConfig {
    pub roots: VaultSet,
    pub limits: Vec<FsLimits>,
    /// Path patterns hidden from every enumeration surface for this vault,
    /// one set per root, aligned by index with `roots`. Governs
    /// omission from listings only; a direct, explicit-path read is never
    /// affected. See [`crate::default_hidden_patterns`] for the baseline
    /// callers should layer their own patterns on top of.
    pub hidden: Vec<Vec<String>>,
    pub atomic_write_guard: Option<Arc<dyn GuardsAtomicWrites>>,
}

/// Controllable boundary immediately after temporary content is durably flushed.
pub trait GuardsAtomicWrites: std::fmt::Debug + Send + Sync {
    /// Allows persistence to continue or injects a pre-rename failure.
    ///
    /// # Errors
    ///
    /// Returns an I/O error to simulate interruption before atomic replacement.
    fn after_flush(&self, target: &Path) -> Result<(), std::io::Error>;
}

#[derive(Debug)]
struct AllowAtomicWrites;

impl GuardsAtomicWrites for AllowAtomicWrites {
    fn after_flush(&self, _target: &Path) -> Result<(), std::io::Error> {
        Ok(())
    }
}

/// Root-confined filesystem operations.
#[derive(Clone, Debug)]
pub struct Filesystem {
    pub(crate) roots: VaultSet,
    pub(crate) limits: Vec<FsLimits>,
    pub(crate) hidden: Vec<Vec<String>>,
    pub(crate) atomic_write_guard: Arc<dyn GuardsAtomicWrites>,
}

impl TryFrom<FilesystemConfig> for Filesystem {
    type Error = FsError;

    fn try_from(value: FilesystemConfig) -> Result<Self, Self::Error> {
        if value.roots.len() != value.limits.len() {
            return Err(FsError::LimitCountMismatch {
                root_count: value.roots.len(),
                limit_count: value.limits.len(),
            });
        }
        if value.roots.len() != value.hidden.len() {
            return Err(FsError::HiddenCountMismatch {
                root_count: value.roots.len(),
                hidden_count: value.hidden.len(),
            });
        }
        if let Some((root_index, _)) = value
            .limits
            .iter()
            .enumerate()
            .find(|(_, limits)| limits.max_read_bytes == 0 || limits.max_batch_files == 0)
        {
            return Err(FsError::InvalidLimits { root_index });
        }
        let atomic_write_guard = match value.atomic_write_guard {
            Some(guard) => guard,
            None => Arc::new(AllowAtomicWrites),
        };
        Ok(Self {
            roots: value.roots,
            limits: value.limits,
            hidden: value.hidden,
            atomic_write_guard,
        })
    }
}

/// A validated 1-based inclusive line range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineRange {
    from: usize,
    to: usize,
}

impl TryFrom<(usize, usize)> for LineRange {
    type Error = FsError;

    fn try_from(value: (usize, usize)) -> Result<Self, Self::Error> {
        let (from, to) = value;
        if from == 0 || to == 0 || from > to {
            return Err(FsError::InvalidRange { from, to });
        }
        Ok(Self { from, to })
    }
}

/// Optional selection applied to a text read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadLimit {
    Head(usize),
    Tail(usize),
    Range(LineRange),
}

/// Application request for one text file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadTextRequest {
    pub path: VaultPath,
    pub limit: Option<ReadLimit>,
}

/// Result of a UTF-8 text read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadTextResult {
    pub content: String,
    pub line_count: usize,
    pub content_hash: ContentHash,
    pub truncated: bool,
}

/// Request for one MCP-embeddable file capped independently at 10 MiB.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentRequest {
    pub path: VaultPath,
}

/// Root-confined bytes and detected media type for an MCP attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub path: String,
    /// The validated vault path, used to build a `{name}://{relative-path}`
    /// URI addressing-compatible with the resource surface. This superseded
    /// an earlier scheme that addressed attachments by a plain `file://`
    /// path, so every filesystem-backed MCP content item now shares one URI
    /// form regardless of which tool produced it.
    pub vault_path: VaultPath,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: bool,
}

/// Best-effort MIME type for a path by extension, via `mime_guess`'s
/// registry-backed table (the same data behind Apache's `mime.types` and
/// most browsers) rather than a hand-rolled, narrow match: the one
/// extension-based source both `read_attachment` and the resource
/// surface's listing use, so the same file is never described
/// with two different MIME types depending on which surface reports it.
/// Returns `None` for anything `mime_guess` doesn't recognise; callers
/// choose their own fallback (a listing has no content in hand and
/// defaults to a generic type; `read_attachment` content-sniffs first via
/// `infer` and only reaches this as its own fallback).
#[must_use]
pub fn mime_type_for_extension(path: &Path) -> Option<String> {
    mime_guess::from_path(path)
        .first()
        .map(|mime| mime.essence_str().to_owned())
}

/// Application request for isolated reads of several files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadManyRequest {
    pub paths: Vec<VaultPath>,
}

/// Per-file result for a batch read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadManyResult {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<ContentHash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<FsErrorInfo>,
}

impl Filesystem {
    /// Reads a text or binary attachment with the fixed MCP 10 MiB cap.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement, metadata, size, type, or read error.
    pub fn read_attachment(&self, request: &AttachmentRequest) -> Result<Attachment, FsError> {
        const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
        let path: &Path = (&request.path).into();
        if !self.roots.authorises(&request.path) {
            return Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        let metadata = match path.metadata() {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(FsError::ReadMetadata {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !metadata.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        if metadata.len() > MAX_ATTACHMENT_BYTES {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                maximum: MAX_ATTACHMENT_BYTES,
            });
        }
        let bytes = std::fs::read(path).map_err(|source| FsError::ReadContent {
            path: path.to_path_buf(),
            source,
        })?;
        let text = !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok();
        // Content-sniffing (`infer`, magic bytes) is authoritative when it
        // recognises the bytes; an extension can lie, content cannot.
        // Most plain-text formats (JSON, YAML, Markdown, CSV, plain text)
        // have no magic-byte signature, so `infer` correctly returns
        // `None` for those and extension-based `mime_guess` fills the
        // gap; the text/binary heuristic is the last resort for
        // anything neither recognises.
        let mime_type = infer::get(&bytes)
            .map(|kind| kind.mime_type().to_owned())
            .or_else(|| mime_type_for_extension(path))
            .unwrap_or_else(|| {
                if text {
                    "text/plain"
                } else {
                    "application/octet-stream"
                }
                .to_owned()
            });
        Ok(Attachment {
            path: request.path.relative().to_string_lossy().into_owned(),
            vault_path: request.path.clone(),
            mime_type,
            bytes,
            text,
        })
    }

    /// Returns the largest batch that can be valid across all configured roots.
    #[must_use]
    pub fn batch_capacity(&self) -> usize {
        self.limits.iter().fold(0_usize, |capacity, limits| {
            capacity.saturating_add(limits.max_batch_files)
        })
    }

    /// Reads UTF-8 text while streaming hashes, validation, and line selection.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the path is unauthorised, missing, not a
    /// regular file, too large without a limiter, unreadable, or binary.
    pub fn read_text(&self, request: &ReadTextRequest) -> Result<ReadTextResult, FsError> {
        let path: &std::path::Path = (&request.path).into();
        if !self.roots.authorises(&request.path) {
            return Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        let metadata = match path.metadata() {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(FsError::ReadMetadata {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !metadata.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        let limits = self.limits(&request.path)?;
        if metadata.len() > limits.max_read_bytes && request.limit.is_none() {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                maximum: limits.max_read_bytes,
            });
        }

        let file = File::open(path).map_err(|source| FsError::OpenRead {
            path: path.to_path_buf(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = Vec::<u8>::new();
        let mut accumulator = ReadAccumulator::from(request.limit);

        loop {
            buffer.clear();
            let bytes_read =
                reader
                    .read_until(b'\n', &mut buffer)
                    .map_err(|source| FsError::ReadContent {
                        path: path.to_path_buf(),
                        source,
                    })?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer);
            if buffer.contains(&0) {
                return Err(FsError::Binary {
                    path: path.to_path_buf(),
                });
            }
            let line = std::str::from_utf8(&buffer).map_err(|_| FsError::Binary {
                path: path.to_path_buf(),
            })?;
            accumulator.observe(line);
        }

        let (content, line_count, truncated) = accumulator.finish();

        Ok(ReadTextResult {
            content,
            line_count,
            content_hash: ContentHash::from(<[u8; 32]>::from(hasher.finalize())),
            truncated,
        })
    }

    /// Reads a bounded batch without promoting item failures to batch failure.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::BatchTooLarge`] when the request exceeds the
    /// configured item limit. Individual read errors remain in their item.
    pub fn read_many(&self, request: ReadManyRequest) -> Result<Vec<ReadManyResult>, FsError> {
        let mut counts = vec![0_usize; self.limits.len()];
        for path in &request.paths {
            if !self.roots.authorises(path) {
                continue;
            }
            let absolute: &std::path::Path = path.into();
            let root_index =
                usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
                    path: absolute.to_path_buf(),
                    source,
                })?;
            let count = counts
                .get_mut(root_index)
                .ok_or(FsError::LimitCountMismatch {
                    root_count: self.roots.len(),
                    limit_count: self.limits.len(),
                })?;
            *count += 1;
        }
        for (root_index, count) in counts.into_iter().enumerate() {
            let limits =
                self.limits
                    .get(root_index)
                    .copied()
                    .ok_or(FsError::LimitCountMismatch {
                        root_count: self.roots.len(),
                        limit_count: self.limits.len(),
                    })?;
            if count > limits.max_batch_files {
                return Err(FsError::BatchTooLarge {
                    count,
                    maximum: limits.max_batch_files,
                });
            }
        }

        Ok(request
            .paths
            .into_iter()
            .map(|path| {
                let display_path = path.relative().to_string_lossy().into_owned();
                match self.read_text(&ReadTextRequest { path, limit: None }) {
                    Ok(result) => ReadManyResult {
                        path: display_path,
                        content: Some(result.content),
                        content_hash: Some(result.content_hash),
                        error: None,
                    },
                    Err(error) => ReadManyResult {
                        path: display_path,
                        content: None,
                        content_hash: None,
                        error: Some(FsErrorInfo::from(&error)),
                    },
                }
            })
            .collect())
    }

    pub(crate) fn limits(&self, path: &VaultPath) -> Result<FsLimits, FsError> {
        let absolute: &std::path::Path = path.into();
        let root_index =
            usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
                path: absolute.to_path_buf(),
                source,
            })?;
        self.limits
            .get(root_index)
            .copied()
            .ok_or(FsError::LimitCountMismatch {
                root_count: self.roots.len(),
                limit_count: self.limits.len(),
            })
    }

    /// Returns the hidden path patterns configured for `path`'s vault root.
    /// Applied by enumeration surfaces only (`list_directory`,
    /// `list_directory_with_sizes`, `directory_tree`, `search_files`); a
    /// direct, explicit-path read never consults this.
    pub(crate) fn hidden(&self, path: &VaultPath) -> Result<&[String], FsError> {
        let absolute: &Path = path.into();
        let root_index =
            usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
                path: absolute.to_path_buf(),
                source,
            })?;
        self.hidden
            .get(root_index)
            .map(Vec::as_slice)
            .ok_or(FsError::HiddenCountMismatch {
                root_count: self.roots.len(),
                hidden_count: self.hidden.len(),
            })
    }
}

struct ReadAccumulator {
    limit: Option<ReadLimit>,
    selected: String,
    tail: VecDeque<String>,
    line_count: usize,
    selected_count: usize,
}

impl From<Option<ReadLimit>> for ReadAccumulator {
    fn from(value: Option<ReadLimit>) -> Self {
        Self {
            limit: value,
            selected: String::new(),
            tail: VecDeque::new(),
            line_count: 0,
            selected_count: 0,
        }
    }
}

impl ReadAccumulator {
    fn observe(&mut self, line: &str) {
        self.line_count += 1;
        match self.limit {
            None => self.select(line),
            Some(ReadLimit::Head(maximum)) if self.line_count <= maximum => self.select(line),
            Some(ReadLimit::Tail(maximum)) if maximum > 0 => {
                self.tail.push_back(line.to_owned());
                if self.tail.len() > maximum {
                    self.tail.pop_front();
                }
            }
            Some(ReadLimit::Range(range))
                if self.line_count >= range.from && self.line_count <= range.to =>
            {
                self.select(line);
            }
            Some(ReadLimit::Head(_) | ReadLimit::Tail(_) | ReadLimit::Range(_)) => {}
        }
    }

    fn select(&mut self, line: &str) {
        self.selected.push_str(line);
        self.selected_count += 1;
    }

    fn finish(mut self) -> (String, usize, bool) {
        if matches!(self.limit, Some(ReadLimit::Tail(_))) {
            self.selected_count = self.tail.len();
            for line in self.tail {
                self.selected.push_str(&line);
            }
        }
        let truncated = self.selected_count != self.line_count;
        (self.selected, self.line_count, truncated)
    }
}
