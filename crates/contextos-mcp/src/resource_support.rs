//! Shared support for every surface that addresses a vault file as an MCP
//! resource: the [`resources`](crate::resources) capability itself
//! (`resources/list`/`resources/read`) and `fs_attach_file`
//! (`tools::fs`), which both need to build or resolve the same
//! `{name}://{relative-path}` URI. Also home to two small cross-cutting schema helpers
//! no single tool module owns: `output_schema_for`, for a tool handler
//! returning `CallToolResult` directly (rather than `Json<T>`) that still
//! needs to keep its advertised output schema; and `optional_u64_schema`,
//! for an `Option<u64>` input field that needs to advertise a plain
//! `"type": "integer"` schema rather than schemars' nullable array form.

use contextos_core::{PathError, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::FsError;
use rmcp::model::ContentBlock;
use rmcp::{ErrorData, schemars};
use thiserror::Error;

/// A typed failure while listing or reading a resource, rendered through
/// [`ResourceError::into_error_data`] into the one JSON-RPC error shape
/// `ServerHandler::list_resources`/`read_resource` can return (there is no
/// `CallToolResult`-style structured envelope for resources).
#[derive(Debug, Error)]
pub(crate) enum ResourceError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error("{path:?} could not be represented as a resource path")]
    InvalidPath { path: std::path::PathBuf },
}

impl ResourceError {
    /// Reports whether this resource exists but cannot be served as
    /// requested (invalid request shape, `-32602`) versus genuinely not
    /// existing or not being reachable from any configured root (`-32002`):
    /// the two JSON-RPC codes a client can actually switch on, distinct from
    /// the finer-grained `code` this method also embeds in `data`.
    fn is_invalid_request(&self) -> bool {
        matches!(self, Self::Filesystem(FsError::TooLarge { .. }))
    }

    /// Stable code and remediation text for this failure, shared by
    /// [`Self::into_error_data`] (the `resources/list`/`resources/read`
    /// JSON-RPC error shape) and `ToolError::Resource`'s conversion into
    /// [`crate::tool_error::ToolFailure`] (the `vault_info` tool-call error
    /// shape): one place describing what each variant means, regardless of
    /// which surface reports it.
    pub(crate) fn code_and_remediation(&self) -> (&str, &str) {
        match self {
            Self::InvalidPath { .. } => (
                "resource/invalid-uri",
                "Use a {name}://{relative-path} URI previously returned by resources/list.",
            ),
            Self::Path(error) => (error.code(), error.remediation()),
            Self::Filesystem(FsError::TooLarge { .. }) => (
                "io/too-large",
                "The file exceeds the vault's configured read limit; read it with \
                 fs_read_text_file using an explicit head, tail, or line range instead.",
            ),
            Self::Filesystem(error) => (error.code(), error.remediation()),
        }
    }

    pub(crate) fn into_error_data(self) -> ErrorData {
        let (code, remediation) = self.code_and_remediation();
        let message = self.to_string();
        let data = Some(serde_json::json!({ "code": code, "remediation": remediation }));
        if self.is_invalid_request() {
            ErrorData::invalid_params(message, data)
        } else {
            ErrorData::resource_not_found(message, data)
        }
    }
}

/// Builds a `{name}://{relative-path}` URI from an already-known vault name
/// and a root-relative path, for a caller that already has both
/// in hand and does not need to re-derive them from a `VaultPath`.
/// `resources.rs`'s listing walk is exactly this case: it already holds a
/// `VaultRoot` and a search-result relative path per iteration, so building
/// a fresh `VaultPath` just to call [`resource_uri_for`] would mean
/// re-validating and re-resolving a path this server just walked.
/// Deliberately not percent-encoded: this is the same string convention
/// accepted as a `path`-parameter prefix, and the two are meant to
/// address a file identically rather than diverge on escaping rules for one
/// form.
pub(crate) fn resource_uri(name: &str, relative: &std::path::Path) -> String {
    format!(
        "{name}://{}",
        relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
    )
}

/// Builds an RFC 6570 URI template describing every resource addressable
/// under one configured vault: `{name}://{+path}`, the
/// same `{name}://` scheme [`resource_uri`] builds a concrete instance of,
/// with `{+path}` (reserved expansion, permitting `/`) standing in for any
/// vault-relative path. Advertised via `resources/templates/list` so a
/// client can construct a valid [`path_for_resource_uri`] input directly,
/// without first enumerating every file through `resources/list`.
pub(crate) fn resource_uri_template(name: &str) -> String {
    format!("{name}://{{+path}}")
}

