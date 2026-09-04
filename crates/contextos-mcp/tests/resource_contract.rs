//! `FR-65`/`FR-80`: every eligible vault file (not only `.md`) exposed as
//! MCP resources (list and read) for hosts that support resource
//! attachment: root-confined, read-only, and size-capped per `NFR-08`,
//! excluding any path matching the vault's `hidden` configuration
//! (`FR-84`). Binary content (`FR-81`) is Stage 3 scope; a listed binary
//! file is not yet readable via `resources/read` here. Exercised through
//! a real MCP transport (`.claude/rules/mcp-contracts.md`: tests invoke
//! the adapter, not the underlying service).

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Connects an in-process MCP client to `server` over a duplex pipe, framed
/// exactly like stdio, mirroring `module_contract.rs`'s own transport
/// harness.
async fn connect(
    server: ContextOsServer,
) -> Result<
    (
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
        tokio::task::JoinHandle<Result<(), BoxError>>,
    ),
    BoxError,
> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ().serve(client_transport).await?;
    Ok((client, server_handle))
}

/// Builds a `{name}://{absolute-path}` URI (`FR-99`) for a test that needs
/// to hand-construct a resource URI reaching outside what `resources/list`
/// would ever itself emit (an escape attempt, or a path the test controls
/// directly by absolute location rather than by vault-relative name). The
/// absolute remainder still has to resolve within the named root
/// (`FR-97`'s `resolve_within_named_root`), exactly like a plain absolute
/// `path` argument would on any other tool.
fn named_uri(name: &str, path: &std::path::Path) -> String {
    format!("{name}://{}", path.display())
}

/// A vault config with an explicit, known `name` (`FR-96`), for a test that
/// needs to hand-build a `{name}://` URI deterministically rather than
/// discover the default name-from-basename at runtime. `resources_list_include`
/// is set to `**/*` (`FR-107`): every test using this helper cares about
/// `resources/read`, `resources/templates/list`, or URI construction, never
/// about `resources/list`'s own narrowed enumeration scope, which has its
/// own dedicated tests below using bespoke configs instead.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn named_vault_config(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!(
            "[[vault]]\npath = {vault:?}\nname = \"vault\"\nresources_list_include = [\"**/*\"]\n"
        )
        .as_str(),
    )?)
}

/// As [`named_vault_config`], but without an explicit `name` (so the
/// resource-URI scheme defaults to the root directory's basename), for a
/// test that only needs `resources/list` to actually enumerate files, not a
/// deterministic name.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn config_allowing_all_resources(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!("[[vault]]\npath = {vault:?}\nresources_list_include = [\"**/*\"]\n").as_str(),
    )?)
}

/// A vault config with an explicit, known `name`, but no
/// `resources_list_include` (`FR-107`'s empty default left in place), for
/// a test that needs a deterministic `{name}://` URI while still proving
/// the unconfigured `resources/list` behaviour.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn named_vault_config_without_resources_list(vault: &std::path::Path) -> Result<Config, BoxError> {
    Ok(Config::try_from(
        format!("[[vault]]\npath = {vault:?}\nname = \"vault\"\n").as_str(),
    )?)
}

/// `FR-107`/`D-24`: with no `resources_list_include` configured,
/// `resources/list` reports nothing, even though real, otherwise-eligible
/// files exist in the vault. This is the actual fix for the original
/// complaint (a vault holding thousands of files has little value in an
/// eager, unconditional dump); `resources/read` remains fully able to
/// serve any of these files directly, unaffected by this default.
#[tokio::test]
async fn fr_107_with_no_include_patterns_configured_resources_list_reports_nothing()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::write(vault.path().join("registry.md"), "# Registry\n")?;

    let config = named_vault_config_without_resources_list(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    assert_eq!(
        listed.resources,
        Vec::new(),
        "an unconfigured vault must list nothing, per D-24's default"
    );

    // `resources/read` still serves the file directly, by hand-built URI,
    // proving the empty list is a discovery-scope decision, not a
    // read-access restriction (the same `hidden` independence `D-13`
    // already established for the older exclude-list mechanism).
    let uri = named_uri("vault", std::path::Path::new("note.md"));
    let read = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => assert_eq!(text, "# Note\n"),
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `FR-107`/`D-24`: a configured include pattern scopes `resources/list`
/// to only the matching files, leaving other real, otherwise-eligible
/// files in the same vault unlisted, the "targeted list of selected
/// files" the allowlist exists to produce.
#[tokio::test]
async fn fr_107_resources_list_only_enumerates_files_matching_configured_include_patterns()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("registry"))?;
    std::fs::write(vault.path().join("registry/skill.md"), "# Skill\n")?;
    std::fs::create_dir_all(vault.path().join("journal"))?;
    std::fs::write(vault.path().join("journal/entry.md"), "# Entry\n")?;

    let config = Config::try_from(
        format!(
            "[[vault]]\npath = {:?}\nresources_list_include = [\"registry/**\"]\n",
            vault.path()
        )
        .as_str(),
    )?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let names: Vec<&str> = listed.resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["registry/skill.md"],
        "only the configured registry/** pattern should be enumerated; \
         journal/entry.md is a real, eligible file but outside every \
         configured pattern"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `FR-84`'s `hidden` exclusion still applies within a configured
