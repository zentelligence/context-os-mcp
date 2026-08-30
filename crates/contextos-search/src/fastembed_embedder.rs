//! `FastembedLocal`: an [`EmbedsText`] provider running local ONNX
//! inference via fastembed's user-defined model API, behind the
//! default-on `semantic-local` Cargo feature
//! (phase-5-decision-addendum.md A1).
//!
//! This module only ever calls
//! [`fastembed::TextEmbedding::try_new_from_user_defined`], never
//! `TextEmbedding::try_new` (fastembed's `hf-hub`-backed auto-download
//! constructor). `semantic-local` does not enable fastembed's `hf-hub`
//! feature at all, so that constructor is not even compiled into this
//! build: there is no code path here that can trigger a model download.
//! The model directory is always supplied by the caller (platform app-data,
//! per the addendum); this module never derives it from the current
//! working directory or the operator's home directory.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::{Chunk, EmbedsText, SearchError};

/// Expected file names inside an injected model directory, mirroring the
/// Hugging Face repository layout for a sentence-transformers model (the
/// addendum's default is `sentence-transformers/all-MiniLM-L6-v2`). Later
/// pre-fetch tooling is responsible for populating a directory with
/// exactly these files.
const ONNX_FILE: &str = "model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const CONFIG_FILE: &str = "config.json";
const SPECIAL_TOKENS_FILE: &str = "special_tokens_map.json";
const TOKENIZER_CONFIG_FILE: &str = "tokenizer_config.json";

/// The exact file names a `model_directory` must contain for
/// [`FastembedLocal::try_from`] to succeed, in no particular order. Exposed
/// so tooling that populates a `model_directory` (for example a model-fetch
/// CLI command) can name the files it writes without duplicating this list.
pub const REQUIRED_MODEL_FILES: [&str; 5] = [
    ONNX_FILE,
    TOKENIZER_FILE,
    CONFIG_FILE,
    SPECIAL_TOKENS_FILE,
    TOKENIZER_CONFIG_FILE,
];

/// A short, fixed probe text embedded once at construction solely to learn
/// the model's output dimension. This is one local, CPU-only inference
/// call, never network I/O, so it never weakens the offline guarantee; it
/// exists so [`EmbedsText::dimension`] is available immediately rather than
/// only after the caller's first real [`EmbedsText::embed`] call.
const DIMENSION_PROBE_TEXT: &str = "contextos-dimension-probe";

/// Local ONNX embedding provider.
pub struct FastembedLocal {
    model: Mutex<TextEmbedding>,
    dimension: usize,
    directory: String,
}

impl std::fmt::Debug for FastembedLocal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FastembedLocal")
            .field("directory", &self.directory)
            .field("dimension", &self.dimension)
            .finish_non_exhaustive()
    }
}

impl TryFrom<PathBuf> for FastembedLocal {
    type Error = SearchError;

    /// Loads the model from the injected `model_directory`: one ONNX file
    /// and four tokenizer files, all read fully into memory and handed to
    /// fastembed's user-defined model API. Never downloads anything; a
    /// missing or invalid file is a typed error, not a fetch attempt.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingModelUnavailable`] when any required
    /// file under `model_directory` is missing or unreadable, or when the
    /// files present do not load as a valid ONNX model or tokenizer.
    fn try_from(model_directory: PathBuf) -> Result<Self, Self::Error> {
        let directory = model_directory.display().to_string();
        let onnx_file = read_required(&model_directory, ONNX_FILE)?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_required(&model_directory, TOKENIZER_FILE)?,
            config_file: read_required(&model_directory, CONFIG_FILE)?,
            special_tokens_map_file: read_required(&model_directory, SPECIAL_TOKENS_FILE)?,
            tokenizer_config_file: read_required(&model_directory, TOKENIZER_CONFIG_FILE)?,
        };

        let user_defined_model =
            UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files).with_pooling(Pooling::Mean);

        let mut model = TextEmbedding::try_new_from_user_defined(
            user_defined_model,
            InitOptionsUserDefined::default(),
        )
        .map_err(|source| model_error(&directory, &source))?;

        let probe = model
            .embed(vec![DIMENSION_PROBE_TEXT], None)
            .map_err(|source| model_error(&directory, &source))?;
        let dimension = probe.first().map_or(0, Vec::len);

        Ok(Self {
            model: Mutex::new(model),
            dimension,
            directory,
        })
    }
}

impl EmbedsText for FastembedLocal {
    /// # Errors
    ///
    /// Returns [`SearchError::EmbeddingModelUnavailable`] when the loaded
    /// model fails to run inference over the given chunks.
    fn embed(&self, chunks: &[Chunk]) -> Result<Vec<Vec<f32>>, SearchError> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let texts: Vec<&str> = chunks.iter().map(Chunk::text).collect();
        let mut model = self
            .model
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        model
            .embed(texts, None)
            .map_err(|source| model_error(&self.directory, &source))
    }

    fn dimension(&self) -> Option<usize> {
        Some(self.dimension)
    }
}

fn read_required(directory: &Path, name: &str) -> Result<Vec<u8>, SearchError> {
    let path = directory.join(name);
    std::fs::read(&path).map_err(|source| SearchError::EmbeddingModelUnavailable {
        directory: directory.display().to_string(),
        reason: format!("{name} could not be read: {source}"),
    })
}

fn model_error(directory: &str, source: &fastembed::Error) -> SearchError {
    SearchError::EmbeddingModelUnavailable {
        directory: directory.to_owned(),
        reason: format!("{source:#}"),
    }
}
