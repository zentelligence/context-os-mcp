//! Assembles [`NavData`](super::page::NavData) for the shared nav shell
//! (vault switcher, primary nav, current-directory listing) every full-page
//! response ([`super::page::render_page`]) is wrapped in.
//!
//! Up to three live MCP calls per full page: `vault_info` (the switcher's
//! own list) and, when the page has a current vault, the vault root's own
//! `.gitignore` (if present, `fs_read_text_file`) and `fs_list_directory`
//! scoped to the current directory only. A full recursive whole-vault tree
//! (the mock's literal, multi-section illustration) is deliberately not
//! built: an unconditional whole-vault enumeration does not scale, the
//! same reasoning `resources/list` already applies, and `fs_list_directory`
//! at one directory is the same bounded-cost operation the directory
//! route ([`crate::routes::vault`]) itself already relies on.
//!
//! The nav tree lists sub-directories only ([`directory_entries`]):
//! clicking a file link there would otherwise leave the shell entirely for
//! any extension without its own rendering pipeline (`render_other_file`
//! serves raw bytes, `crate::routes::vault`), so files never appear in it
//! at all; a file is reached through its containing directory's own
//! rendered content instead. Dotfiles/dot-directories and anything the
//! vault root's own `.gitignore` matches are excluded the same way.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::page::{BreadcrumbSegment, NavData, NavEntry, NavVault};
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

/// `pub(crate)` (not just this module's own [`build_nav`] use): also the
/// bare HTTP server root's own redirect target lookup
/// ([`crate::routes::vault::get_server_root`]), which needs the identical
/// "every configured vault's own `name`, in `vault_info` order" list and
/// should not maintain a second implementation of it.
pub(crate) async fn configured_vault_names(client: &McpClient) -> Result<Vec<String>, McpCallError> {
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

#[derive(Debug, Deserialize)]
struct ReadTextResult {
    content: String,
}

/// The vault root's own `.gitignore`, compiled into a matcher, or `None`
/// when the file is absent, unreadable, or contains no usable pattern: the
/// nav tree degrades to showing everything rather than the whole shell
/// failing over this convenience filter. Read fresh on every call (there is
/// no per-request nav-shell cache anywhere else in this module either) so
/// an edit to `.gitignore` takes effect on the very next page render.
async fn root_gitignore(client: &McpClient, vault_name: &str) -> Option<Gitignore> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://.gitignore")));
    let result = client.call_tool("fs_read_text_file".to_owned(), args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let content = result.into_typed::<ReadTextResult>().ok()?.content;
    let mut builder = GitignoreBuilder::new(".");
    for line in content.lines() {
        // A malformed individual line is skipped rather than discarding
        // every other pattern in an otherwise usable `.gitignore`.
        let _ = builder.add_line(None, line);
    }
    builder.build().ok()
}

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
    let prefix = if directory_path.is_empty() {
        String::new()
    } else {
        format!("{directory_path}/")
    };
    let gitignore = root_gitignore(client, vault_name).await;
    let mut dirs: Vec<DirEntryResult> = listing
        .entries
        .into_iter()
        // The nav tree lists sub-directories only (module doc): a file
        // link there would otherwise leave the page shell entirely for any
        // extension `crate::routes::vault` does not have a dedicated
        // rendering pipeline for.
        .filter(|entry| entry.kind == "dir")
        .filter(|entry| !entry.name.starts_with('.'))
        .filter(|entry| {
            gitignore.as_ref().is_none_or(|matcher| {
                !matcher
                    .matched_path_or_any_parents(format!("{prefix}{}", entry.name), true)
                    .is_ignore()
            })
        })
        .collect();
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(dirs
        .into_iter()
        .take(MAX_NAV_ENTRIES)
        .map(|entry| {
            let href = format!("/{vault_name}/{prefix}{}/", entry.name);
            NavEntry {
                name: entry.name,
                href,
                is_dir: true,
            }
        })
        .collect())
}

/// Builds a clickable ancestor trail for `path` (vault-relative, already
/// trimmed of leading/trailing `/`; possibly empty for the vault root
/// itself), rooted at `vault_name`: every segment carries a link to that
/// ancestor's own directory route except the last, which is always the
/// current page or directory itself and is never a link back to where the
/// reader already is. Shared by [`NavData::breadcrumb`](super::page::NavData)
/// (the top bar, `path` a file's own full path, a directory's own path, or
/// a fixed label like `"apps"`) and
/// [`NavData::directory_breadcrumb`](super::page::NavData) (the nav tree
/// section's own heading, `path` always a real directory): both are "a
/// trail of ancestors down to where we are now," differing only in what
/// that final unlinked segment names.
#[must_use]
pub(crate) fn breadcrumb_segments(vault_name: &str, path: &str) -> Vec<BreadcrumbSegment> {
    let mut segments = vec![BreadcrumbSegment {
        label: vault_name.to_owned(),
        href: if path.is_empty() {
            None
        } else {
            Some(format!("/{vault_name}/"))
        },
    }];
    if path.is_empty() {
        return segments;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let mut accumulated = String::new();
    for (index, part) in parts.iter().enumerate() {
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(part);
        let is_last = index == parts.len() - 1;
        segments.push(BreadcrumbSegment {
            label: (*part).to_owned(),
            href: if is_last {
                None
            } else {
                Some(format!("/{vault_name}/{accumulated}/"))
            },
        });
    }
    segments
}

/// The vault-independent breadcrumb (`/settings/`): a single unlinked
/// `"settings"` segment, matching every other page's own final,
/// current-location segment carrying no link.
fn settings_breadcrumb() -> Vec<BreadcrumbSegment> {
    vec![BreadcrumbSegment {
        label: "settings".to_owned(),
        href: None,
    }]
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
    let vaults: Vec<NavVault> = vault_names
        .into_iter()
        .map(|name| {
            let is_current = vault_name == Some(name.as_str());
            NavVault { name, is_current }
        })
        .collect();
    let nav_target_vault = vault_name
        .map(str::to_owned)
        .or_else(|| vaults.first().map(|entry| entry.name.clone()));

    let (directory_breadcrumb, entries) = if let Some(vault_name) = vault_name {
        let directory_path = tree_directory.unwrap_or("").trim_matches('/');
        let breadcrumb = breadcrumb_segments(vault_name, directory_path);
        let entries = directory_entries(client, vault_name, directory_path)
            .await
            .unwrap_or_default();
        (Some(breadcrumb), entries)
    } else {
        (None, Vec::new())
    };
    let breadcrumb = vault_name.map_or_else(settings_breadcrumb, |name| {
        let trimmed = breadcrumb_suffix.unwrap_or("").trim_matches('/');
        breadcrumb_segments(name, trimmed)
    });

    NavData {
        vaults,
        current_vault: vault_name.map(str::to_owned),
        nav_target_vault,
        directory_breadcrumb,
        entries,
        breadcrumb,
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
