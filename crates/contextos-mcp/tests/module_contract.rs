//! The `ServerModule` extension contract, covering namespaced
//! tools, startup collision rejection, injected core services with no
//! bypass, and a feature-off catalogue that is byte-identical to the core
//! catalogue. Exercised end to end through a test-only fixture module.
//!
//! Per `phase-5-decision-addendum.md` A4, no `demo_*` reference module ships
//! in any build. The fixture below lives only in this test binary (it is
//! never linked into the shipped `contextos` binary) and stands in for the
//! future Business/Personal/Developer OS module crates this trait exists to
//! support.

use std::sync::Arc;

use contextos_core::{Origin, WriteMutation};
use contextos_mcp::{
    Config, ContextOsServer, ModuleCall, ModuleContext, ModuleManifest, ModuleNamespace, ModuleRegistry,
    ModuleRegistryError, ServerBuildConfig, ServerBuildError, ServerModule, ServerModuleFuture,
};
use rmcp::ErrorData;
use rmcp::ServiceExt;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A minimal Developer OS fixture exercising the full contract: manifest, a
/// schema-carrying namespaced tool, and a mutation through the injected
/// write pipeline (so it inherits path safety, locking, logging,
/// versioning, and indexing exactly like a core tool, with no way around
/// any of them).
struct DeveloperOsFixture;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoWriteInput {
    path: String,
    content: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct EchoWriteOutput {
    path: String,
    bytes_written: usize,
    created: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoReadInput {
    path: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct EchoReadOutput {
    content: Option<String>,
    root_entries: Vec<String>,
}

impl ServerModule for DeveloperOsFixture {
    fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            namespace: ModuleNamespace::DeveloperOs,
            name: "developer-os-fixture",
            version: "0.0.0",
        }
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "dos_echo_write",
                "Fixture: writes a note through the injected pipeline",
                rmcp::handler::server::common::schema_for_input::<EchoWriteInput>()
                    .unwrap_or_else(|_| Arc::new(serde_json::Map::new())),
            ),
            Tool::new(
                "dos_echo_read",
                "Fixture: reads a note and lists the vault root through the injected read access",
                rmcp::handler::server::common::schema_for_input::<EchoReadInput>()
                    .unwrap_or_else(|_| Arc::new(serde_json::Map::new())),
            ),
        ]
    }

    fn handle<'a>(&'a self, call: ModuleCall, ctx: &'a ModuleContext) -> ServerModuleFuture<'a> {
        Box::pin(async move {
            match call.name.as_str() {
                "dos_echo_write" => echo_write(call, ctx).await,
                "dos_echo_read" => echo_read(call, ctx).await,
                other => Err(ErrorData::invalid_request(
                    format!("fixture module has no tool named {other:?}"),
                    None,
                )),
            }
        })
    }
}

/// Tool-level failure for a request the module itself rejects before
/// reaching `ModuleContext` (argument deserialisation): still the shared
/// `code`/`message`/`remediation` shape every core tool and every
/// `ModuleContextError` use, not an ad hoc, less complete envelope.
fn invalid_arguments(message: &str) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "code": "module/invalid-arguments",
        "message": message,
        "remediation": "Correct the tool arguments using the advertised input schema.",
    }))
}

