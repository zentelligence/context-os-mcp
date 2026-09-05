# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once a first public release is tagged.

The record below starts at the current version - 0.20.1. From this point forward, every user-visible change belongs in `[Unreleased]` until it ships in a tagged release.

## [Unreleased]

## [0.21.0] - 2026-09-05

### Added

- `contextos-web`, a new crate and second binary in this workspace (Phase 14): an `mcp_client` module that spawns or connects to configured MCP servers (`web.toml`'s `[[mcp_server]]` list) and performs the `initialize` handshake before serving any request; a `POST /mcp/{server_name}/{tool_name}` proxy route giving any HTTP caller deterministic, non-model-mediated MCP tool calls; and `/static/` asset serving. Not yet a browsable web UI: vault content rendering, app registration, and a settings UI are later phases (15 to 17).
- `contextos-web service install` / `uninstall` / `status`: registers (or removes, or reports on) `contextos-web` as an auto-starting, per-user background service, none of the three needing elevation, via `systemd --user` on Linux, a `launchd` `LaunchAgent` on macOS, or a Scheduled Task on Windows. The release archive for each platform now also includes the `contextos-web` binary alongside `contextos`.
- `base_query`: `file.inFolder(folder)`, matching Obsidian's own "in folder or subfolder" Bases function. Recognised as a scan-root narrowing hint the same way `file.folder == "..."` already was, so a Base whose filter uses it (rather than the unnarrowable `file.folder == "..." || file.folder.contains(".../")` idiom the lack of a real `inFolder` previously forced) no longer requires a full vault-wide scan.
- `base_query` display columns: a formula whose body is a bare property reference now resolves to that property's own value, and `file.asLink(display?)` resolves to a link targeting the row's own file (`display` defaulting to the file's basename when omitted or empty). Every other formula (arithmetic, `if()`, date functions, and the rest of Obsidian's formula language) still shows the existing "not evaluated" marker rather than guessing at a value. `contextos-web`'s Base card view renders a resolved link as a real, clickable anchor rather than raw JSON.

### Changed

- Renamed the `contextos-server` crate to `contextos-mcp` to disambiguate the MCP server from the web server planned for a future release. The installed binary name (`contextos`) and its CLI are unchanged.
- `contextos-web`'s `--web-config` flag is now `--config` (the binary name already says "web"). `contextos-web`'s own `--help` description is now "ContextOS web UI", dropping "and MCP proxy" as an overstatement of what the CLI itself exposes. Both `contextos` and `contextos-web` now print a lowercase `v`-prefixed version (`v0.20.2`) for `--version`, matching the `vMAJOR.MINOR.PATCH` git tag convention releases already use.
- `web.toml`'s `[server] static_dir` is now optional (previously always required, defaulting to the relative path `./static`): `contextos-web` embeds its own bundled `/static/` assets (CSS, JS) into the binary at compile time, so they serve with no configuration and no dependency on the process's working directory or install layout. A configured `static_dir` is now an override consulted first, falling back to the embedded copy for any file it does not itself contain, rather than the sole source.

### Fixed

- `/static/` no longer 404s on every request when `contextos-web` is launched from a directory that has no `static/` subdirectory next to it, such as a plain `cargo install` or a `PATH` binary directory on Windows: `static_dir`'s previous default (the relative path `./static`, resolved against the process's current working directory rather than the binary's own location) silently pointed nowhere in that layout, and startup gave no warning that it had. The crate's bundled assets are now embedded in the binary itself instead.
- `base_query`'s scan-root narrowing (`scan_root_hint`) could silently narrow a scan, and drop genuine matches, when a `file.path`/`file.folder` equality leaf sat inside an `or` or `not` filter node, or inside a compound `&&`/`||`/`!`/`(...)` filter string its previous whole-string parser did not actually respect. Both are now evaluated with the same `and`/`or`/`not` grammar and safety rules `evaluate_filters` itself uses: narrowing only through a safe `and` conjunct, an `or` only when every operand agrees on the identical hint, never through a `not`.
- `contextos-web`'s Base card view no longer offers a `formula.*` display column as an editable, patchable field in a row's edit form: saving the form unchanged previously wrote the formula's own name or "not evaluated" marker text back into the note's real frontmatter as a bogus key.
- `contextos-web` vault content, apps-list, and settings-page rendering issued every independent MCP round trip, and every `web.toml` read (one of them a blocking filesystem read running directly on the async executor thread), strictly sequentially. Independent work (nav-shell assembly, a note's content fetch/render, appearance/config loads) now runs concurrently via `tokio::join!`, and every blocking `web.toml` read or write moved onto `spawn_blocking`.

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
