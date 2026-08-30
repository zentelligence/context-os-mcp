//! Per-domain tool-router modules. Each declares its own
//! `#[tool_router(router = <name>)]` on an `impl ContextOsServer` block;
//! [`crate::server::ContextOsServer::effective_catalogue`] merges every
//! named router into the one dispatch table.

pub(crate) mod base_query;
pub(crate) mod diagnostics;
pub(crate) mod doctor;
pub(crate) mod ephemeris;
pub(crate) mod fs;
pub(crate) mod fs_types;
pub(crate) mod git;
pub(crate) mod index_status;
pub(crate) mod mermaid;
pub(crate) mod obsidian;
pub(crate) mod obsidian_types;
pub(crate) mod query;
pub(crate) mod vault;