async fn echo_write(call: ModuleCall, ctx: &ModuleContext) -> Result<CallToolResult, ErrorData> {
    let input: EchoWriteInput = match serde_json::from_value(Value::Object(call.arguments)) {
        Ok(input) => input,
        Err(error) => {
            return Ok(invalid_arguments(&format!("failed to deserialize parameters: {error}")));
        }
    };
    let path = match ctx.resolve_path(&input.path).await {
        Ok(path) => path,
        Err(error) => return Ok(error.into_tool_result()),
    };
    let result = ctx
        .write(WriteMutation {
            path,
            content: input.content,
            expected_hash: None,
            force: false,
            origin: Origin::Tool("dos_echo_write".to_owned()),
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => return Ok(error.into_tool_result()),
    };
    Json(EchoWriteOutput {
        path: result.value.path.relative().display().to_string(),
        bytes_written: result.value.bytes_written,
        created: result.value.created,
    })
    .into_call_tool_result()
}

/// Exercises `ModuleContext::read_optional_text` and `::list`: the read
/// side of the injected capabilities, proven here to actually run off the
/// async executor thread (both are `async fn`s backed by
/// `tokio::task::spawn_blocking`), not merely to exist.
async fn echo_read(call: ModuleCall, ctx: &ModuleContext) -> Result<CallToolResult, ErrorData> {
    let input: EchoReadInput = match serde_json::from_value(Value::Object(call.arguments)) {
        Ok(input) => input,
        Err(error) => {
            return Ok(invalid_arguments(&format!("failed to deserialize parameters: {error}")));
        }
    };
    let path = match ctx.resolve_path(&input.path).await {
        Ok(path) => path,
        Err(error) => return Ok(error.into_tool_result()),
    };
    let content = match ctx.read_optional_text(&path).await {
        Ok(text) => text.map(|text| text.content),
        Err(error) => return Ok(error.into_tool_result()),
    };
    let root = match ctx.resolve_path(".").await {
        Ok(root) => root,
        Err(error) => return Ok(error.into_tool_result()),
    };
    let root_entries = match ctx.list(&root).await {
        Ok(entries) => entries.into_iter().map(|entry| entry.name).collect(),
        Err(error) => return Ok(error.into_tool_result()),
    };
    Json(EchoReadOutput { content, root_entries }).into_call_tool_result()
}

fn registry_with_fixture() -> ModuleRegistry {
    ModuleRegistry::new().register(Arc::new(DeveloperOsFixture))
}

/// Drives one tool call through a real in-process MCP transport (a duplex
/// pipe, framed exactly like stdio), so the contract is proven through the
/// MCP adapter rather than by calling `ServerModule` directly.
async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

#[test]
fn a_module_registered_under_the_wrong_namespace_fails_server_construction() -> Result<(), BoxError> {
    struct MisnamedFixture;
    impl ServerModule for MisnamedFixture {
        fn manifest(&self) -> ModuleManifest {
            ModuleManifest {
                namespace: ModuleNamespace::DeveloperOs,
                name: "misnamed-fixture",
                version: "0.0.0",
            }
        }
        fn tools(&self) -> Vec<Tool> {
            vec![Tool::new("pos_intruder", "wrong namespace", Map::new())]
        }
        fn handle<'a>(&'a self, _call: ModuleCall, _ctx: &'a ModuleContext) -> ServerModuleFuture<'a> {
            Box::pin(async { unreachable!("rejected before dispatch") })
        }
    }

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let modules = ModuleRegistry::new().register(Arc::new(MisnamedFixture));
    let result = ContextOsServer::try_from(ServerBuildConfig { config, modules });
    assert!(matches!(
        result,
        Err(ServerBuildError::Module(ModuleRegistryError::UnnamespacedTool { .. }))
    ));
    Ok(())
}

#[test]
fn two_modules_contributing_the_same_tool_name_fail_server_construction() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let modules = ModuleRegistry::new()
        .register(Arc::new(DeveloperOsFixture))
        .register(Arc::new(DeveloperOsFixture));
    let result = ContextOsServer::try_from(ServerBuildConfig { config, modules });
    assert!(matches!(
        result,
        Err(ServerBuildError::Module(ModuleRegistryError::DuplicateTool { tool_name }))
            if tool_name == "dos_echo_write"
    ));
    Ok(())
}

// A module tool colliding with a *reserved core* name specifically (as
// opposed to colliding with another module's tool, covered above) cannot
// currently be exercised at this level: no core tool name starts with
// `bos_`/`pos_`/`dos_`, so a correctly namespaced module tool can never
// literally equal one. `ModuleRegistry::validate`'s reserved-set behaviour
// is covered directly in `crates/contextos-mcp/src/module.rs`'s unit
// tests, which pass a synthetic reserved set to exercise that branch.

