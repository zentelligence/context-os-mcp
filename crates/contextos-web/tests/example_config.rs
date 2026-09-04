//! Keeps `docs/web.example.toml` honest: it must parse against
//! [`contextos_web::WebConfig`]'s real schema, not merely look plausible.

#[test]
fn the_shipped_example_web_toml_parses() -> Result<(), Box<dyn std::error::Error>> {
    let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/web.example.toml");

    let config = contextos_web::load_web_config(std::path::Path::new(example_path))?;

    assert_eq!(config.server.bind, "127.0.0.1:7332");
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].name(), "contextos");
    Ok(())
}
