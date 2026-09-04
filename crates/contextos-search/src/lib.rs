// `unsafe_code` is `deny`, not `forbid`, only in this crate (see
// `Cargo.toml`'s `[lints]` comment): `vector_store::register_sqlite_vec`
// carries the one `unsafe` block approved by
// `phase-5-decision-addendum.md` A3, behind a narrow, documented
// `#[allow(unsafe_code)]`. Every other line in this crate remains subject
// to the deny, so a second `unsafe` occurrence anywhere else still fails
// the build.
#![deny(unsafe_code)]

mod chunk;
mod document;
mod embedding;
mod embedding_worker;
mod error;
mod fake_embedder;
#[cfg(feature = "semantic-local")]
mod fastembed_embedder;
mod graph;
mod openai_embedder;
mod service;
mod sync;
mod text;
mod vector_store;

pub use chunk::{Chunk, ChunkSource, chunk_document, estimate_tokens};
pub use document::{DocumentSource, IndexedDocument};
pub use embedding::{EmbeddingProviderConfig, EmbedsText};
pub use embedding_worker::{
    EmbeddingWorker, EmbeddingWorkerConfig, EmbeddingWorkerStatus, FilesystemChunkSource, PathEmbeddingOutcome,
    ReadsChunkSource,
};
pub use error::SearchError;
pub use fake_embedder::FakeEmbedder;
#[cfg(feature = "semantic-local")]
pub use fastembed_embedder::{FastembedLocal, REQUIRED_MODEL_FILES};
pub use graph::{
    CatchUpKind, GraphBackend, GraphDirection, GraphEdge, GraphEdgeKind, GraphNode, GraphView, LinkGraph,
    LinkGraphConfig, SyncStatus,
};
pub use openai_embedder::{OpenAiCompatible, OpenAiCompatibleConfig};
pub use service::{
    GraphIndexStatus, GraphRebuildReport, IndexStatusReport, RebuildProgress, RebuildReport, RebuildTarget,
    SemanticConfig, SemanticHit, SemanticIndexStatus, SemanticQuery, SemanticRebuildReport, TextIndexStatus,
    VaultSearchConfig, VaultSearchService,
};
pub use sync::{FreshnessReport, TextSearchService, TextSyncConfig, is_markdown};
pub use text::{IndexesText, TantivyIndex, TextHit, TextIndexConfig, TextQuery};
pub use vector_store::{
    SimilarityHit, SimilarityQuery, SqliteVecConfig, SqliteVecStore, StoresVectors, VectorRecord, VectorStoreStats,
};