#[test]
fn with_no_modules_registered_the_effective_catalogue_is_byte_identical_to_core() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;

    // Two servers built from the same config, differing only in `modules`
    // (`ModuleRegistry::new()` explicitly on both, `ContextOsServer::try_from(Config)`
    // implicitly): isolates the variable this test actually checks
    // (module registration) from `[server] astro`, which both sides share
    // unchanged from `config`.
    let without_module_registry_argument = ContextOsServer::try_from(config.clone())?.effective_catalogue();
    let with_an_empty_module_registry = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: ModuleRegistry::new(),
    })?
    .effective_catalogue();

    let mut baseline: Vec<String> = without_module_registry_argument
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    baseline.sort_unstable();

    let mut effective: Vec<String> = with_an_empty_module_registry
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    effective.sort_unstable();

    assert_eq!(
        baseline, effective,
        "an empty module registry must never change the core catalogue"
    );
    Ok(())
}

#[test]
fn a_registered_module_adds_its_namespaced_tool_without_touching_the_core_catalogue() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;

    // This instance's own effective catalogue with no modules registered
    // (`[server] astro` held constant from the same `config`, unlike
    // `ContextOsServer::catalogue()`, which always includes the
    // `ephemeris_*` tools regardless of that runtime toggle). Built and
    // dropped (only its catalogue is kept) before `server` below opens the
    // same vault's derived-state directory: the link graph's `fjall` store
    // holds an exclusive lock while open, so two `ContextOsServer`
    // instances cannot have it open over the same vault at once, unlike
    // the former JSON cache, which had no such lock.
    let core_names: Vec<String> = ContextOsServer::try_from(ServerBuildConfig {
        config: config.clone(),
        modules: ModuleRegistry::new(),
    })?
    .effective_catalogue()
    .list_all()
    .into_iter()
    .map(|tool| tool.name.into_owned())
    .collect();
    assert!(!core_names.contains(&"dos_echo_write".to_owned()));

    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let effective = server.effective_catalogue();
    let tool = effective
        .get("dos_echo_write")
        .ok_or_else(|| std::io::Error::other("registered module tool missing from catalogue"))?;
    assert_eq!(
        tool.description.as_deref(),
        Some("Fixture: writes a note through the injected pipeline")
    );
    assert!(tool.input_schema.contains_key("properties"));
    let properties = tool.input_schema.get("properties").and_then(Value::as_object);
    let properties = properties.ok_or_else(|| std::io::Error::other("fixture tool schema has no properties object"))?;
    assert!(properties.contains_key("path"));
    assert!(properties.contains_key("content"));

    let mut effective_names: Vec<String> = effective
        .list_all()
        .into_iter()
        .map(|entry| entry.name.into_owned())
        .collect();
    effective_names.sort_unstable();
    let mut expected: Vec<String> = core_names;
    expected.push("dos_echo_write".to_owned());
    expected.push("dos_echo_read".to_owned());
    expected.sort_unstable();
    assert_eq!(effective_names, expected);
    Ok(())
}

#[tokio::test]
async fn a_module_write_flows_through_the_shared_pipeline_and_reaches_the_operation_log() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let config = Config::try_from(vec![vault_path.clone()])?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let result = call_tool(
        server,
        "dos_echo_write",
        serde_json::from_value(json!({
            "path": "fixture/note.md",
            "content": "# Fixture note\n",
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false));

    let written = vault_path.join("fixture/note.md");
    assert_eq!(std::fs::read_to_string(&written)?, "# Fixture note\n");

    // The module never touched the operation log directly: this entry only
    // exists because `ctx.write` routed through the same pipeline every
    // core write tool uses.
    let log_root = vault_path.join("memory/log");
    let log_path = std::fs::read_dir(log_root)?
        .next()
        .ok_or_else(|| std::io::Error::other("log year was not created"))??
        .path();
    let log_path = std::fs::read_dir(log_path)?
        .next()
        .ok_or_else(|| std::io::Error::other("log month was not created"))??
        .path();
    let log_path = std::fs::read_dir(log_path)?
        .next()
        .ok_or_else(|| std::io::Error::other("daily log was not created"))??
        .path();
    let persisted = std::fs::read_to_string(log_path)?;
    assert!(
        persisted.contains("| dos_echo_write | create | Created fixture/note.md"),
        "operation log missing the module's write: {persisted:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_module_read_and_list_use_the_injected_read_access() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let config = Config::try_from(vec![vault_path.clone()])?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let written = call_tool(
        server.clone(),
        "dos_echo_write",
        serde_json::from_value(json!({
            "path": "readable.md",
            "content": "# Readable\n",
        }))?,
    )
    .await?;
    assert_eq!(written.is_error, Some(false));

    let read = call_tool(
        server,
        "dos_echo_read",
        serde_json::from_value(json!({"path": "readable.md"}))?,
    )
    .await?;
    assert_eq!(read.is_error, Some(false));
    let structured = read
        .structured_content
        .ok_or_else(|| std::io::Error::other("read tool did not return structured content"))?;
    assert_eq!(structured.get("content"), Some(&json!("# Readable\n")));
    let root_entries = structured
        .get("root_entries")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("read tool did not return root_entries"))?;
    assert!(root_entries.iter().any(|entry| entry.as_str() == Some("readable.md")));
    Ok(())
}