/// `resources_list_include` allowlist: the two pattern axes stay
/// independent, matching the same independence proof Phase 7's own gate
/// already required between `hidden` and the other pattern axes.
#[tokio::test]
async fn fr_107_a_broad_include_pattern_still_excludes_hidden_patterns() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    std::fs::create_dir_all(vault.path().join("nested"))?;
    std::fs::write(vault.path().join("nested/child.md"), "# Child\n")?;
    std::fs::write(vault.path().join("not-markdown.txt"), "ignore me")?;
    std::fs::create_dir_all(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join(".contextos/derived.md"), "# derived\n")?;
    std::fs::create_dir_all(vault.path().join(".git"))?;
    std::fs::write(vault.path().join(".git/COMMIT_EDITMSG.md"), "# git\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let mut names: Vec<String> = listed.resources.iter().map(|r| r.name.clone()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "nested/child.md".to_owned(),
            "not-markdown.txt".to_owned(),
            "note.md".to_owned(),
        ],
        "not-markdown.txt is eligible (FR-80); .contextos and .git stay \
         hidden (FR-84) even under a broad **/* include pattern"
    );
    for resource in &listed.resources {
        let is_markdown = std::path::Path::new(&resource.name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        let expected_mime = if is_markdown {
            "text/markdown"
        } else {
            "text/plain"
        };
        assert_eq!(resource.mime_type.as_deref(), Some(expected_mime));
        assert!(
            resource.uri.starts_with("vault://"),
            "unexpected resource uri: {}",
            resource.uri
        );
        assert!(resource.size.is_some());
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_directory_is_never_listed_as_a_resource() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/note.md"), "# Note\n")?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let names: Vec<&str> = listed.resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["notes/note.md"],
        "the 'notes' directory itself must never appear as a resource"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_serves_a_png_as_base64_blob_with_correct_mime_type() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3])?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "image.png")
        .ok_or_else(|| std::io::Error::other("image.png was not listed"))?;
    assert_eq!(resource.mime_type.as_deref(), Some("image/png"));

    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert_eq!(read.contents.len(), 1);
    match &read.contents[0] {
        ResourceContents::BlobResourceContents {
            blob, mime_type, ..
        } => {
            assert_eq!(blob, "AAECAw==");
            assert_eq!(mime_type.as_deref(), Some("image/png"));
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected blob resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_serves_an_unknown_extension_as_octet_stream_blob() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("mystery.zzzznotreal"),
        [0_u8, 159, 146, 150],
    )?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "mystery.zzzznotreal")
        .ok_or_else(|| std::io::Error::other("mystery.zzzznotreal was not listed"))?;
    assert_eq!(
        resource.mime_type.as_deref(),
        Some("application/octet-stream"),
        "no format allow-list: an unrecognised extension still gets listed and read"
    );

    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert!(matches!(
        &read.contents[0],
        ResourceContents::BlobResourceContents { mime_type, .. }
            if mime_type.as_deref() == Some("application/octet-stream")
    ));

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_binary_content_matches_fs_attach_files_existing_encoding()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3, 255, 254])?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource_uri = listed
        .resources
        .iter()
        .find(|resource| resource.name == "image.png")
        .ok_or_else(|| std::io::Error::other("image.png was not listed"))?
        .uri
        .clone();
    let read = client
        .read_resource(ReadResourceRequestParams::new(resource_uri))
        .await?;
    let (resource_blob, resource_mime) = match &read.contents[0] {
        ResourceContents::BlobResourceContents {
            blob, mime_type, ..
        } => (blob.clone(), mime_type.clone()),
        other => {
            return Err(std::io::Error::other(format!(
                "expected blob resource contents, got {other:?}"
            ))
            .into());
        }
    };

    let arguments = serde_json::json!({ "path": "image.png" })
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object"))?;
    let attached = client
        .call_tool(CallToolRequestParams::new("fs_attach_file").with_arguments(arguments))
        .await?;
    let attached_resource = attached
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("fs_attach_file did not embed a resource"))?;
    let (attached_blob, attached_mime) = match &attached_resource.resource {
        ResourceContents::BlobResourceContents {
            blob, mime_type, ..
        } => (blob.clone(), mime_type.clone()),
        other => {
            return Err(std::io::Error::other(format!(
                "expected blob resource contents, got {other:?}"
            ))
            .into());
        }
    };

    assert_eq!(
        resource_blob, attached_blob,
        "resources/read and fs_attach_file must encode identical bytes for the same file"
    );
    assert_eq!(resource_mime, attached_mime);

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_rejects_a_binary_file_over_the_attachment_size_cap() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let oversized = std::fs::File::create(vault.path().join("oversized.bin"))?;
    oversized.set_len(10 * 1024 * 1024 + 1)?;

    // `NFR-08`'s configurable text cap defaults to 5 MB and would reject
    // this file before binary detection even runs; raise it so the read
    // reaches binary detection and actually exercises `read_attachment`'s
    // own, independent, fixed 10 MiB cap.
    let source = format!(
        "[[vault]]\npath = {:?}\nresources_list_include = [\"**/*\"]\n[vault.limits]\nmax_read_mb = 15\n",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .iter()
        .find(|resource| resource.name == "oversized.bin")
        .ok_or_else(|| std::io::Error::other("oversized.bin was not listed"))?;
    let result = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await;
    assert!(
        result.is_err(),
        "a binary resource over read_attachment's fixed 10 MiB cap must be rejected"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_attach_file_and_resources_read_agree_on_the_uri_for_the_same_path()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let listed_uri = listed
        .resources
        .first()
        .ok_or_else(|| std::io::Error::other("no resources listed"))?
        .uri
        .clone();

    let arguments = serde_json::json!({ "path": "note.md" })
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object"))?;
    let attached = client
        .call_tool(CallToolRequestParams::new("fs_attach_file").with_arguments(arguments))
        .await?;
    let attached_resource = attached
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("fs_attach_file did not embed a resource"))?;
    let attached_uri = match &attached_resource.resource {
        ResourceContents::TextResourceContents { uri, .. }
        | ResourceContents::BlobResourceContents { uri, .. } => uri.clone(),
        other => {
            return Err(
                std::io::Error::other(format!("unexpected resource contents: {other:?}")).into(),
            );
        }
    };

    assert!(attached_uri.starts_with("vault://"));
    assert_eq!(
        attached_uri, listed_uri,
        "fs_attach_file and resources/read must address the same file with the same URI (D-17, superseding D-14)"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn resources_read_returns_the_exact_file_content_by_the_listed_uri() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n\nBody text.\n")?;

    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let listed = client.list_resources(None).await?;
    let resource = listed
        .resources
        .first()
        .ok_or_else(|| std::io::Error::other("no resources listed"))?;
    let read = client
        .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
        .await?;
    assert_eq!(read.contents.len(), 1);
    match &read.contents[0] {
        ResourceContents::TextResourceContents {
            text,
            mime_type,
            uri,
            ..
        } => {
            assert_eq!(text, "# Note\n\nBody text.\n");
            assert_eq!(mime_type.as_deref(), Some("text/markdown"));
            assert_eq!(uri, &resource.uri);
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_resource_uri_outside_every_configured_root_is_rejected() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "# secret\n")?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let escaping_uri = named_uri("vault", &outside.path().join("secret.md"));
    let result = client
        .read_resource(ReadResourceRequestParams::new(escaping_uri))
        .await;
    assert!(
        result.is_err(),
        "reading a resource outside every configured root must fail, not succeed"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_resource_reached_only_through_a_symlink_escaping_the_root_is_rejected()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "# secret\n")?;
    std::os::unix::fs::symlink(outside.path(), vault.path().join("linked-outside"))?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", &vault.path().join("linked-outside/secret.md"));
    let result = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await;
    assert!(
        result.is_err(),
        "a resource reached only through a symlink escaping the vault root must be rejected"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn a_resource_over_the_configured_size_cap_is_rejected_without_partial_content()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let big = "a".repeat(2 * 1024 * 1024);
    std::fs::write(vault.path().join("big.md"), &big)?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"vault\"\n[vault.limits]\nmax_read_mb = 1\n",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let uri = named_uri("vault", &vault.path().join("big.md"));
    let result = client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await;
    assert!(
        result.is_err(),
        "a resource over the configured size cap must be rejected, not truncated"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn get_info_advertises_the_resources_capability() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let peer_info = client
        .peer_info()
        .ok_or_else(|| std::io::Error::other("no peer info negotiated"))?;
    assert!(peer_info.capabilities.resources.is_some());
    assert!(peer_info.capabilities.tools.is_some());

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports.
#[tokio::test]
async fn resources_are_reachable_over_the_streamable_http_transport_too() -> Result<(), BoxError> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    let config = config_allowing_all_resources(vault.path())?;
    let server = ContextOsServer::try_from(config)?;

    let token = "resource-parity-token";
    let http = contextos_mcp::HttpConfig {
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

    let client_config =
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(client_config);
    let client = ().serve(transport).await?;

    let listed = client.list_resources(None).await?;
    assert_eq!(listed.resources.len(), 1);
    let read = client
        .read_resource(ReadResourceRequestParams::new(
            listed.resources[0].uri.clone(),
        ))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, "# Note\n");
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.cancel().await?;
    shutdown.cancel();
    handle.await?;
    Ok(())
}

// Debug-formatting the path (rather than `Display`/`.display()`) is
// required here, not just conventional: TOML string values need the
// surrounding quotes and escaping `Debug` provides, which `Display` would
// strip, breaking the generated config for any path with special
// characters.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn threshold_config(vault: &std::path::Path, threshold_kb: u64) -> Result<Config, BoxError> {
    let source = format!(
        "[[vault]]\npath = {vault:?}\nname = \"vault\"\n[server]\nresource_link_threshold_kb = {threshold_kb}\n"
    );
    Ok(Config::try_from(source.as_str())?)
}

fn call_tool_arguments(
    value: &serde_json::Value,
) -> Result<serde_json::Map<String, serde_json::Value>, BoxError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object").into())
}

#[tokio::test]
async fn fs_read_text_file_below_threshold_returns_full_content_with_no_resource_link()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // Exactly one byte under the 1 KB threshold, so this is the tightest
    // possible "still below" boundary case.
    let content = "a".repeat(1024 - 1);
    std::fs::write(vault.path().join("note.md"), &content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_text_file").with_arguments(call_tool_arguments(
                &serde_json::json!({ "path": "note.md" }),
            )?),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str),
        Some(content.as_str()),
        "below threshold, content must be returned in full, unchanged"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("truncated"))
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        result.content.len(),
        1,
        "below threshold, no resource_link content block is attached"
    );
    assert!(rmcp::model::ContentBlock::as_resource_link(&result.content[0]).is_none());

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_read_text_file_above_threshold_attaches_a_resource_link_whose_uri_reads_back_identical_content()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let content = "line one\n".repeat(200); // well over 1 KB
    std::fs::write(vault.path().join("note.md"), &content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_text_file").with_arguments(call_tool_arguments(
                &serde_json::json!({ "path": "note.md" }),
            )?),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    let preview = result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("missing content field"))?
        .to_owned();
    assert!(
        preview.len() < content.len(),
        "at or above threshold, the inline preview must be shorter than the full file"
    );
    assert!(
        content.starts_with(&preview),
        "the preview must be a genuine prefix of the real content, not a stub"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("truncated"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let link = result
        .content
        .iter()
        .find_map(rmcp::model::ContentBlock::as_resource_link)
        .ok_or_else(|| std::io::Error::other("no resource_link content block was attached"))?;
    assert!(link.uri.starts_with("vault://"));

    let read = client
        .read_resource(ReadResourceRequestParams::new(link.uri.clone()))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(
                text, &content,
                "the resource_link must resolve to the complete, untruncated content"
            );
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_read_multiple_files_attaches_per_file_resource_links_independently()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let small_content = "small";
    let large_content = "line one\n".repeat(200);
    std::fs::write(vault.path().join("small.md"), small_content)?;
    std::fs::write(vault.path().join("large.md"), &large_content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_multiple_files").with_arguments(
                call_tool_arguments(&serde_json::json!({ "paths": ["small.md", "large.md"] }))?,
            ),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    let files = result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("files"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing files field"))?;
    let small = files
        .iter()
        .find(|file| file.get("path").and_then(serde_json::Value::as_str) == Some("small.md"))
        .ok_or_else(|| std::io::Error::other("small.md missing from batch result"))?;
    let large = files
        .iter()
        .find(|file| file.get("path").and_then(serde_json::Value::as_str) == Some("large.md"))
        .ok_or_else(|| std::io::Error::other("large.md missing from batch result"))?;

    assert_eq!(
        small.get("content").and_then(serde_json::Value::as_str),
        Some(small_content)
    );
    assert_eq!(
        small.get("truncated").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        large.get("truncated").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let large_preview = large
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("large.md missing content"))?;
    assert!(large_preview.len() < large_content.len());

    // Exactly one resource_link, for the one file that crossed the
    // threshold: the small file's own headroom must not spuriously
    // attach a second link.
    let links: Vec<_> = result
        .content
        .iter()
        .filter_map(rmcp::model::ContentBlock::as_resource_link)
        .collect();
    assert_eq!(links.len(), 1);

    let read = client
        .read_resource(ReadResourceRequestParams::new(links[0].uri.clone()))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, &large_content);
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `FR-106`/`D-23`: `resources/templates/list` advertises one
/// `{name}://{+path}` template per configured vault, so a client can
/// construct a valid `resources/read` URI without first calling
/// `resources/list`, without changing `resources/list` itself.
#[tokio::test]
async fn fr_106_resources_templates_list_advertises_one_template_per_configured_vault()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    assert_eq!(templates.resource_templates.len(), 1);
    let template = &templates.resource_templates[0];
    assert_eq!(template.uri_template, "vault://{+path}");
    assert_eq!(template.name, "vault");

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fr_106_a_multi_vault_configuration_advertises_a_distinct_template_per_vault()
-> Result<(), BoxError> {
    let mine = tempfile::Builder::new().prefix("mine").tempdir()?;
    let family = tempfile::Builder::new().prefix("family").tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\nname = \"mine\"\n[[vault]]\npath = {:?}\nname = \"family\"\n",
        mine.path(),
        family.path()
    );
    let server = ContextOsServer::try_from(Config::try_from(source.as_str())?)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    let mut uri_templates: Vec<String> = templates
        .resource_templates
        .iter()
        .map(|template| template.uri_template.clone())
        .collect();
    uri_templates.sort_unstable();
    assert_eq!(
        uri_templates,
        vec!["family://{+path}".to_owned(), "mine://{+path}".to_owned()],
        "each configured vault must be named by its own template, never the other's"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fr_106_substituting_the_template_produces_a_uri_that_reads_back_identical_content()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("nested"))?;
    let content = "# Nested note\n";
    std::fs::write(vault.path().join("nested/note.md"), content)?;

    let config = named_vault_config(vault.path())?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let templates = client.list_resource_templates(None).await?;
    let template = &templates.resource_templates[0];
    // RFC 6570 reserved expansion (`{+path}`): substituting a relative
    // path with no characters needing extra percent-encoding is exact
    // string replacement, mirroring how `resource_uri` already builds a
    // concrete URI for a real file.
    let constructed_uri = template.uri_template.replace("{+path}", "nested/note.md");

    let listed = client.list_resources(None).await?;
    let listed_uri = listed
        .resources
        .iter()
        .find(|resource| resource.name == "nested/note.md")
        .map(|resource| resource.uri.clone())
        .ok_or_else(|| std::io::Error::other("nested/note.md missing from resources/list"))?;
    assert_eq!(
        constructed_uri, listed_uri,
        "a URI built from the advertised template must match the one resources/list emits"
    );

    let read = client
        .read_resource(ReadResourceRequestParams::new(constructed_uri))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, content);
        }
        other => {
            return Err(std::io::Error::other(format!(
                "expected text resource contents, got {other:?}"
            ))
            .into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports.
#[tokio::test]
async fn fr_106_resource_templates_are_reachable_over_the_streamable_http_transport_too()
-> Result<(), BoxError> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = Config::try_from(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;

    let token = "resource-templates-parity-token";
    let http = contextos_mcp::HttpConfig {
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

    let client_config =
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(client_config);
    let client = ().serve(transport).await?;

    let templates = client.list_resource_templates(None).await?;
    assert_eq!(templates.resource_templates.len(), 1);

    client.cancel().await?;
    shutdown.cancel();
    handle.await?;
    Ok(())
}
