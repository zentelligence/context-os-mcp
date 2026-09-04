//! Flat-fallible-output-schema contract tests: every tool that returns a
//! `CallToolResult` directly (rather than `Json<T>`) must still advertise
//! a single flat object output schema merging its success shape with
//! `ToolFailure`, with no `oneOf`/`anyOf`/`allOf` composition and no
//! uninlined `$ref`/`$defs`. Split from `tool_contract.rs` to keep both
//! files under the project's file-size limit.

use contextos_mcp::ContextOsServer;

/// `fs_read_text_file` and `fs_read_multiple_files` return `CallToolResult`
/// directly rather than `Json<T>`, so their error path (like
/// every tool's) populates `structured_content` with a `ToolFailure`
/// (`code`/`message`/`path`/`remediation`), not their declared success
/// shape. A `oneOf` union was tried and reverted: confirmed live against
/// the Cowork desktop app, a `oneOf`-composed output schema made the
/// entire connector's toolset disappear inside a Cowork task (though the
/// connector-settings screen still detected it), because Cowork only
/// supports flat object schemas, no `oneOf`/`anyOf`/`allOf` composition.
/// The advertised schema must instead be a single flat object whose
/// `properties` are the union of the success shape and `ToolFailure`'s,
/// with nothing marked `required`: `CallToolResult.is_error` is the
/// actual discriminator, not a field inside `structured_content`.
#[test]
fn fs_read_text_file_and_fs_read_multiple_files_advertise_a_flat_fallible_output_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = ContextOsServer::catalogue();
    assert_flat_fallible_output_schema(
        &catalogue,
        "fs_read_text_file",
        &["content", "content_hash", "line_count", "truncated"],
    )?;
    assert_flat_fallible_output_schema(&catalogue, "fs_read_multiple_files", &["files"])?;
    Ok(())
}

/// Every other tool returning `Result<Json<T>, ToolFailure>` shares the
/// exact same exposure `fs_read_text_file`/`fs_read_multiple_files` were
/// fixed for above: `rmcp-macros` auto-generates that shape's output
/// schema from the success type `T` alone (confirmed by reading
/// `rmcp-macros`' `extract_schema_from_return_type`), with no awareness
/// that the same handler's error path returns a `ToolFailure` through the
/// identical `structured_content` channel. Left unfixed, a schema-
/// validating client rejects the well-formed error the moment any of
/// these tools' error path is actually exercised, hiding the remediation
/// it is meant to surface (confirmed live via `fs_write_file`'s
/// `io/conflict` path against the Cowork desktop app).
///
/// Covers `query.rs`, `doctor.rs`, `ephemeris.rs`, and `mermaid.rs`'s
/// `mermaid_validate` only: every `fs.rs`/`git.rs`/`obsidian.rs`/
/// `vault.rs` tool is already covered by
/// `every_delivered_tool_advertises_its_exact_result_fields`'s own
/// (now similarly fixed) exact-field matrix, and duplicating that table
/// here would just be a second copy to keep in sync. `fs_attach_file`
/// and `fs_directory_tree` are deliberately absent from both: both
/// return `CallToolResult` directly and never populate
/// `structured_content` on success (`fs_directory_tree`'s self-
/// referential shape cannot be expressed via `fallible_output_schema_for`
/// at all, `fcaa45e`), so neither advertises an output schema, and
/// neither is exposed to this bug. `mermaid_render` is absent for the
/// same reason.
#[test]
fn every_remaining_tool_advertises_a_flat_fallible_output_schema() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = ContextOsServer::catalogue();
    let expected: &[(&str, &[&str])] = &[
        // query.rs
        ("query_text", &["hits", "index_freshness"]),
        ("query_semantic", &["hits"]),
        ("query_graph", &["nodes", "edges"]),
        ("query_index_status", &["state_directory", "text", "graph", "semantic"]),
        ("query_index_rebuild", &["text", "graph", "semantic"]),
        // doctor.rs
        ("doctor", &["checks", "has_failures"]),
        ("doctor_resolve", &["outcomes", "report"]),
        // ephemeris.rs
        (
            "ephemeris_moon_phase",
            &[
                "name",
                "illumination_fraction",
                "days_into_cycle",
                "near_new",
                "near_first_quarter",
                "near_full",
                "near_last_quarter",
            ],
        ),
        ("ephemeris_solar_events", &["events"]),
        ("ephemeris_wheel_of_year", &["points"]),
        (
            "ephemeris_personal_year_period",
            &["period_number", "ruling_planet", "transition"],
        ),
        ("ephemeris_boundaries", &["events"]),
        // mermaid.rs
        ("mermaid_validate", &["valid", "diagnostics"]),
    ];

    for (name, success_fields) in expected {
        assert_flat_fallible_output_schema(&catalogue, name, success_fields)?;
    }
    Ok(())
}