/// Builds a `{name}://{relative-path}` URI for an already-validated
/// `VaultPath`, looking up its owning root's name in `roots`.
pub(crate) fn resource_uri_for(
    path: &VaultPath,
    roots: &VaultSet,
) -> Result<String, ResourceError> {
    let root = roots
        .root(path.root_id())
        .ok_or_else(|| ResourceError::InvalidPath {
            path: <&std::path::Path>::from(path).to_path_buf(),
        })?;
    Ok(resource_uri(root.name(), path.relative()))
}

/// Recovers the `VaultPath` a `resources/read` URI names
/// (`{name}://{relative-path}`), by reusing the identical
/// named-prefix resolution `VaultPath::try_from` already performs for a
/// `path`-parameter tool input: a resource URI and a hand-typed
/// tool path address a file the same way, so parsing them is the same
/// operation, not two.
pub(crate) fn path_for_resource_uri(
    uri: &str,
    roots: &VaultSet,
) -> Result<VaultPath, ResourceError> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw: uri })?)
}

/// Bounds `content` to at most `max_bytes`, preferring a whole-line cut so
/// the preview reads naturally; falls back to a UTF-8
/// char-boundary-safe byte cut only when even the first line alone
/// exceeds the limit, so the preview is never empty and never splits a
/// multi-byte character. Returns the (possibly unchanged) text and
/// whether it was actually shortened.
pub(crate) fn bounded_preview(content: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_owned(), false);
    }
    let mut end = 0;
    for line in content.split_inclusive('\n') {
        if end + line.len() > max_bytes {
            break;
        }
        end += line.len();
    }
    if end == 0 {
        end = content
            .char_indices()
            .take_while(|&(index, _)| index <= max_bytes)
            .last()
            .map_or(0, |(index, character)| index + character.len_utf8());
    }
    (content[..end].to_owned(), true)
}

/// Output schema for a tool handler that returns `CallToolResult`
/// directly rather than `Json<T>` (needed to attach an optional
/// `resource_link` content block): the `#[tool]` macro's
/// automatic schema derivation only recognises a `Json<T>` return type,
/// so this replicates it explicitly. Falls back to an empty schema in
/// the defensive-only case `schema_for_output` itself fails: an
/// `output_schema` attribute expression must not panic.
pub(crate) fn output_schema_for<T: schemars::JsonSchema + std::any::Any>()
-> std::sync::Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_output::<T>().unwrap_or_default()
}

/// Output schema for a tool handler whose `CallToolResult` return type
/// means its error path shares the same `structured_content` channel as
/// its success path: every such handler's errors go through
/// [`crate::tool_error::ToolFailure`]'s `IntoCallToolResult` impl, which
/// populates `structured_content` with `{code, message, path,
/// remediation}`, a different shape from `T`, not a subset of it. An
/// `output_schema` advertising only `T` is therefore inaccurate for any
/// call that errors.
///
/// A `oneOf` union of the two shapes was tried first and reverted:
/// confirmed live against the Cowork desktop app, a `oneOf`-composed
/// output schema made the *entire connector's* toolset disappear inside a
/// Cowork task (a local, on-device conversation), even though Cowork's
/// separate connector-settings screen still detected the server and
/// listed its tools; Cowork's per-task tool registry only supports flat
/// object schemas, no `oneOf`/`anyOf`/`allOf` composition. This instead
/// merges both shapes' `properties` into one flat object schema with
/// nothing marked `required`: the fields actually present depend on
/// whether the call succeeded or errored, and `CallToolResult.is_error`
/// (not a field inside `structured_content`) is the real discriminator.
///
/// Merging `properties` alone is not enough: a success shape whose field
/// is itself a nested type (`fs_read_multiple_files`'s `files:
/// [ReadManyItemToolResult]`) keeps a `$ref` to that type, so `$defs`
/// must be merged too, or the advertised schema references a definition
/// that was never carried over: a broken `$ref` a strict schema
/// consumer fails to compile, found live the same way as the previous
/// two Cowork-breaking shapes. `$schema` is likewise copied across for
/// consistency with every other tool's output schema.
pub(crate) fn fallible_output_schema_for<T: schemars::JsonSchema + std::any::Any>()
-> std::sync::Arc<rmcp::model::JsonObject> {
    let success = output_schema_for::<T>();
    let failure = output_schema_for::<crate::tool_error::ToolFailure>();
    let mut properties = rmcp::model::JsonObject::new();
    let mut defs = rmcp::model::JsonObject::new();
    for schema in [&success, &failure] {
        if let Some(schema_properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            properties.extend(schema_properties.clone());
        }
        if let Some(schema_defs) = schema.get("$defs").and_then(serde_json::Value::as_object) {
            defs.extend(schema_defs.clone());
        }
    }
    let mut merged = rmcp::model::JsonObject::new();
    merged.insert(
        "$schema".to_owned(),
        serde_json::json!("https://json-schema.org/draft/2020-12/schema"),
    );
    if !defs.is_empty() {
        merged.insert("$defs".to_owned(), serde_json::Value::Object(defs));
    }
    merged.insert("type".to_owned(), serde_json::json!("object"));
    merged.insert(
        "properties".to_owned(),
        serde_json::Value::Object(properties),
    );
    std::sync::Arc::new(merged)
}

