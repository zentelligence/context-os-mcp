//! `ephemeris_moon_phase`, `ephemeris_solar_events`, `ephemeris_wheel_of_year`,
//! `ephemeris_personal_year_period`, `ephemeris_boundaries` (`FR-101` to
//! `FR-105`, Phase 10): schema-valid MCP surface over `contextos-ephemeris`.
//! Every ephemeris tool handler is always compiled in; visibility is gated
//! at runtime by `[server] astro` (`D-25`), not a Cargo feature, so this
//! whole file runs under a plain `cargo test -p contextos-mcp`, unlike
//! `contextos-search/tests/embedding_fastembed.rs`'s feature-gated
//! precedent (that capability genuinely is compile-time optional; this one
//! is not). Every test that exercises tool behaviour builds its own
//! astro-enabled server via [`server_with_astro_enabled`]; a dedicated test
//! below confirms the tools are absent with astro left at its default.

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Map, Value, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn call_tool_arguments(value: &Value) -> Result<Map<String, Value>, BoxError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object").into())
}

/// A server with a single, otherwise-empty vault and `[server] astro`
/// enabled: the ephemeris tools touch no vault content at all, but
/// `ContextOsServer` still requires at least one configured root to
/// construct.
fn server_with_astro_enabled(vault: &std::path::Path) -> Result<ContextOsServer, BoxError> {
    let mut config = Config::try_from(vec![vault.to_path_buf()])?;
    config.server.astro = true;
    Ok(ContextOsServer::try_from(config)?)
}

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<rmcp::model::CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
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

/// `D-25`: with `[server] astro` left at its default (`false`), none of
/// the five ephemeris tools are advertised, even though every handler is
/// compiled into this binary. The complementary case (`astro = true`
/// advertises all five) is covered by the next test.
#[tokio::test]
async fn ephemeris_tools_are_not_advertised_with_astro_left_at_its_default() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(Config::try_from(vec![vault.path().to_path_buf()])?)?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;

    let tools = client.list_all_tools().await?;
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(
        !names.iter().any(|name| name.starts_with("ephemeris_")),
        "no ephemeris_* tool should be advertised with astro at its default, got {names:?}"
    );

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// `mcp-contracts.md` checklist item 1: advertised name and schema. All
/// five ephemeris tools are present, each with a non-empty description,
/// once `[server] astro` is enabled for this instance (`D-25`).
#[tokio::test]
async fn ephemeris_tools_are_advertised_when_astro_is_enabled() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;

    let tools = client.list_all_tools().await?;
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in [
        "ephemeris_moon_phase",
        "ephemeris_solar_events",
        "ephemeris_wheel_of_year",
        "ephemeris_personal_year_period",
        "ephemeris_boundaries",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from the advertised catalogue: {names:?}"
        );
    }
    for tool in &tools {
        if tool.name.starts_with("ephemeris_") {
            assert!(
                tool.description
                    .as_ref()
                    .is_some_and(|description| !description.is_empty()),
                "{} must advertise a non-empty description",
                tool.name
            );
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

/// The same Meeus-cited 1977 New Moon fixture `contextos-ephemeris`'s own
/// unit tests verify (JD 2443192.65118, ~03:38 UT): confirms the MCP
/// boundary reports identical content to the domain layer, not just that
/// the domain layer itself is correct.
#[tokio::test]
async fn fr_101_ephemeris_moon_phase_reports_the_verified_new_moon_fixture() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_moon_phase",
        call_tool_arguments(&json!({ "date": "1977-02-18" }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    assert_eq!(content.get("name"), Some(&json!("new")));
    assert_eq!(content.get("near_new"), Some(&json!(true)));
    let illumination = content
        .get("illumination_fraction")
        .and_then(Value::as_f64)
        .ok_or_else(|| std::io::Error::other("missing illumination_fraction"))?;
    assert!(
        illumination < 0.05,
        "illumination so close to New should be near zero, was {illumination}"
    );
    Ok(())
}

#[tokio::test]
async fn fr_101_ephemeris_moon_phase_rejects_a_malformed_date() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_moon_phase",
        call_tool_arguments(&json!({ "date": "not-a-date" }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("io/invalid-argument"))
    );
    Ok(())
}

/// `mcp-contracts.md` checklist item 5: unknown fields are rejected
/// (`#[serde(deny_unknown_fields)]`), not silently ignored.
#[tokio::test]
async fn ephemeris_moon_phase_rejects_an_unknown_input_field() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_moon_phase",
        call_tool_arguments(&json!({ "date": "1977-02-18", "unexpected": true }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    Ok(())
}

#[tokio::test]
async fn fr_102_ephemeris_solar_events_returns_four_events_in_chronological_order()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_solar_events",
        call_tool_arguments(&json!({ "year": 2024 }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    let events = content
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing events array"))?;
    assert_eq!(events.len(), 4);

    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("kind").and_then(Value::as_str))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "march_equinox",
            "june_solstice",
            "september_equinox",
            "december_solstice"
        ]
    );

    let instants: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("instant").and_then(Value::as_str))
        .collect();
    assert_eq!(instants.len(), 4, "every event must carry an instant");
    for instant in &instants {
        assert!(
            time::OffsetDateTime::parse(instant, &time::format_description::well_known::Rfc3339)
                .is_ok(),
            "{instant} is not valid RFC 3339"
        );
    }
    let mut sorted = instants.clone();
    sorted.sort_unstable();
    assert_eq!(instants, sorted, "events must already be chronological");
    Ok(())
}