/// Shared assertion behind both flat-fallible-output-schema tests above:
/// a Cowork-safe output schema is a single flat object schema (no
/// `oneOf`/`anyOf`/`allOf`), whose `properties` are exactly the union of
/// `success_fields` and `ToolFailure`'s own fields, with nothing marked
/// `required` (the fields actually present depend on whether the call
/// succeeded or errored; `CallToolResult.is_error` is the real
/// discriminator, not a field inside `structured_content`), every
/// property's `type` a single string rather than an array-form nullable
/// union, and every `$ref` the merge carried over resolved by a matching
/// `$defs` entry.
fn assert_flat_fallible_output_schema(
    catalogue: &rmcp::handler::server::router::tool::ToolRouter<ContextOsServer>,
    name: &str,
    success_fields: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let failure_fields = ["code", "message", "path", "remediation"];
    let tool = catalogue
        .get(name)
        .ok_or_else(|| std::io::Error::other(format!("missing tool {name}")))?;
    let schema = tool
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other(format!("{name} omitted output schema")))?;

    for composition_keyword in ["oneOf", "anyOf", "allOf"] {
        assert!(
            !schema.contains_key(composition_keyword),
            "{name} schema uses {composition_keyword}, which Cowork's flat-schema-only \
             tool registry cannot parse"
        );
    }
    assert_eq!(
        schema.get("type").and_then(serde_json::Value::as_str),
        Some("object"),
        "{name} schema root is not a flat object"
    );
    assert!(
        !schema.contains_key("required"),
        "{name} schema advertises a required array, but the fields actually present depend \
         on whether the call succeeded or errored"
    );

    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other(format!("{name} omitted output properties")))?;
    let mut actual_fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
    actual_fields.sort_unstable();

    let mut expected_fields = success_fields.to_vec();
    expected_fields.extend_from_slice(&failure_fields);
    expected_fields.sort_unstable();
    expected_fields.dedup();

    assert_eq!(actual_fields, expected_fields, "{name} flat fallible schema drifted");

    // `optional_u64_schema`'s own doc comment already documents this
    // class of problem for input schemas: real MCP clients (Cowork's
    // per-task tool registry among them, confirmed live) don't
    // recognise JSON Schema's array-form `type` used to express
    // nullability (`["string", "null"]`), the same failure mode as
    // a `oneOf`/`anyOf` composition as far as a naive schema
    // consumer is concerned. Every property's `type` must be a
    // single value, never an array.
    for (field, property) in properties {
        assert!(
            property.get("type").is_none_or(serde_json::Value::is_string),
            "{name} property {field:?} has a non-string (likely array-form nullable) \
             type: {property:?}"
        );
    }

    // The merge only ever copied `properties`, so a success shape
    // whose fields reference a nested type via `$ref` (like
    // `fs_read_multiple_files`'s `files: [ReadManyItemToolResult]`)
    // kept the `$ref` but silently dropped the `$defs` entry it
    // points to: a schema a strict consumer fails to compile at
    // all, distinct from (and more severe than) the flat-shape and
    // array-nullable problems already covered above.
    assert!(
        schema.contains_key("$schema"),
        "{name} schema omits $schema, unlike every other tool's output schema"
    );
    let defined_names = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .map(|defs| defs.keys().cloned().collect::<std::collections::HashSet<_>>())
        .unwrap_or_default();
    let mut referenced_names = std::collections::HashSet::new();
    collect_ref_names(&serde_json::Value::Object(properties.clone()), &mut referenced_names);
    for referenced in &referenced_names {
        assert!(
            defined_names.contains(referenced),
            "{name} schema references ${{referenced}} via $ref but has no matching \
             $defs entry (referenced: {referenced:?}, defined: {defined_names:?})"
        );
    }
    Ok(())
}

/// Collects every `$defs`-local type name a JSON value's `$ref` fields
/// point at (`#/$defs/Name`), recursing through arrays and objects.
fn collect_ref_names(value: &serde_json::Value, names: &mut std::collections::HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(serde_json::Value::as_str)
                && let Some(name) = reference.strip_prefix("#/$defs/")
            {
                names.insert(name.to_owned());
            }
            for nested in map.values() {
                collect_ref_names(nested, names);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_ref_names(item, names);
            }
        }
        _ => {}
    }
}
