# ContextOS Server

ContextOS Server is a Rust implementation of a Model Context Protocol (MCP) server for operating ContextOS knowledge vaults: Obsidian-flavoured Markdown vaults with structured notes, Bases, and Canvas files. The repository name is `context-os-mcp`; the installed binary is `contextos`.

It is a vault-aware execution layer, not a generic filesystem MCP: every write is path-safe and atomic, and the server layers automatic `index.md` maintenance, append-only operation logging, local Git recovery, structured Obsidian operations, and text/graph/semantic search on top.

## Intended capabilities

| Area | Capability |
| --- | --- |
| Filesystem | Read, write, edit, list, tree, search, move, inspect, attach, and guarded delete within configured roots |
| Substrate | Preserve operator prose while maintaining directory indexes and append-only daily operation logs |
| Recovery | Stage mutations and create debounced local Git commits; restore historical content without rewriting history |
| Obsidian | Structured note/frontmatter, Bases, Canvas, and wikilink operations with validation |
| Query | Ranked text search, link-graph traversal, and opt-in local semantic search |
| Transport | MCP over stdio and authenticated streamable HTTP from one service instance |
| Extension | Feature-gated, namespaced modules using the same secured core services |

See [`docs/mcp-tools.md`](docs/mcp-tools.md) for the full tool catalogue.

## Installation

Requires a recent stable Rust toolchain (see `rust-version` in `Cargo.toml`).

```sh
cargo install --path crates/contextos-mcp
```

or build from a checkout without installing:

```sh
cargo build --workspace --release
```

See [`docs/installation.md`](docs/installation.md) for first-run setup, and [`docs/configuration.md`](docs/configuration.md) for the full `config.toml` reference.

## Quick start

Run the interactive guided-setup wizard, which creates `config.toml`, adds a vault, and can register the server with an MCP host:

```sh
contextos config
```

Or configure by hand and run directly against a vault:

```sh
contextos --vault /path/to/vault
```

Check configuration, vault reachability, and recovery state at any time with:

```sh
contextos doctor
```

## Design principles

- **Safe by construction:** every path is resolved through a root-scoped `VaultPath`; traversal and symlink escapes are rejected.
- **One mutation pipeline:** validation, conflict detection, atomic persistence, indexing, logging, Git staging, and search updates share one flow.
- **Operator content is authoritative:** machine-managed regions never overwrite prose outside their explicit markers.
- **Local first:** Git history and default embeddings stay on the operator's machine; remote Git operations are outside v1.
- **Graceful degradation:** plain directories retain filesystem tools while ContextOS services can be disabled per vault.
- **Explicit conversions:** project types convert only through Rust's `From` or `TryFrom` traits.

## Architecture

Hexagonal Cargo workspace; every dependency edge points towards `contextos-core`.

| Crate | Responsibility |
| --- | --- |
| `contextos-core` | Domain types, validated paths, errors, service traits, and write-pipeline contracts |
| `contextos-fs` | Atomic, conflict-aware filesystem operations |
| `contextos-obsidian` | Markdown, frontmatter, Bases, Canvas, and link codecs |
| `contextos-index` | Managed `index.md` reconciliation |
| `contextos-oplog` | Append-only operation logging |
| `contextos-git` | Local repository status, staging, commit, diff, and recovery |
| `contextos-search` | Text, graph, vector, and embedding services |
| `contextos-mermaid` | Mermaid diagram parsing, layout, and SVG rendering |
| `contextos-ephemeris` | Optional astronomical/astrological calculations (moon phase, solar events, personal-year periods) |
| `contextos-mcp` | MCP tool registry, configuration, stdio, and HTTP transports |

Every mutation passes through one write pipeline: validate, check for conflicts, write atomically (temp file, fsync, rename), then route the resulting event to the index, operation log, Git, and search substrates. A downstream substrate failure becomes a typed warning; it never rolls back a completed write.

## Documentation

- [`docs/installation.md`](docs/installation.md) — building, installing, and first run
- [`docs/configuration.md`](docs/configuration.md) — `config.toml` reference
- [`docs/cli-reference.md`](docs/cli-reference.md) — every `contextos` subcommand
- [`docs/mcp-tools.md`](docs/mcp-tools.md) — the MCP tools the server registers
- API/code reference: run `cargo doc --workspace --no-deps --open`

## Contributing

All contributors and coding agents must read [AGENTS.md](AGENTS.md), which governs trait-first design, test-driven delivery, and the security and quality gates every change must pass. Codex sessions should then use [`.claude/rules/00-index.md`](.claude/rules/00-index.md) to load only the guidance relevant to the task.

The development loop is:

1. Select a requirement and define its observable contract.
2. Add the smallest test and confirm that it fails for the intended reason.
3. Implement only enough behaviour to pass.
4. Refactor with the suite green.
5. Run the affected crate tests, then the workspace quality gate.

The workspace quality gate is:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

Run the complete repository gate, including guidance checks, with:

```sh
.claude/scripts/check.sh
```

## Licence

Apache 2.0. Copyright Zentelligence Pty Ltd.