#[tokio::test]
async fn fr_102_ephemeris_solar_events_rejects_an_out_of_range_year() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_solar_events",
        call_tool_arguments(&json!({ "year": 50_000 }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("ephemeris/year-out-of-range"))
    );
    Ok(())
}

/// `FR-103`'s own worked example: a June solstice is Southern Hemisphere
/// `winter_solstice` by name, at the same position `ephemeris_solar_events`
/// (`FR-102`) independently reports as `june_solstice`.
#[tokio::test]
async fn fr_103_ephemeris_wheel_of_year_hemisphere_correctly_names_points() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_wheel_of_year",
        call_tool_arguments(&json!({ "year": 2024, "hemisphere": "southern" }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    let points = content
        .get("points")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing points array"))?;
    assert_eq!(points.len(), 8);

    // Index 3 is the June solstice boundary (Imbolc, equinox, Beltane,
    // solstice, ...); Southern Hemisphere names it winter_solstice.
    assert_eq!(points[3].get("name"), Some(&json!("winter_solstice")));
    assert_eq!(points[3].get("role"), Some(&json!("boundary")));
    Ok(())
}

#[tokio::test]
async fn fr_103_ephemeris_wheel_of_year_rejects_an_unknown_hemisphere_value() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_wheel_of_year",
        call_tool_arguments(&json!({ "year": 2024, "hemisphere": "north" }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    Ok(())
}

#[tokio::test]
async fn fr_104_ephemeris_personal_year_period_reports_period_1_at_the_birthday()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_personal_year_period",
        call_tool_arguments(&json!({
            "birth_date": "1990-06-15",
            "as_of_date": "1990-06-15"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    assert_eq!(content.get("period_number"), Some(&json!(1)));
    assert_eq!(content.get("ruling_planet"), Some(&json!("sun")));
    assert_eq!(content.get("transition"), Some(&json!(true)));
    Ok(())
}

/// `FR-105`: with both `hemisphere` and `birth_date` supplied, all three
/// event kinds can appear in one aggregate call.
#[tokio::test]
async fn fr_105_ephemeris_boundaries_aggregates_multiple_event_kinds() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_boundaries",
        call_tool_arguments(&json!({
            "start_date": "2024-03-01",
            "end_date": "2024-04-01",
            "birth_date": "1990-06-15",
            "hemisphere": "southern"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    let events = content
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing events array"))?;
    assert!(
        events
            .iter()
            .any(|event| event.get("kind") == Some(&json!("moon_quarter"))),
        "expected at least one moon_quarter event in a full month, got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.get("kind") == Some(&json!("wheel_of_year"))),
        "expected the March equinox (wheel_of_year), got {events:?}"
    );

    let instants: Vec<&str> = events
        .iter()
        .filter_map(|event| event.get("instant").and_then(Value::as_str))
        .collect();
    let mut sorted = instants.clone();
    sorted.sort_unstable();
    assert_eq!(instants, sorted, "events must already be chronological");
    Ok(())
}

/// A caller supplying no `hemisphere`/`birth_date` gets a valid, narrower
/// result (moon quarters only), never an error.
#[tokio::test]
async fn fr_105_ephemeris_boundaries_with_no_optional_data_still_succeeds() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_boundaries",
        call_tool_arguments(&json!({
            "start_date": "1977-02-17",
            "end_date": "1977-02-27"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .ok_or_else(|| std::io::Error::other("missing structured content"))?;
    let events = content
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing events array"))?;
    assert!(
        events
            .iter()
            .all(|event| event.get("kind") == Some(&json!("moon_quarter"))),
        "with no hemisphere/birth_date, only moon_quarter events may appear, got {events:?}"
    );
    assert!(
        !events.is_empty(),
        "expected the verified New Moon in range"
    );
    Ok(())
}

#[tokio::test]
async fn fr_105_ephemeris_boundaries_rejects_an_inverted_date_range() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;
    let result = call_tool(
        server,
        "ephemeris_boundaries",
        call_tool_arguments(&json!({
            "start_date": "2024-06-20",
            "end_date": "2024-06-10"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("code")),
        Some(&json!("ephemeris/invalid-date-range"))
    );
    Ok(())
}

/// `mcp-contracts.md` checklist item 7: parity across transports.
#[tokio::test]
async fn ephemeris_moon_phase_is_reachable_over_the_streamable_http_transport_too()
-> Result<(), BoxError> {
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_with_astro_enabled(vault.path())?;

    let token = "ephemeris-parity-token";
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

    let result = client
        .call_tool(
            CallToolRequestParams::new("ephemeris_moon_phase")
                .with_arguments(call_tool_arguments(&json!({ "date": "1977-02-18" }))?),
        )
        .await?;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("name")),
        Some(&json!("new"))
    );

    client.cancel().await?;
    shutdown.cancel();
    handle.await?;
    Ok(())
}
