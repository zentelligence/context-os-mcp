# Installation

## Prerequisites

A recent stable Rust toolchain. Check the minimum supported version in `rust-version` under `[workspace.package]` in the repository's `Cargo.toml`.

## Build

From a checkout of this repository:

```sh
cargo build --workspace --release
```

or install the `contextos` binary onto your `PATH`:

```sh
cargo install --path crates/contextos-mcp
```

## First run

The fastest path is the guided-setup wizard:

```sh
contextos config
```

It walks through choosing or creating a vault, downloading the default local embedding model (optional, for semantic search), and registering `contextos` with a supported MCP host (currently Claude Desktop). It writes `config.toml` to the platform default location (`~/.config/contextos/config.toml` on every platform, including Windows and macOS) unless `--config` or the `CONTEXTOS_MCP_CONFIG` environment variable points somewhere else.

To configure by hand instead, copy [`config.example.toml`](config.example.toml) and edit it — see [`configuration.md`](configuration.md) for the field reference — then run:

```sh
contextos --config /path/to/config.toml
```

or point at one or more vault directories directly, without a configuration file:

```sh
contextos --vault /path/to/vault
```

## Verify the setup

```sh
contextos doctor
```

reports configuration validity, per-vault reachability, managed index staleness, Git recovery state, and semantic search health, without writing anything. Add `--resolve` to have it fix everything it can (a stale or missing managed index, or an absent Git repository) automatically; add `--dry-run` alongside `--resolve` to preview what would change first.

## Registering with an MCP host

If you skipped the guided wizard, register (or check, or remove) the server's entry in a host's own configuration file directly:

```sh
contextos config mcp register --host claude-desktop
contextos config mcp status --host claude-desktop
contextos config mcp deregister --host claude-desktop
```

See [`cli-reference.md`](cli-reference.md) for the full command set.
