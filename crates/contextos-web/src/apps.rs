//! App registration and manifest parsing (`web-apps.md`, FR-230 to FR-232):
//! `registry/apps/{{slug}}/manifest.toml` discovery and validation.
//!
//! Every step here is an MCP tool call against an already-connected
//! [`McpClient`] (`fs_list_directory`, `fs_read_text_file`,
//! `fs_get_file_info`), never a direct filesystem read (FR-201, D-W03):
//! app manifests live inside a vault, so discovering them is a vault
//! operation like any other.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};

/// `manifest.toml`'s `kind` field (`web-apps.md` §2). `Htmx` is accepted by
/// the schema now but not served until stage 2 (`FR-233a`, `D-W06`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    Spa,
    Htmx,
}

/// `manifest.toml`'s `target` field (`web-apps.md` §2).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum AppTarget {
    #[serde(rename = "_blank")]
    Blank,
    #[serde(rename = "embed")]
    Embed,
}

/// Whether `contextos-web` v1 actually serves a registered app (`FR-233`)
/// or merely lists it (`FR-233a`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppStatus {
    Supported,
    NotYetSupported,
}

/// One successfully validated app, ready to list (`FR-234`) and, if
/// [`AppStatus::Supported`], serve (`FR-233`).
#[derive(Clone, Debug)]
pub struct RegisteredApp {
    pub slug: String,
    pub name: String,
    pub kind: AppKind,
    pub entry: String,
    pub target: AppTarget,
    pub mcp_servers: Vec<String>,
    pub status: AppStatus,
}

/// `manifest.toml`'s schema (`web-apps.md` §2). `deny_unknown_fields`: an
/// unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    kind: AppKind,
    entry: String,
    target: AppTarget,
    #[serde(default)]
    mcp_servers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DirEntryResult {
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ListDirectoryResult {
    entries: Vec<DirEntryResult>,
}

#[derive(Debug, Deserialize)]
struct ReadTextResult {
    content: String,
}

/// Checks whether `vault_name` is a configured vault at all, distinct from
/// "configured but has no `registry/apps/` yet": callers 404 on `false`
/// rather than reporting an empty app list for a vault that does not exist
/// (`web-routes.md` §2 item 1's precedent for vault content routes).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn vault_exists(client: &McpClient, vault_name: &str) -> Result<bool, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://.")),
    );
    let result = client
        .call_tool("fs_get_file_info".to_owned(), args)
        .await?;
    Ok(result.is_error != Some(true))
}

/// Discovers and validates every app under `vault_name`'s
/// `registry/apps/` (`FR-230` to `FR-232`).
///
/// A directory with no `manifest.toml`, a manifest failing schema
/// validation, or one naming an `mcp_servers` entry absent from
/// `known_mcp_servers` fails registration for that app specifically (a
/// `tracing::warn!` diagnostic naming the app and the violation) without
/// affecting any other app's registration (`FR-232`). `registry/apps/`
/// itself not existing yet is not an error: it simply yields no apps.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails (a directory or manifest simply not existing is not this: it is
/// reported as an MCP-level tool error, handled internally as "skip this
/// entry").
pub async fn discover_apps(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Result<Vec<RegisteredApp>, McpCallError> {
    let dirs = list_app_directories(client, vault_name).await?;
    let mut apps = Vec::with_capacity(dirs.len());
    let mut seen_slugs = std::collections::HashSet::new();
    for dir in dirs {
        match load_app(client, vault_name, &dir, known_mcp_servers).await? {
            LoadOutcome::NoManifest => {}
            LoadOutcome::Invalid(reason) => {
                tracing::warn!(vault = vault_name, dir = %dir, reason = %reason, "app registration failed");
            }
            LoadOutcome::Valid(app) => {
                if seen_slugs.insert(app.slug.clone()) {
                    apps.push(app);
                } else {
                    tracing::warn!(
                        vault = vault_name,
                        dir = %dir,
                        slug = %app.slug,
                        "app registration failed: duplicate slug within this vault"
                    );
                }
            }
        }
    }
    apps.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(apps)
}

async fn list_app_directories(
    client: &McpClient,
    vault_name: &str,
) -> Result<Vec<String>, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://registry/apps")),
    );
    let result = client
        .call_tool("fs_list_directory".to_owned(), args)
        .await?;
    if result.is_error == Some(true) {
        return Ok(Vec::new());
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Ok(Vec::new());
    };
    Ok(listing
        .entries
        .into_iter()
        .filter(|entry| entry.kind == "dir")
        .map(|entry| entry.name)
        .collect())
}

enum LoadOutcome {
    NoManifest,
    Invalid(String),
    Valid(RegisteredApp),
}

async fn load_app(
    client: &McpClient,
    vault_name: &str,
    dir_name: &str,
    known_mcp_servers: &[String],
) -> Result<LoadOutcome, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!(
            "{vault_name}://registry/apps/{dir_name}/manifest.toml"
        )),
    );
    let result = client
        .call_tool("fs_read_text_file".to_owned(), args)
        .await?;
    if result.is_error == Some(true) {
        return Ok(LoadOutcome::NoManifest);
    }
    let Ok(read) = result.into_typed::<ReadTextResult>() else {
        return Ok(LoadOutcome::Invalid(
            "manifest.toml could not be read as text".to_owned(),
        ));
    };
    let raw: ManifestRaw = match toml::from_str(&read.content) {
        Ok(raw) => raw,
        Err(source) => {
            return Ok(LoadOutcome::Invalid(format!(
                "manifest.toml does not match the required schema: {source}"
            )));
        }
    };
    if raw.name.trim().is_empty() {
        return Ok(LoadOutcome::Invalid("name must not be empty".to_owned()));
    }
    let slug = match raw.slug {
        Some(explicit) if is_url_safe_slug(&explicit) => explicit,
        Some(explicit) => {
            return Ok(LoadOutcome::Invalid(format!(
                "slug {explicit:?} is not URL-safe (expected [a-z0-9-]+)"
            )));
        }
        None => dir_name.to_owned(),
    };
    for server in &raw.mcp_servers {
        if !known_mcp_servers.iter().any(|name| name == server) {
            return Ok(LoadOutcome::Invalid(format!(
                "mcp_servers entry {server:?} is not configured in web.toml"
            )));
        }
    }
    if !entry_exists(client, vault_name, dir_name, &raw.entry).await? {
        return Ok(LoadOutcome::Invalid(format!(
            "entry file {:?} does not exist",
            raw.entry
        )));
    }
    let status = match raw.kind {
        AppKind::Spa => AppStatus::Supported,
        AppKind::Htmx => AppStatus::NotYetSupported,
    };
    Ok(LoadOutcome::Valid(RegisteredApp {
        slug,
        name: raw.name,
        kind: raw.kind,
        entry: raw.entry,
        target: raw.target,
        mcp_servers: raw.mcp_servers,
        status,
    }))
}

async fn entry_exists(
    client: &McpClient,
    vault_name: &str,
    dir_name: &str,
    entry: &str,
) -> Result<bool, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://registry/apps/{dir_name}/{entry}")),
    );
    let result = client
        .call_tool("fs_get_file_info".to_owned(), args)
        .await?;
    Ok(result.is_error != Some(true))
}

fn is_url_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
