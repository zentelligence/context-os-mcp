//! HTTP route handlers grouped by concern (`web-architecture.md` §2):
//! `vault` (vault content rendering), `apps` (registered app serving), and
//! `settings` (the `/settings/` configuration UI).

pub mod apps;
pub mod settings;
pub mod vault;
pub mod vault_mutations;
