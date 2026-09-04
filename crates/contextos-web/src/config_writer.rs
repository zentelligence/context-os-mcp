//! `web.toml` editing for `/settings/` (FR-251): structure- and comment-
//! preserving edits via `toml_edit`, mirroring `contextos-mcp`'s own
//! `contextos config` "validate-then-write discipline" `FR-251` names
//! directly as its precedent.
//!
//! Every mutating method re-parses its own rendered output through
//! [`WebConfig`]'s real schema validator ([`WebConfig::validate`], the same
//! check [`crate::config::load_web_config`] runs on startup) before
//! returning success, rolling back to the pre-mutation document on any
//! failure. A caller never observes, and this module never persists, a
//! document that would fail [`crate::config::load_web_config`] to re-load.
//!
//! This module knows nothing about HTTP or about registered apps;
//! [`crate::routes::settings`] is the thin adapter translating request
//! bodies to these methods, running the registered-app-dependency check
//! these methods cannot themselves perform (they have no MCP session), and
//! translating results back to HTTP responses.

use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value as TomlValue};

use crate::config::{WebConfig, WebConfigError};

/// A `web.toml` document under edit, preserving comments and formatting for
/// everything this type's methods do not themselves touch.
#[derive(Clone, Debug)]
pub struct WebConfigDocument {
    document: DocumentMut,
}

impl WebConfigDocument {
    /// Parses an existing `web.toml`'s text, preserving its comments and
    /// formatting for subsequent edits.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::Toml`] when `source` is not valid
    /// TOML (not schema-checked here; a structurally valid but
    /// schema-invalid document, for example one carrying an already
    /// malformed `server.bind`, parses successfully and is caught by the
    /// next mutating call's own validation pass instead).
    pub fn parse(source: &str) -> Result<Self, WebConfigWriterError> {
        Ok(Self {
            document: source.parse::<DocumentMut>().map_err(|source| {
                WebConfigWriterError::Toml {
                    source: Box::new(source),
                }
            })?,
        })
    }

    /// Renders the current document back to `web.toml` text.
    #[must_use]
    pub fn render(&self) -> String {
        self.document.to_string()
    }

    /// Every configured `[[mcp_server]]` name, in file order. The settings
    /// route diffs this before and after an edit to learn which names, if
    /// any, the edit removed: the trigger for FR-251's registered-app-
    /// dependency check.
    #[must_use]
    pub fn mcp_server_names(&self) -> Vec<String> {
        self.document
            .get("mcp_server")
            .and_then(Item::as_array_of_tables)
            .map(|array| array.iter().filter_map(mcp_server_name).collect())
            .unwrap_or_default()
    }

    /// Appends a new `[[mcp_server]]` entry (`POST /settings/`). `entry`'s
    /// keys and value shapes are otherwise unconstrained here: the real
    /// constraint is [`WebConfig`]'s own schema (`transport`, `name`, and
    /// the fields each transport requires), enforced by re-parsing the
    /// rendered document below, the same schema `web.toml`'s own TOML form
    /// is held to.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnsupportedValue`] when `entry`
    /// contains a JSON value this module cannot represent in TOML (`null`,
    /// or an array containing anything but a scalar). Returns
    /// [`WebConfigWriterError::Invalid`] and leaves the document unchanged
    /// when the resulting configuration would be invalid (a duplicate
    /// name, an unrecognised `transport`, a missing required field, or a
    /// pre-existing invalid `server.bind` this edit did not itself
    /// introduce).
    pub fn add_mcp_server(
        &mut self,
        entry: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = json_object_to_table(entry)?;
        let array = self
            .document
            .entry("mcp_server")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or(WebConfigWriterError::McpServerNotAnArrayOfTables)?;
        array.push(table);
        self.validate_or_rollback(backup)
    }

    /// Merges `patch`'s keys into the `[[mcp_server]]` entry currently named
    /// `name` (`PATCH /settings/`, `target: "mcp_server"`), leaving every
    /// other key on that entry untouched.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `name`. Returns
    /// [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents.
    pub fn patch_mcp_server(
        &mut self,
        name: &str,
        patch: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = self.find_mcp_server_table_mut(name)?;
        merge_patch(table, patch)?;
        self.validate_or_rollback(backup)
    }

