# CLI reference

## Global flags

These apply to every invocation, including the bare server run:

| Flag | Effect |
| --- | --- |
| `--config <path>` | Load configuration from this TOML file instead of the default location |
| `--vault <path>` | Add an allowed vault directory; may repeat |
| `--log-level <level>` | Override the configured runtime log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `--http [addr]` | Enable the HTTP transport for this run, optionally overriding the configured bind address (for example `--http 127.0.0.1:9000`); with no address, the configured or default bind is used unchanged |
| `--astro` | Register the ephemeris tools for this run, overriding `[server] astro` in config; enable-only, there is no `--no-astro` |

## `contextos`

With no subcommand, starts the MCP server on the configured transport(s) and blocks until shut down (`Ctrl-C` or `SIGTERM`).

## `contextos doctor [--resolve] [--dry-run]`

Reports actionable, read-only health checks: configuration validity, per-vault reachability, managed index staleness, Git recovery state, and semantic search health. Never writes to a vault by itself.

- `--resolve` — additionally resolve every currently auto-fixable finding (a stale or missing managed index, or an absent Git repository). Mirrors the `doctor_resolve` MCP tool.
- `--dry-run` — with `--resolve`, report what would be resolved without writing anything.

## `contextos index`

Rebuilds every enabled vault search index (text and link graph) for the configured vaults.

## `contextos model <list|download>`

Manages the shared local embedding model cache. Vault-independent — works without `--config` or `--vault`.

- `list` — report the default local embedding model's cache status.
- `download` — fetch the default local embedding model into the shared cache.

## `contextos config`

With no further subcommand, runs the interactive guided-setup interview (vault selection, model download, host registration). Vault-independent: it edits `config.toml` directly and works against a not-yet-valid or not-yet-existing file. When `config.toml` already has vault(s) configured, the interview loads them first instead of only offering to add more: a single existing vault is offered back for edit with its current name, path, managed flag, and semantic-search setting prefilled as defaults, and more than one existing vault instead asks what to focus on — general (server) settings, all vaults in turn, or one named vault to edit or remove — before optionally adding a new vault.

### `contextos config vault <add|remove|list>`

Manages `[[vault]]` entries in `config.toml`.

- `add <name> <path> [--unmanaged]` — add a vault. `name` addresses it as `name://relative-path`; `--unmanaged` marks it filesystem-only (mutating tools reject writes, and managed indexes, the oplog, and Git recovery are disabled).
- `remove <name>` — remove a configured vault by name.
- `list` — list configured vaults.

### `contextos config mcp <register|deregister|status>`

Manages this server's entry in an MCP host's own configuration file. Currently supports `--host claude-desktop`; pass `--config-path` explicitly if host discovery reports more than one candidate configuration file, or none.

- `register --host <host> [--config-path <path>] [--force]` — add or update the entry. `--force` proceeds even though the host is detected running (logging a warning) instead of refusing.
- `deregister --host <host> [--config-path <path>] [--force]` — remove the entry.
- `status --host <host> [--config-path <path>]` — report whether the server is currently registered, without writing.
