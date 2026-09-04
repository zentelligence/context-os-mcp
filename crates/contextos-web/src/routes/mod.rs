//! HTTP route handlers grouped by concern (`web-architecture.md` §2).
//! `vault` (FR-220 to FR-225a, Phase 15) and `apps` (FR-233 to FR-234,
//! Phase 16) are implemented; `settings` is Phase 17 and is not part of
//! this module yet.

pub mod apps;
pub mod vault;
pub mod vault_mutations;