    /// Replaces the `[[mcp_server]]` entry currently named `current_name`
    /// with `entry` in full (`PUT /settings/`).
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `current_name`. Returns
    /// [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents.
    pub fn replace_mcp_server(
        &mut self,
        current_name: &str,
        entry: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = json_object_to_table(entry)?;
        let index = self.find_mcp_server_index(current_name)?;
        // `find_mcp_server_index` already proved the array exists; this
        // second lookup only re-borrows it mutably (`toml_edit`'s immutable
        // and mutable array accessors are separate methods).
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: current_name.to_owned(),
            })?;
        array.replace(index, table);
        self.validate_or_rollback(backup)
    }

    /// Removes the `[[mcp_server]]` entry named `name` (`DELETE
    /// /settings/`).
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `name`.
    pub fn remove_mcp_server(&mut self, name: &str) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let target = name.to_ascii_lowercase();
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: name.to_owned(),
            })?;
        let before = array.len();
        array.retain(|table| {
            mcp_server_name(table).map(|name| name.to_ascii_lowercase()) != Some(target.clone())
        });
        if array.len() == before {
            return Err(WebConfigWriterError::UnknownMcpServerName {
                name: name.to_owned(),
            });
        }
        self.validate_or_rollback(backup)
    }

    /// Merges `patch`'s keys into `[server.ui]` (`PATCH /settings/`,
    /// `target: "ui"`); `[server.ui]`'s own key set is intentionally
    /// unenumerated (`config.rs`: "a rendering/theme concern deferred to
    /// web-rendering.md"), so any JSON-representable value is accepted
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents. Returns [`WebConfigWriterError::ServerNotATable`] or
    /// [`WebConfigWriterError::UiNotATable`] when `[server]`/`[server.ui]`
    /// is present in the document but is not itself a table (a
    /// hand-corrupted `web.toml`).
    pub fn patch_ui(
        &mut self,
        patch: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let server = self
            .document
            .entry("server")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or(WebConfigWriterError::ServerNotATable)?;
        let ui = server
            .entry("ui")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or(WebConfigWriterError::UiNotATable)?;
        merge_patch(ui, patch)?;
        self.validate_or_rollback(backup)
    }

    fn find_mcp_server_index(&self, name: &str) -> Result<usize, WebConfigWriterError> {
        let target = name.to_ascii_lowercase();
        self.document
            .get("mcp_server")
            .and_then(Item::as_array_of_tables)
            .and_then(|array| {
                array.iter().position(|table| {
                    mcp_server_name(table).map(|found| found.to_ascii_lowercase())
                        == Some(target.clone())
                })
            })
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: name.to_owned(),
            })
    }

    fn find_mcp_server_table_mut(
        &mut self,
        name: &str,
    ) -> Result<&mut Table, WebConfigWriterError> {
        let target = name.to_ascii_lowercase();
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: name.to_owned(),
            })?;
        array
            .iter_mut()
            .find(|table| {
                mcp_server_name(table).map(|found| found.to_ascii_lowercase())
                    == Some(target.clone())
            })
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: name.to_owned(),
            })
    }

    fn validate_or_rollback(&mut self, backup: DocumentMut) -> Result<(), WebConfigWriterError> {
        match render_and_validate(&self.document) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.document = backup;
                Err(error)
            }
        }
    }
}

/// Re-parses `document`'s rendered text through [`WebConfig`]'s own schema
/// deserializer and [`WebConfig::validate`], the identical pair
/// [`crate::config::load_web_config`] runs on startup: a document that
/// passes here is guaranteed loadable, never a divergent, writer-only
/// notion of validity.
fn render_and_validate(document: &DocumentMut) -> Result<(), WebConfigWriterError> {
    let text = document.to_string();
    let config: WebConfig =
        toml::from_str(&text).map_err(|source| WebConfigWriterError::Invalid {
            source: WebConfigError::Toml {
                source: Box::new(source),
            },
        })?;
    config
        .validate()
        .map_err(|source| WebConfigWriterError::Invalid { source })
}

/// The `name` field on one `[[mcp_server]]` table, if present and a string.
fn mcp_server_name(table: &Table) -> Option<String> {
    table.get("name").and_then(Item::as_str).map(str::to_owned)
}

fn json_object_to_table(map: &JsonMap<String, JsonValue>) -> Result<Table, WebConfigWriterError> {
    let mut table = Table::new();
    for (key, value) in map {
        table.insert(key, json_to_toml_item(value)?);
    }
    Ok(table)
}

/// Converts one submitted JSON value into the `toml_edit` item it
/// represents. `null` and a non-scalar array element have no TOML
/// representation this endpoint accepts and are rejected rather than
/// silently coerced (`rust-quality.md`: "reject malformed or unknown
/// input").
fn json_to_toml_item(value: &JsonValue) -> Result<Item, WebConfigWriterError> {
    match value {
        JsonValue::Null => Err(WebConfigWriterError::UnsupportedValue),
        JsonValue::Bool(flag) => Ok(Item::Value(TomlValue::from(*flag))),
        JsonValue::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(Item::Value(TomlValue::from(int)))
            } else if let Some(float) = number.as_f64() {
                Ok(Item::Value(TomlValue::from(float)))
            } else {
                Err(WebConfigWriterError::UnsupportedValue)
            }
        }
        JsonValue::String(text) => Ok(Item::Value(TomlValue::from(text.clone()))),
        JsonValue::Array(items) => {
            let mut array = Array::new();
            for entry in items {
                let Item::Value(scalar) = json_to_toml_item(entry)? else {
                    return Err(WebConfigWriterError::UnsupportedValue);
                };
                array.push(scalar);
            }
            Ok(Item::Value(TomlValue::Array(array)))
        }
        JsonValue::Object(nested) => Ok(Item::Table(json_object_to_table(nested)?)),
    }
}

fn merge_patch(
    table: &mut Table,
    patch: &JsonMap<String, JsonValue>,
) -> Result<(), WebConfigWriterError> {
    for (key, value) in patch {
        table.insert(key, json_to_toml_item(value)?);
    }
    Ok(())
}

/// Typed failures editing a `web.toml` document.
#[derive(Debug, Error)]
pub enum WebConfigWriterError {
    #[error("web configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml_edit::TomlError>,
    },
    #[error("the \"mcp_server\" key is not an array of tables")]
    McpServerNotAnArrayOfTables,
    #[error("the \"server\" key is not a table")]
    ServerNotATable,
    #[error("the \"server.ui\" key is not a table")]
    UiNotATable,
    #[error("no [[mcp_server]] entry named {name:?} is configured")]
    UnknownMcpServerName { name: String },
    #[error(
        "a submitted value cannot be represented in TOML (null, or a nested array containing \
         anything but a plain string, number, or boolean)"
    )]
    UnsupportedValue,
    #[error("the edited configuration is invalid")]
    Invalid {
        #[source]
        source: WebConfigError,
    },
}

#[cfg(test)]
#[path = "config_writer_test.rs"]
mod tests;
