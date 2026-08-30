# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once a first public release is tagged.

The record below starts at the current version - 0.20.1. From this point forward, every user-visible change belongs in `[Unreleased]` until it ships in a tagged release.

## [Unreleased]

### Added

- Nothing yet.

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
