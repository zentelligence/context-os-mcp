//! Assembles [`NavData`](super::page::NavData) for the shared nav shell
//! (vault switcher, primary nav, current-directory listing) every full-page
//! response ([`super::page::render_page`]) is wrapped in.
//!
//! Two live MCP calls per full page: `vault_info` (the switcher's own
//! list) and, when the page has a current vault, `fs_list_directory`
//! scoped to the current directory only. A full recursive whole-vault tree
//! (the mock's literal, multi-section illustration) is deliberately not
//! built: an unconditional whole-vault enumeration does not scale, the
//! same reasoning `resources/list` already applies, and `fs_list_directory`
//! at one directory is the same bounded-cost operation the directory
//! route ([`crate::routes::vault`]) itself already relies on.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::page::{NavData, NavEntry, NavVault};
use crate::mcp_client::{McpCallError, McpClient};

/// Which primary-nav item is current, so [`build_nav`] can mark it active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveScreen {
    Vault,
    Apps,
    Settings,
}

#[derive(Debug, Deserialize)]
struct VaultInfoEntry {
    name: String,
}

#[derive(Debug, Deserialize)]
struct VaultInfoResult {
    vaults: Vec<VaultInfoEntry>,
}

async fn configured_vault_names(client: &McpClient) -> Result<Vec<String>, McpCallError> {
    let result = client.call_tool("vault_info".to_owned(), Map::new()).await?;
    let Ok(parsed) = result.into_typed::<VaultInfoResult>() else {
        return Ok(Vec::new());
    };
    Ok(parsed.vaults.into_iter().map(|entry| entry.name).collect())
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

/// Maximum nav-tree entries rendered for one directory: a bound, not a
/// pagination control, keeping the shell's own render cost and markup size
/// independent of how large a single directory happens to be.
const MAX_NAV_ENTRIES: usize = 50;

async fn directory_entries(
    client: &McpClient,
    vault_name: &str,
    directory_path: &str,
) -> Result<Vec<NavEntry>, McpCallError> {
    let target = if directory_path.is_empty() {
        format!("{vault_name}://.")
    } else {
        format!("{vault_name}://{directory_path}")
    };
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(target));
    let result = client.call_tool("fs_list_directory".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(Vec::new());
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Ok(Vec::new());
    };
    let mut dirs: Vec<DirEntryResult> = Vec::new();
    let mut files: Vec<DirEntryResult> = Vec::new();
    for entry in listing.entries {
        if entry.kind == "dir" {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let prefix = if directory_path.is_empty() {
        String::new()
    } else {
        format!("{directory_path}/")
    };
    Ok(dirs
        .into_iter()
        .chain(files)
        .take(MAX_NAV_ENTRIES)
        .map(|entry| {
            let is_dir = entry.kind == "dir";
            let href = if is_dir {
                format!("/{vault_name}/{prefix}{}/", entry.name)
            } else {
                format!("/{vault_name}/{prefix}{}", entry.name)
            };
            NavEntry {
                name: entry.name,
                href,
                is_dir,
            }
        })
        .collect())
}

fn breadcrumb_for(vault_name: Option<&str>, breadcrumb_suffix: Option<&str>) -> String {
    let Some(vault_name) = vault_name else {
        return "settings".to_owned();
    };
    let trimmed = breadcrumb_suffix.unwrap_or("").trim_matches('/');
    if trimmed.is_empty() {
        vault_name.to_owned()
    } else {
        format!("{vault_name} / {}", trimmed.replace('/', " / "))
    }
}

/// The directory a nav-tree fetch should be scoped to for `relative_path`:
/// `relative_path` itself when `is_directory`, otherwise its parent
/// (a file's own containing directory). Callers pass the result as
/// [`build_nav`]'s own `tree_directory` argument.
#[must_use]
pub fn directory_scope(relative_path: &str, is_directory: bool) -> String {
    let trimmed = relative_path.trim_matches('/');
    if is_directory {
        trimmed.to_owned()
    } else {
        trimmed
            .rsplit_once('/')
            .map_or(String::new(), |(dir, _)| dir.to_owned())
    }
}

/// Assembles [`NavData`] for one page render.
///
/// `vault_name` is `None` for a vault-independent page (`/settings/`);
/// `breadcrumb_suffix` and `tree_directory` are two deliberately separate
/// concerns, both `None` at the vault root: `breadcrumb_suffix` is the
/// caller-formatted path segment(s) shown after the vault name in the
/// breadcrumb (a file's own full path, or a fixed label like `"apps"`),
/// while `tree_directory` is the real vault directory the nav-tree section
/// is fetched from (a file's own *containing* directory, via
/// [`directory_scope`], never the file itself). Conflating the two would
/// show the wrong breadcrumb for a file (its containing directory instead
/// of its own path) or try to list a directory that does not exist (a
/// fixed breadcrumb label like `"apps"`).
///
/// Never fails outright: a transport failure degrades to an empty vault
/// list and/or empty nav-tree section (the chrome renders with less data
/// rather than the whole page failing over a nav-shell-only concern).
pub async fn build_nav(
    client: &McpClient,
    active: ActiveScreen,
    vault_name: Option<&str>,
    breadcrumb_suffix: Option<&str>,
    tree_directory: Option<&str>,
) -> NavData {
    let vault_names = configured_vault_names(client).await.unwrap_or_default();
    let vaults = vault_names
        .into_iter()
        .map(|name| {
            let is_current = vault_name == Some(name.as_str());
            NavVault { name, is_current }
        })
        .collect();

    let (directory_label, entries) = if let Some(vault_name) = vault_name {
        let directory_path = tree_directory.unwrap_or("").trim_matches('/');
        let label = if directory_path.is_empty() {
            format!("{vault_name} (root)")
        } else {
            directory_path.to_owned()
        };
        let entries = directory_entries(client, vault_name, directory_path)
            .await
            .unwrap_or_default();
        (Some(label), entries)
    } else {
        (None, Vec::new())
    };

    NavData {
        vaults,
        current_vault: vault_name.map(str::to_owned),
        directory_label,
        entries,
        breadcrumb: breadcrumb_for(vault_name, breadcrumb_suffix),
        active_vault_screen: active == ActiveScreen::Vault,
        active_apps_screen: active == ActiveScreen::Apps,
        active_settings_screen: active == ActiveScreen::Settings,
        rescan_href: vault_name.map(|name| format!("/{name}/apps/rescan")),
        // Callers that have a `web.toml` path (every one in this crate)
        // overwrite this with `config::current_appearance` immediately
        // after; `build_nav` itself has no filesystem access of its own
        // (it is an MCP-only assembler, `shell.rs`'s own module doc).
        appearance: super::page::Appearance::default(),
    }
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;