/// Schema for an optional non-negative integer input parameter, advertised
/// as plain `"type": "integer"` rather than schemars' default `"type":
/// ["integer", "null"]` for an `Option<u64>` field: several real-world MCP
/// clients do not recognise JSON Schema 2020-12's array-form `type` (used
/// to express nullability) and, on seeing no single recognised type, fall
/// back to serialising the argument as a string, which the server then
/// rejects as a deserialization error. A caller still omits the key
/// entirely to mean "use the default"; this only changes the advertised
/// shape of a present value, not deserialization behaviour.
pub(crate) fn optional_u64_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    generator.subschema_for::<u64>()
}

/// Schema for [`crate::tool_error::ToolFailure`]'s optional `path` field,
/// advertised as plain `"type": "string"` rather than schemars' default
/// `"type": ["string", "null"]` for an `Option<PathBuf>` field: the same
/// array-form-nullability problem [`optional_u64_schema`] exists to avoid,
/// here on an output rather than an input schema (found live: it broke
/// Cowork's per-task tool registry for every tool sharing this field via
/// [`fallible_output_schema_for`], even after removing `oneOf`). Paired
/// with `#[serde(skip_serializing_if = "Option::is_none")]` on the field
/// itself, so a `None` path omits the key entirely rather than
/// serialising a `null` the narrowed schema would then reject.
pub(crate) fn optional_path_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    generator.subschema_for::<std::path::PathBuf>()
}

/// Schema for an optional string output field, advertised as plain
/// `"type": "string"` rather than schemars' default `"type": ["string",
/// "null"]` for an `Option<String>` field: the same array-form-nullability
/// problem [`optional_path_schema`] exists to avoid, here for a field
/// (`IndexesStatusToolResult::state_directory`) that is itself a `String`
/// rather than a `PathBuf`. Pair with `#[serde(skip_serializing_if =
/// "Option::is_none")]` on the field itself, so a `None` value omits the
/// key entirely rather than serialising a `null` the narrowed schema would
/// then reject.
pub(crate) fn optional_string_schema(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    generator.subschema_for::<String>()
}

/// Rewrites every nullable-union shape `schemars` can emit for an
/// `Option<T>` field down to a plain, single-type schema for `T`, at any
/// nesting depth including inside `$defs`: array-form `"type": [X,
/// "null"]` (what [`optional_u64_schema`]/[`optional_path_schema`]/
/// [`optional_string_schema`] each opt one field out of by hand), and
/// `"anyOf"`/`"oneOf": [<T's subschema>, {"type": "null"}]` (`schemars`'
/// fallback when `T` is a `$ref` rather than a primitive, so a plain
/// array-form `type` cannot express it). Both shapes are confirmed live to
/// take down Cowork's whole per-task tool registry, not just the one
/// field carrying it (the `oneOf` revert, 0.7.1; the `state_directory`
/// array-form fix, 0.13.4), so this is applied once to every tool this
/// server advertises (`ContextOsServer::catalogue`,
/// `ContextOsServer::effective_catalogue`) rather than opted in field by
/// field.
///
/// A union with more than one non-null branch (`base_apply`'s and
/// `canvas_apply`'s operation-kind inputs are the only such case in this
/// catalogue) is a genuine discriminated choice the caller must supply,
/// not an absent-value marker, and is left untouched: collapsing it would
/// silently accept a shape the handler cannot dispatch, or hide real
/// result variants.
pub(crate) fn sanitise_nullable_unions(schema: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = schema {
        strip_null_from_type_array(map);
        collapse_null_only_union(map);
    }
    match schema {
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                sanitise_nullable_unions(value);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sanitise_nullable_unions(item);
            }
        }
        _ => {}
    }
}