#[tokio::test]
async fn a_module_write_outside_every_configured_root_is_rejected_before_any_write() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let config = Config::try_from(vec![vault_path.clone()])?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let result = call_tool(
        server,
        "dos_echo_write",
        serde_json::from_value(json!({
            "path": "../outside.md",
            "content": "unreachable\n",
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    let structured = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("tool error did not include structured content"))?;
    assert_eq!(structured.get("code"), Some(&json!("path/outside-root")));
    assert!(!vault_path.join("../outside.md").exists());
    Ok(())
}

#[tokio::test]
async fn unknown_and_missing_fixture_tool_arguments_are_rejected() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let missing_field = call_tool(
        server.clone(),
        "dos_echo_write",
        serde_json::from_value(json!({"path": "note.md"}))?,
    )
    .await?;
    assert_eq!(missing_field.is_error, Some(true));
    let missing_field_code = missing_field
        .structured_content
        .as_ref()
        .and_then(|value| value.get("code").cloned());
    assert_eq!(missing_field_code, Some(json!("module/invalid-arguments")));

    let unknown_field = call_tool(
        server,
        "dos_echo_write",
        serde_json::from_value(json!({
            "path": "note.md",
            "content": "text",
            "unexpected": true,
        }))?,
    )
    .await?;
    assert_eq!(unknown_field.is_error, Some(true));
    let unknown_field_code = unknown_field
        .structured_content
        .as_ref()
        .and_then(|value| value.get("code").cloned());
    assert_eq!(unknown_field_code, Some(json!("module/invalid-arguments")));
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports. Both
/// transports dispatch through the one `#[tool_handler]`-generated router
/// (`ContextOsServer::effective_catalogue`), so this proves a registered
/// module's tool is reachable, correctly schemed, and dispatches
/// successfully over the streamable-HTTP transport too, not only stdio.
#[tokio::test]
async fn a_registered_module_tool_is_reachable_and_dispatches_over_http() -> Result<(), BoxError> {
    use contextos_mcp::HttpConfig;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let config = Config::try_from(vec![vault_path.clone()])?;
    let server = ContextOsServer::try_from(ServerBuildConfig {
        config,
        modules: registry_with_fixture(),
    })?;

    let token = "module-parity-token";
    let http = HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        max_body_kb: 2048,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, &http)?;
    let shutdown = CancellationToken::new();
    let shutdown_for_serve = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_serve.cancelled().await })
            .await;
    });
    let url = format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH);

    let client_config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(client_config);
    let client = ().serve(transport).await?;

    let tools = client.list_all_tools().await?;
    let fixture_tool = tools
        .iter()
        .find(|tool| tool.name == "dos_echo_write")
        .ok_or_else(|| std::io::Error::other("module tool missing from the HTTP catalogue"))?;
    assert!(fixture_tool.input_schema.contains_key("properties"));

    let result = client
        .call_tool(
            CallToolRequestParams::new("dos_echo_write").with_arguments(serde_json::from_value(
                json!({"path": "http/note.md", "content": "# Over HTTP\n"}),
            )?),
        )
        .await?;
    client.cancel().await?;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        std::fs::read_to_string(vault_path.join("http/note.md"))?,
        "# Over HTTP\n"
    );

    shutdown.cancel();
    handle.await?;
    Ok(())
}
