//! HTTP route handlers grouped by concern (`web-architecture.md` §2).
//! `vault` (FR-220 to FR-225a) is this phase's scope; `apps` and
//! `settings` are Phase 16 and 17 respectively and are not part of this
//! module yet.

pub mod vault;
pub mod vault_mutations;
