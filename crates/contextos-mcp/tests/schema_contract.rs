//! Catalogue-membership and schema-shape contract tests: build-complete
//! vs. effective catalogue, forbidden schema composition
//! (`oneOf`/`anyOf`/`allOf`/array-form nullable `type`/uninlined `$ref`),
//! per-tool object-schema-and-description presence, the `path`/`vault`
//! addressing-scheme documentation check, and `fs_attach_file`. Split
//! from `tool_contract.rs` to keep both files under the project's
//! file-size limit.

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

/// `ContextOsServer::catalogue()` is the build-complete set: every tool
/// this binary ships, including the `ephemeris_*` tools, regardless of
/// any instance's `[server] astro` setting. It is not what any one
/// running server actually advertises; see
/// [`effective_catalogue_omits_ephemeris_tools_unless_astro_is_enabled`]
/// for that.
#[test]
fn catalogue_exposes_every_built_in_tool_this_binary_ships() {
    let mut names = ContextOsServer::catalogue()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    names.sort();

    let mut expected = core_catalogue_tool_names();
    expected.extend_from_slice(&[
        "ephemeris_boundaries",
        "ephemeris_moon_phase",
        "ephemeris_personal_year_period",
        "ephemeris_solar_events",
        "ephemeris_wheel_of_year",
    ]);
    expected.sort_unstable();
    assert_eq!(names, expected);
}

/// Every advertised schema (input and output) across every tool this
/// binary ships must be free of `oneOf`/`anyOf`/`allOf` composition and of
/// JSON Schema 2020-12's array-form `type` (used for nullability), at any
/// nesting depth: all are confirmed live to take down Cowork's whole
/// per-task tool registry (the `oneOf` revert, 0.7.1; the
/// `state_directory` array-form fix, 0.13.4), not just the offending tool,
/// and `sanitise_router_schemas` (`server.rs`) now sweeps every tool this
/// server can ever advertise, built in or extension, so a single field
/// regressing into any of these shapes anywhere is a whole-connector
/// outage this test now catches directly.
///
/// `base_apply`'s and `canvas_apply`'s operation-kind inputs were the one
/// remaining `oneOf`, a genuine discriminated choice between several
/// operation shapes rather than a nullability shape
/// `sanitise_nullable_unions` could collapse; `base_operations_schema`/
/// `canvas_operations_schema` (`obsidian_types.rs`) now hand-advertise a
/// flat object schema for it instead (an `op` discriminator plus every
/// variant's fields declared optional), trading the advertised schema's
/// precision for a shape confirmed compatible everywhere else in this
/// catalogue. Actual validation is unchanged: both types still deny
/// unknown fields and require each operation's own fields via serde's
/// internally tagged enum matching.
///
/// `$defs`/`$ref` themselves are also forbidden everywhere, no exception:
/// `mempalace-rs`, the confirmed-working control group for Cowork's
/// `/context` picker, hand-writes every schema fully flat with no
/// `$ref`/`$defs` anywhere at all, and `rmcp` hardcodes
/// `SchemaSettings::draft2020_12()` ($ref/$defs for every nested struct or
/// enum) with no way to configure it away at the `#[tool]` macro's own
/// generation site, so `inline_local_refs` (`resource_support.rs`) inlines
/// every reference after the fact instead (wired into
/// `sanitise_router_schemas` alongside `sanitise_nullable_unions`,
/// `server.rs`). `fs_directory_tree`'s recursive directory-entry shape
/// could not be inlined without an infinite schema, the same structural
/// limit `schemars`' own `inline_subschemas` setting hits, so that tool
/// omits an `output_schema` entirely instead of advertising one that
/// carries a `$ref`.
#[test]
fn no_tool_schema_anywhere_uses_forbidden_composition_or_array_form_type() -> Result<(), Box<dyn std::error::Error>> {
    let mut problems = Vec::new();
    for tool in ContextOsServer::catalogue().list_all() {
        scan_schema(&tool.name, "input", &tool.input_schema, &mut problems);
        if let Some(output) = &tool.output_schema {
            scan_schema(&tool.name, "output", output, &mut problems);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n").into())
    }
}

fn scan_schema(tool: &str, which: &str, schema: &Map<String, serde_json::Value>, problems: &mut Vec<String>) {
    let mut path = Vec::new();
    scan_value(
        tool,
        which,
        &serde_json::Value::Object(schema.clone()),
        &mut path,
        problems,
    );
}

fn scan_value(tool: &str, which: &str, value: &serde_json::Value, path: &mut Vec<String>, problems: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for keyword in ["oneOf", "anyOf", "allOf"] {
                if map.contains_key(keyword) {
                    problems.push(format!(
                        "{tool} {which} schema at /{} uses {keyword}, which breaks Cowork's \
                         tool registry",
                        path.join("/")
                    ));
                }
            }
            if matches!(map.get("type"), Some(serde_json::Value::Array(_))) {
                problems.push(format!(
                    "{tool} {which} schema at /{} has an array-form nullable type, which \
                     breaks Cowork's tool registry",
                    path.join("/")
                ));
            }
            if map.contains_key("$defs") {
                problems.push(format!(
                    "{tool} {which} schema at /{} has $defs, which mempalace-rs's flat \
                     schemas never need and inline_local_refs should have removed",
                    path.join("/")
                ));
            }
            if let Some(serde_json::Value::String(reference)) = map.get("$ref") {
                if reference.starts_with('#') {
                    problems.push(format!(
                        "{tool} {which} schema at /{} has an uninlined $ref {reference:?}, \
                         which inline_local_refs should have expanded",
                        path.join("/")
                    ));
                } else {
                    problems.push(format!(
                        "{tool} {which} schema at /{} has a non-local $ref {reference:?}: MCP \
                         tool schemas must be standalone",
                        path.join("/")
                    ));
                }
            }
            for (key, child) in map {
                path.push(key.clone());
                scan_value(tool, which, child, path, problems);
                path.pop();
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                scan_value(tool, which, item, path, problems);
                path.pop();
            }
        }
        _ => {}
    }
}