/// Narrows `"type": [X, "null"]` to plain `"type": X` once `"null"` is
/// removed and exactly one type remains; leaves a genuinely multi-typed
/// array (never observed in this catalogue, but not this function's call
/// to guess at) untouched.
fn strip_null_from_type_array(map: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(serde_json::Value::Array(types)) = map.get("type") else {
        return;
    };
    let mut remaining: Vec<serde_json::Value> = types
        .iter()
        .filter(|entry| entry.as_str() != Some("null"))
        .cloned()
        .collect();
    if remaining.len() == 1 {
        map.insert("type".to_owned(), remaining.remove(0));
    }
}

/// Replaces a `oneOf`/`anyOf` holding exactly one non-null branch (plus
/// one or more `{"type": "null"}` branches) with that branch's own
/// content merged into `map`, preserving any of `map`'s own sibling keys
/// (a field-level `description` over the branch's own, for example) over
/// the replacement's.
fn collapse_null_only_union(map: &mut serde_json::Map<String, serde_json::Value>) {
    for keyword in ["oneOf", "anyOf"] {
        let Some(serde_json::Value::Array(branches)) = map.get(keyword) else {
            continue;
        };
        let non_null: Vec<&serde_json::Value> = branches
            .iter()
            .filter(|branch| !is_null_schema(branch))
            .collect();
        if non_null.len() == 1 && non_null.len() < branches.len() {
            let replacement = non_null[0].clone();
            map.remove(keyword);
            if let serde_json::Value::Object(replacement_map) = replacement {
                for (key, value) in replacement_map {
                    map.entry(key).or_insert(value);
                }
            }
        }
    }
}

/// Reports whether `value` is exactly `{"type": "null"}`, `schemars`'
/// null-marker branch inside a nullable union.
fn is_null_schema(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Object(map)
            if map.len() == 1 && map.get("type").and_then(serde_json::Value::as_str) == Some("null")
    )
}

/// A local JSON Schema reference target: either a named `$defs` entry, or
/// the bare `"#"` self-reference to the schema's own root.
#[derive(Clone, Eq, Hash, PartialEq)]
enum RefTarget {
    Root,
    Def(String),
}

/// Parses a `$ref` value into the [`RefTarget`] it names, or `None` for
/// any shape other than the two [`inline_local_refs`] resolves (a
/// non-local `$ref` is a separate, already-rejected problem the
/// `no_tool_schema_anywhere_uses_forbidden_composition_or_array_form_type`
/// contract test catches directly).
fn ref_target(reference: &str) -> Option<RefTarget> {
    if reference == "#" {
        Some(RefTarget::Root)
    } else {
        reference
            .strip_prefix("#/$defs/")
            .map(|name| RefTarget::Def(name.to_owned()))
    }
}

