//! `contextos config vault` CLI support: structure- and comment-preserving
//! edits to an operator's `config.toml`, unlike `Config`'s existing serde
//! round trip (`config.rs`), which would discard `docs/config.example.toml`-
//! style hand commentary.
//!
//! Every mutating method re-parses its own rendered output through
//! [`Config::try_from::<&str>`] (the real schema validator) and
//! `VaultSet::try_from(&Config)` (the real duplicate-name, duplicate-root,
//! and vault-existence validator `ContextOsServer::try_from` itself
//! exercises at server startup) before returning success, rolling back to
//! the pre-mutation document on any failure. This is "reject malformed
//! input, never silently repair" applied to the file being written, not
//! just read: a caller never observes a document that would fail to load.

use std::path::{Path, PathBuf};

use contextos_core::VaultSet;
use thiserror::Error;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::{Config, ConfigError};

/// One configured vault as reported by [`ConfigDocument::vaults`]: its
/// resolved display name, path, managed flag, and semantic-search state,
/// read directly off the TOML structure rather than through `Config`'s
/// strict validator, so an empty or not-yet-valid document still reports an
/// accurate (possibly empty) list instead of surfacing `ConfigError::NoVaults`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSummary {
    pub name: String,
    pub path: PathBuf,
    pub managed: bool,
    pub semantic: bool,
    pub model_directory: Option<PathBuf>,
}

/// This document's `[server]` settings as reported by
/// [`ConfigDocument::server_settings`], with the same schema defaults
/// `ServerConfig::default` uses (`configuration.md`) filled in when the key
/// is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerSettingsSummary {
    pub transports: Vec<String>,
    pub log_level: String,
    pub log_file: String,
}

/// A `config.toml` document under edit, preserving comments and formatting
/// for everything this type's methods do not themselves touch.
#[derive(Clone, Debug)]
pub struct ConfigDocument {
    document: DocumentMut,
}

impl ConfigDocument {
    /// Starts a fresh, empty document (no existing `config.toml` on disk).
    #[must_use]
    pub fn new() -> Self {
        Self {
            document: DocumentMut::new(),
        }
    }

    /// Parses an existing `config.toml`'s text, preserving its comments and
    /// formatting for subsequent edits.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::Toml`] when `source` is not valid TOML.
    pub fn parse(source: &str) -> Result<Self, ConfigWriterError> {
        Ok(Self {
            document: source
                .parse::<DocumentMut>()
                .map_err(|source| ConfigWriterError::Toml {
                    source: Box::new(source),
                })?,
        })
    }

    /// Renders the current document back to `config.toml` text.
    #[must_use]
    pub fn render(&self) -> String {
        self.document.to_string()
    }

    /// Appends a new `[[vault]]` entry.
    ///
    /// `managed` is written explicitly only when `false`; the common case
    /// (`true`, `VaultConfig`'s own default) stays out of the generated
    /// text.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::NonUtf8Path`] when `path` is not valid
    /// UTF-8, since `config.toml` cannot represent a non-UTF-8 path.
    /// Returns [`ConfigWriterError::Invalid`] and leaves the document
    /// unchanged when the resulting configuration would be invalid: a
    /// duplicate vault name, a duplicate resolved root, or a `path` that
    /// does not resolve to an existing directory.
    pub fn add_vault(
        &mut self,
        name: &str,
        path: &Path,
        managed: bool,
    ) -> Result<(), ConfigWriterError> {
        let path = path
            .to_str()
            .ok_or_else(|| ConfigWriterError::NonUtf8Path {
                path: path.to_path_buf(),
            })?;
        let backup = self.document.clone();

        let array = self
            .document
            .entry("vault")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or(ConfigWriterError::VaultNotAnArrayOfTables)?;
        let mut table = Table::new();
        table.insert("path", value(path));
        table.insert("name", value(name));
        if !managed {
            table.insert("managed", value(false));
        }
        array.push(table);

        self.validate_or_rollback(backup)
    }

    /// Removes the `[[vault]]` entry named `name` (its explicit `name`, or
    /// its `path`'s basename when unset), compared case-insensitively to
    /// match `VaultRoot`'s own name comparison.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::UnknownVaultName`] when no configured
    /// vault matches `name`. Returns [`ConfigWriterError::Invalid`] and
    /// leaves the document unchanged when removing the match would leave no
    /// vault configured at all.
    pub fn remove_vault(&mut self, name: &str) -> Result<(), ConfigWriterError> {
        let target = name.to_ascii_lowercase();
        let backup = self.document.clone();

        let Some(array) = self
            .document
            .get_mut("vault")
            .and_then(Item::as_array_of_tables_mut)
        else {
            return Err(ConfigWriterError::UnknownVaultName {
                name: name.to_owned(),
            });
        };
        let before = array.len();
        array.retain(|table| {
            vault_display_name(table).map(|name| name.to_ascii_lowercase()) != Some(target.clone())
        });
        if array.len() == before {
            return Err(ConfigWriterError::UnknownVaultName {
                name: name.to_owned(),
            });
        }

        self.validate_or_rollback(backup)
    }