/// `effective_catalogue()`, what a running instance actually advertises
/// and can dispatch, omits the `ephemeris_*` tools unless that instance's
/// `[server] astro` is set; the runtime toggle, not a Cargo feature, is
/// what decides visibility.
#[test]
fn effective_catalogue_omits_ephemeris_tools_unless_astro_is_enabled()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;

    let disabled_names: Vec<String> = ContextOsServer::try_from(config.clone())?
        .effective_catalogue()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert!(
        !disabled_names.iter().any(|name| name.starts_with("ephemeris_")),
        "no ephemeris_* tool should be advertised with astro at its default, got {disabled_names:?}"
    );

    config.server.astro = true;
    let enabled_names: Vec<String> = ContextOsServer::try_from(config)?
        .effective_catalogue()
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    for expected in [
        "ephemeris_boundaries",
        "ephemeris_moon_phase",
        "ephemeris_personal_year_period",
        "ephemeris_solar_events",
        "ephemeris_wheel_of_year",
    ] {
        assert!(
            enabled_names.iter().any(|name| name == expected),
            "{expected} missing once astro is enabled: {enabled_names:?}"
        );
    }
    Ok(())
}

fn core_catalogue_tool_names() -> Vec<&'static str> {
    vec![
        "base_apply",
        "base_create",
        "base_query",
        "base_read",
        "canvas_apply",
        "canvas_create",
        "canvas_read",
        "doctor",
        "doctor_resolve",
        "frontmatter_read",
        "frontmatter_update",
        "fs_attach_file",
        "fs_create_directory",
        "fs_delete_file",
        "fs_directory_tree",
        "fs_edit_file",
        "fs_get_file_info",
        "fs_list_allowed_directories",
        "fs_list_directory",
        "fs_move_file",
        "fs_read_multiple_files",
        "fs_read_text_file",
        "fs_search_files",
        "fs_write_file",
        "git_commit",
        "git_diff",
        "git_init",
        "git_log",
        "git_restore",
        "git_status",
        "links_read",
        "mermaid_render",
        "mermaid_validate",
        "note_create",
        "query_graph",
        "query_index_rebuild",
        "query_index_status",
        "query_semantic",
        "query_text",
        "vault_index_rebuild",
        "vault_info",
        "vault_log_append",
    ]
}