/// Inlines every local `$ref` a `schema_for_output`/`schema_for_input`-
/// generated schema can contain, `"#/$defs/Name"` and the bare
/// self-reference `"#"` alike, directly into the site that references it,
/// and drops `$defs` once nothing points into it any more.
///
/// `mempalace-rs`, the confirmed-working control group for Cowork's
/// `/context` picker, hand-writes every schema fully flat with no
/// `$ref`/`$defs` anywhere at all; `rmcp` hardcodes
/// `SchemaSettings::draft2020_12()` (`$ref`/`$defs` for every nested
/// struct or enum) with no way to configure it away at the `#[tool]`
/// macro's own generation site (`rmcp::handler::server::common::
/// schema_for_type`), so inlining after the fact, here, is the only lever
/// available. Call after [`sanitise_nullable_unions`]: a null-combined
/// union whose surviving branch is itself a `$ref` is collapsed to a bare
/// `$ref` first, which this function then inlines like any other.
///
/// A definition that (directly or transitively) references itself cannot
/// be inlined without an infinite schema, the same structural limit
/// `schemars`' own `inline_subschemas` setting hits; `expanding` (the
/// reference targets already being expanded on the current path) guards
/// against that by leaving such a `$ref` in place rather than attempting
/// it. Nothing in the current catalogue reaches this path any more:
/// `fs_directory_tree`'s recursive `TreeNodeToolResult` (`children:
/// Option<Vec<Self>>`) was the one type that did, and its tool now omits
/// an `output_schema` entirely rather than advertise a schema this
/// function can only partially flatten. This guard stays as defensive
/// infrastructure against a future recursive type reaching a tool schema
/// again; `no_tool_schema_anywhere_uses_forbidden_composition_or_array_
/// form_type` (`tests/tool_contract.rs`) would still catch that at the
/// same `$ref` it caught this one at.
pub(crate) fn inline_local_refs(schema: &mut serde_json::Value) {
    let serde_json::Value::Object(root) = schema else {
        return;
    };
    let mut definitions: std::collections::HashMap<RefTarget, serde_json::Value> =
        std::collections::HashMap::new();
    if let Some(serde_json::Value::Object(defs)) = root.get("$defs") {
        for (name, definition) in defs {
            definitions.insert(RefTarget::Def(name.clone()), definition.clone());
        }
    }
    let mut root_shape = root.clone();
    root_shape.remove("$defs");
    definitions.insert(
        RefTarget::Root,
        serde_json::Value::Object(root_shape.clone()),
    );

    let mut expanded = serde_json::Value::Object(root_shape);
    let mut expanding = vec![RefTarget::Root];
    inline_refs_in(&mut expanded, &definitions, &mut expanding);

    if let serde_json::Value::Object(result) = expanded {
        *root = result;
    }
    root.remove("$defs");
}

/// Recursive worker behind [`inline_local_refs`]: expands a `$ref` node
/// in place (after first expanding whatever it points to, so the result
/// is never left holding a further reference this pass could have
/// resolved), or, absent a `$ref`, recurses into every child unchanged.
fn inline_refs_in(
    value: &mut serde_json::Value,
    definitions: &std::collections::HashMap<RefTarget, serde_json::Value>,
    expanding: &mut Vec<RefTarget>,
) {
    match value {
        serde_json::Value::Object(map) => {
            let reference = map
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(reference) = reference {
                if let Some(target) = ref_target(&reference)
                    && !expanding.contains(&target)
                    && let Some(resolved) = definitions.get(&target).cloned()
                {
                    let mut resolved = resolved;
                    expanding.push(target);
                    inline_refs_in(&mut resolved, definitions, expanding);
                    expanding.pop();
                    if let serde_json::Value::Object(mut resolved_map) = resolved {
                        for (key, sibling) in map.iter() {
                            if key != "$ref" {
                                resolved_map.insert(key.clone(), sibling.clone());
                            }
                        }
                        *map = resolved_map;
                    }
                }
                // Otherwise (a cycle, or a `$ref` shape this function does
                // not resolve): leave it exactly as advertised.
                return;
            }
            for child in map.values_mut() {
                inline_refs_in(child, definitions, expanding);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                inline_refs_in(item, definitions, expanding);
            }
        }
        _ => {}
    }
}

/// Builds a `resource_link` content block for `path`, or `None` if its URI
/// genuinely cannot be built (defensive only: see
/// `ToolError::AttachmentUriInvalid`'s equivalent for `fs_attach_file`; a
/// `VaultPath` resolved against its own `VaultSet` should never actually
/// hit this).
pub(crate) fn resource_link_for(path: &VaultPath, roots: &VaultSet) -> Option<ContentBlock> {
    let uri = resource_uri_for(path, roots).ok()?;
    let absolute_path: &std::path::Path = path.into();
    let name = absolute_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut resource = rmcp::model::Resource::new(uri, name);
    if let Some(mime_type) = contextos_fs::mime_type_for_extension(absolute_path) {
        resource = resource.with_mime_type(mime_type);
    }
    Some(ContentBlock::resource_link(resource))
}