    /// Enables semantic search for the `[[vault]]` entry named `name`,
    /// setting `search.semantic = true` and
    /// `search.embedding.model_directory = model_directory` on that vault's
    /// table.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::NonUtf8Path`] when `model_directory` is
    /// not valid UTF-8. Returns [`ConfigWriterError::UnknownVaultName`] when
    /// no configured vault matches `name`. Returns
    /// [`ConfigWriterError::Invalid`] and leaves the document unchanged when
    /// the resulting configuration would be invalid.
    pub fn enable_semantic_search(
        &mut self,
        name: &str,
        model_directory: &Path,
    ) -> Result<(), ConfigWriterError> {
        let model_directory =
            model_directory
                .to_str()
                .ok_or_else(|| ConfigWriterError::NonUtf8Path {
                    path: model_directory.to_path_buf(),
                })?;
        let backup = self.document.clone();
        let table = self.find_vault_table_mut(name)?;

        let search = table
            .entry("search")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigWriterError::VaultKeyNotATable {
                key: "search".to_owned(),
            })?;
        search.insert("semantic", value(true));
        let embedding = search
            .entry("embedding")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigWriterError::VaultKeyNotATable {
                key: "search.embedding".to_owned(),
            })?;
        embedding.insert("model_directory", value(model_directory));

        self.validate_or_rollback(backup)
    }

    /// Renames, re-roots, and/or re-flags managed status on the `[[vault]]`
    /// entry currently named `current_name`, using the same validate-or-
    /// rollback discipline every other mutating method here uses.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::NonUtf8Path`] when `new_path` is not
    /// valid UTF-8. Returns [`ConfigWriterError::UnknownVaultName`] when no
    /// configured vault matches `current_name`. Returns
    /// [`ConfigWriterError::Invalid`] and leaves the document unchanged when
    /// the resulting configuration would be invalid.
    pub fn update_vault(
        &mut self,
        current_name: &str,
        new_name: &str,
        new_path: &Path,
        managed: bool,
    ) -> Result<(), ConfigWriterError> {
        let new_path_str = new_path
            .to_str()
            .ok_or_else(|| ConfigWriterError::NonUtf8Path {
                path: new_path.to_path_buf(),
            })?;
        let backup = self.document.clone();
        let table = self.find_vault_table_mut(current_name)?;

        table.insert("path", value(new_path_str));
        table.insert("name", value(new_name));
        if managed {
            table.remove("managed");
        } else {
            table.insert("managed", value(false));
        }

        self.validate_or_rollback(backup)
    }

    /// Disables semantic search for the `[[vault]]` entry named `name`,
    /// setting `search.semantic = false`. The embedding model directory, if
    /// any was previously recorded, is left in place so re-enabling later
    /// can offer it back as a default.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::UnknownVaultName`] when no configured
    /// vault matches `name`. Returns [`ConfigWriterError::Invalid`] and
    /// leaves the document unchanged when the resulting configuration would
    /// be invalid.
    pub fn disable_semantic_search(&mut self, name: &str) -> Result<(), ConfigWriterError> {
        let backup = self.document.clone();
        let table = self.find_vault_table_mut(name)?;
        if let Some(search) = table.get_mut("search").and_then(Item::as_table_mut) {
            search.insert("semantic", value(false));
        }
        self.validate_or_rollback(backup)
    }

    /// Every configured `[[vault]]` entry, in file order. See
    /// [`VaultSummary`] for why this bypasses `Config`'s strict validator.
    #[must_use]
    pub fn vaults(&self) -> Vec<VaultSummary> {
        self.document
            .get("vault")
            .and_then(Item::as_array_of_tables)
            .map(|array| {
                array
                    .iter()
                    .filter_map(|table| {
                        let path = table.get("path").and_then(Item::as_str)?;
                        let name = vault_display_name(table)?;
                        let managed = table.get("managed").and_then(Item::as_bool).unwrap_or(true);
                        let search = table.get("search").and_then(Item::as_table);
                        let semantic = search
                            .and_then(|search| search.get("semantic"))
                            .and_then(Item::as_bool)
                            .unwrap_or(false);
                        let model_directory = search
                            .and_then(|search| search.get("embedding"))
                            .and_then(Item::as_table)
                            .and_then(|embedding| embedding.get("model_directory"))
                            .and_then(Item::as_str)
                            .map(PathBuf::from);
                        Some(VaultSummary {
                            name,
                            path: PathBuf::from(path),
                            managed,
                            semantic,
                            model_directory,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// This document's `[server]` settings, with schema defaults filled in
    /// for any absent key.
    #[must_use]
    pub fn server_settings(&self) -> ServerSettingsSummary {
        let server = self.document.get("server").and_then(Item::as_table);
        let transports = server
            .and_then(|server| server.get("transports"))
            .and_then(Item::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .filter(|transports: &Vec<String>| !transports.is_empty())
            .unwrap_or_else(|| vec!["stdio".to_owned()]);
        let log_level = server
            .and_then(|server| server.get("log_level"))
            .and_then(Item::as_str)
            .unwrap_or("info")
            .to_owned();
        let log_file = server
            .and_then(|server| server.get("log_file"))
            .and_then(Item::as_str)
            .unwrap_or("")
            .to_owned();
        ServerSettingsSummary {
            transports,
            log_level,
            log_file,
        }
    }

    /// Sets this document's `[server]` settings. A value equal to the
    /// schema default is omitted from the written document (mirrors
    /// [`Self::add_vault`]'s own "only write `managed` when it deviates from
    /// the default" precedent), so accepting every prefilled default through
    /// an edit pass leaves the file unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigWriterError::ServerNotATable`] when `[server]` is
    /// present but not a table. Returns [`ConfigWriterError::Invalid`] and
    /// leaves the document unchanged when the resulting configuration would
    /// be invalid (for example an unrecognised `log_level`).
    pub fn set_server_settings(
        &mut self,
        transports: &[String],
        log_level: &str,
        log_file: &str,
    ) -> Result<(), ConfigWriterError> {
        let backup = self.document.clone();
        {
            let server = self
                .document
                .entry("server")
                .or_insert_with(|| Item::Table(Table::new()))
                .as_table_mut()
                .ok_or(ConfigWriterError::ServerNotATable)?;

            if transports == ["stdio"] {
                server.remove("transports");
            } else {
                let mut array = Array::new();
                for transport in transports {
                    array.push(transport.as_str());
                }
                server.insert("transports", value(array));
            }

            if log_level.eq_ignore_ascii_case("info") {
                server.remove("log_level");
            } else {
                server.insert("log_level", value(log_level));
            }

            if log_file.is_empty() {
                server.remove("log_file");
            } else {
                server.insert("log_file", value(log_file));
            }
        }
        // An empty `[server]` table still renders a header with nothing
        // under it; drop the key entirely once every field is back at its
        // default, so re-accepting every prefilled default is a true no-op.
        if self
            .document
            .get("server")
            .and_then(Item::as_table)
            .is_some_and(Table::is_empty)
        {
            self.document.remove("server");
        }

        self.validate_or_rollback(backup)
    }

    /// Locates the `[[vault]]` table currently named `name` (case-
    /// insensitively), shared by every mutating method that edits an
    /// existing vault entry in place rather than appending or removing one.
    fn find_vault_table_mut(&mut self, name: &str) -> Result<&mut Table, ConfigWriterError> {
        let target = name.to_ascii_lowercase();
        let array = self
            .document
            .get_mut("vault")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| ConfigWriterError::UnknownVaultName {
                name: name.to_owned(),
            })?;
        array
            .iter_mut()
            .find(|table| {
                vault_display_name(table).map(|name| name.to_ascii_lowercase())
                    == Some(target.clone())
            })
            .ok_or_else(|| ConfigWriterError::UnknownVaultName {
                name: name.to_owned(),
            })
    }

    fn validate_or_rollback(&mut self, backup: DocumentMut) -> Result<(), ConfigWriterError> {
        match render_and_validate(&self.document) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.document = backup;
                Err(error)
            }
        }
    }
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self::new()
    }
}

fn render_and_validate(document: &DocumentMut) -> Result<(), ConfigWriterError> {
    let text = document.to_string();
    let config =
        Config::try_from(text.as_str()).map_err(|source| ConfigWriterError::Invalid { source })?;
    VaultSet::try_from(&config).map_err(|source| ConfigWriterError::Invalid { source })?;
    Ok(())
}

/// The name a running server would resolve this `[[vault]]` table to: its
/// explicit `name`, or its `path`'s basename when unset, mirroring
/// `VaultRoot::try_from`'s own defaulting (`contextos-core::path`). Returned
/// in its original casing; callers needing `VaultSet`'s case-insensitive
/// comparison lowercase it themselves.
fn vault_display_name(table: &Table) -> Option<String> {
    if let Some(name) = table.get("name").and_then(Item::as_str) {
        return Some(name.to_owned());
    }
    table
        .get("path")
        .and_then(Item::as_str)
        .and_then(|path| Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
}

/// Typed failures editing a `config.toml` document.
#[derive(Debug, Error)]
pub enum ConfigWriterError {
    #[error("configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml_edit::TomlError>,
    },
    #[error("vault path is not valid UTF-8: {}", path.display())]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("the \"vault\" key is not an array of tables")]
    VaultNotAnArrayOfTables,
    #[error("the {key:?} key on this vault is not a table")]
    VaultKeyNotATable { key: String },
    #[error("the \"server\" key is not a table")]
    ServerNotATable,
    #[error("no vault named {name} is configured")]
    UnknownVaultName { name: String },
    #[error("the edited configuration is invalid")]
    Invalid {
        #[source]
        source: ConfigError,
    },
}

#[cfg(test)]
#[path = "config_writer_test.rs"]
mod tests;