#[tokio::test]
async fn attach_file_embeds_text_and_base64_binary_resources() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("plain.txt"), "hello")?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3])?;
    let oversized = std::fs::File::create(vault.path().join("oversized.bin"))?;
    oversized.set_len(10 * 1024 * 1024 + 1)?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;

    let text = call_tool(
        server.clone(),
        "fs_attach_file",
        serde_json::from_value(json!({"path": "plain.txt"}))?,
    )
    .await?;
    let binary = call_tool(
        server.clone(),
        "fs_attach_file",
        serde_json::from_value(json!({"path": "image.png"}))?,
    )
    .await?;
    let too_large = call_tool(
        server,
        "fs_attach_file",
        serde_json::from_value(json!({"path": "oversized.bin"}))?,
    )
    .await?;

    assert_eq!(text.is_error, Some(false));
    assert_eq!(binary.is_error, Some(false));
    assert_eq!(too_large.is_error, Some(true));
    assert_eq!(
        too_large
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("io/too-large"))
    );
    let text_resource = text
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("text attachment was not an embedded resource"))?;
    let binary_resource = binary
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("binary attachment was not an embedded resource"))?;
    // `{name}://`, never `file://` (superseded); the vault here uses its
    // default name-from-basename rather than an explicit one, so the
    // scheme is asserted generically rather than against a literal name.
    assert!(matches!(
        &text_resource.resource,
        rmcp::model::ResourceContents::TextResourceContents { text, uri, .. }
            if text == "hello" && uri.contains("://") && !uri.starts_with("file://")
    ));
    assert!(matches!(
        &binary_resource.resource,
        rmcp::model::ResourceContents::BlobResourceContents { blob, mime_type, uri, .. }
            if blob == "AAECAw==" && mime_type.as_deref() == Some("image/png")
                && uri.contains("://") && !uri.starts_with("file://")
    ));
    Ok(())
}

#[test]
fn every_delivered_tool_has_an_object_schema_and_description() {
    for tool in ContextOsServer::catalogue().list_all() {
        assert_eq!(
            tool.input_schema.get("type").and_then(|value| value.as_str()),
            Some("object")
        );
        assert!(tool.description.is_some());
    }
}

/// Regression for a Cowork failure observed against `fs_search_files`:
/// with no field-level schema description, a caller guessed `"vault://"`
/// for a top-level `path` property, which the named-prefix addressing
/// scheme accepts syntactically but rejects at resolution time
/// (`path/empty-named-prefix`) because it omits the `.` that selects the
/// vault root. Every top-level `path`/`vault` input property across the
/// whole catalogue must carry a non-empty description that actually
/// explains the `{name}://{relative-path}` addressing scheme, so a caller
/// with no prior knowledge of that scheme sees the correct form (and the
/// `{name}://.` whole-vault case) before ever calling the tool.
#[test]
fn every_path_or_vault_input_property_documents_the_vault_addressing_scheme() {
    let mut problems = Vec::new();
    for tool in ContextOsServer::catalogue().list_all() {
        let Some(properties) = tool.input_schema.get("properties").and_then(|value| value.as_object()) else {
            continue;
        };
        for property_name in ["path", "vault"] {
            let Some(schema) = properties.get(property_name) else {
                continue;
            };
            let description = schema
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if description.is_empty() {
                problems.push(format!(
                    "{}'s {property_name:?} input property has no description",
                    tool.name
                ));
            } else if !description.contains("://") {
                problems.push(format!(
                    "{}'s {property_name:?} input property description does not explain the \
                     vault addressing scheme: {description:?}",
                    tool.name
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}
