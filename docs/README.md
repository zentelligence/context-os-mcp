# Documentation

Usage documentation for the `contextos` MCP server.

- [`installation.md`](installation.md) — building, installing, and first run
- [`configuration.md`](configuration.md) — `config.toml` field reference (see also [`config.example.toml`](config.example.toml), a complete annotated example)
- [`cli-reference.md`](cli-reference.md) — every `contextos` subcommand
- [`mcp-tools.md`](mcp-tools.md) — the MCP tools the server registers

## API and code reference

Rustdoc comments on every public type and function are the source of truth for implementation detail. Generate a browsable copy locally with:

```sh
cargo doc --workspace --no-deps --open
```

Nothing generated is committed to this repository.
