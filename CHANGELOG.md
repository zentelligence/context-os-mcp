# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once a first public release is tagged.

The record below starts at the current version - 0.20.1. From this point forward, every user-visible change belongs in `[Unreleased]` until it ships in a tagged release.

## [Unreleased]

### Added

- Nothing yet.

### Changed

- Renamed the `contextos-server` crate to `contextos-mcp` to disambiguate the MCP server from the web server planned for a future release. The installed binary name (`contextos`) and its CLI are unchanged.

## [0.20.2] - 2026-09-03

### Fixed

- `contextos config` now loads an existing `config.toml` before running the guided-setup interview instead of treating every run as a fresh install: a single existing vault is offered back for edit with its current settings prefilled as defaults, and more than one vault asks what to focus on (general server settings, all vaults, or one named vault to edit or remove) before optionally adding more.
- `Config::validate` now rejects an empty transports list, which previously validated successfully but left the server started with neither stdio nor HTTP.
- `fs_write_file` no longer reports an omitted `expected_hash` on an existing file with the same conflict error as a genuine write conflict. A missing hash now fails fast with its own accurate message, and a genuine conflict returns the file's current on-disk hash so a caller can retry in one step without a second read; the remediation text no longer names `force` as a peer option to a hash retry.

### Removed

- The unreleased `embedvec` evaluation prototype (bench target, dev-dependency, and the `embedvec-unreleased-flush-sync` feature on `contextos-search`), which existed only to gate calls into an unmerged local checkout and made a plain `--all-features` build fail against the published crate this workspace actually pins.

## [0.20.1] - 2026-08-30

### Added

- Filesystem parity tools: read, write, edit, list, tree, search, move, inspect, attach, and guarded delete within configured vault roots.
- Substrate services: managed `index.md` maintenance, append-only operation logging, and local Git recovery.
- Obsidian-aware operations: structured notes, frontmatter, Bases, Canvas, and wikilink handling with validation.
- Query layer: ranked text search, link-graph traversal, and opt-in local semantic search, exposed over MCP stdio and authenticated streamable HTTP.
- Extension API (`ServerModule`) for feature-gated, namespaced tool modules built on the same secured core services.
- Mermaid diagram parsing, layout, and SVG rendering support.
- Generalised resource capability: any-file `resources/read`, `hidden`-aware directory enumeration, and `resource_link` attachment for oversized results.
- Doctor support: `doctor` and `doctor_resolve` MCP tools, `--resolve`/`--dry-run` CLI flags, and a vault-wide frontmatter validity check.
