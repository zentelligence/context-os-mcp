# MCP tools

Every tool operates on a vault-relative path (`name://relative-path`, or a bare relative path when only one vault is configured) resolved through the server's path-safety layer. Tools that mutate a vault reject the call if the target vault is unmanaged.

## Filesystem

| Tool | Purpose |
| --- | --- |
| `fs_read_text_file` | Read a UTF-8 file with an optional head, tail, or inclusive line range |
| `fs_read_multiple_files` | Read several UTF-8 files with isolated per-file failures |
| `fs_write_file` | Atomically create or replace a UTF-8 file with conflict protection |
| `fs_edit_file` | Apply exact-match edits transactionally, with an optional dry-run unified diff |
| `fs_attach_file` | Embed a text or base64 binary file as a size-capped MCP resource |
| `fs_create_directory` | Create a directory tree idempotently |
| `fs_list_directory` | List direct children with file and directory markers |
| `fs_directory_tree` | Return a bounded recursive JSON directory tree with exclusions |
| `fs_move_file` | Move or rename a file or directory without replacing the destination |
| `fs_delete_file` | Move a file or empty directory to trash, or hard-delete when configured |
| `fs_search_files` | Find paths by case-insensitive glob with exclusions |
| `fs_get_file_info` | Return path metadata, permissions, timestamps, and a bounded content hash |
| `fs_list_allowed_directories` | List configured, resolved vault roots and their managed flags |

## Obsidian notes, Bases, and Canvas

| Tool | Purpose |
| --- | --- |
| `note_create` | Create a validated Obsidian Markdown note with standard frontmatter defaults |
| `frontmatter_read` | Read ordered YAML frontmatter from a note |
| `frontmatter_update` | Apply an atomic JSON merge patch to note frontmatter while preserving the body |
| `links_read` | Read outgoing wikilinks and embeds from a note |
| `base_create` | Create a validated Obsidian Bases YAML definition |
| `base_read` | Read an ordered Bases definition with schema diagnostics |
| `base_apply` | Apply ordered Base operations atomically after validating the complete result |
| `base_query` | Execute a Base view's filter tree against the vault and return matching rows |
| `canvas_create` | Create a validated JSON Canvas 1.0 document and generate omitted identifiers |
| `canvas_read` | Read Canvas nodes and edges with schema diagnostics |
| `canvas_apply` | Apply ordered Canvas operations atomically after full validation |

## Query

| Tool | Purpose |
| --- | --- |
| `query_text` | Ranked full-text search across vault markdown with path, tag, and frontmatter filters |
| `query_semantic` | Vector similarity search over chunked note content; requires `[vault.search] semantic = true` |
| `query_graph` | Traverse the wikilink graph: neighbours, backlinks, shortest path, or orphaned notes |
| `query_index_status` | Report per-index document counts, staleness estimate, and last build time |
| `query_index_rebuild` | Rebuild the text index, link graph, and/or semantic index from a full vault scan |

## Git recovery

| Tool | Purpose |
| --- | --- |
| `git_init` | Initialise recoverable local Git history for a vault |
| `git_commit` | Commit all pending MCP-owned staged paths immediately |
| `git_restore` | Restore historical content as new forward mutations, without rewriting history |
| `git_status` | Report branch, staged, unstaged, untracked, and pending MCP paths |
| `git_log` | Read local commit history with an optional path filter |
| `git_diff` | Return a size-capped unified diff between refs or the working tree |

## Diagrams

| Tool | Purpose |
| --- | --- |
| `mermaid_validate` | Validate a Mermaid diagram from a note's fenced or inline source, without rendering |
| `mermaid_render` | Parse, lay out, and render a Mermaid diagram to SVG |

## Diagnostics and maintenance

| Tool | Purpose |
| --- | --- |
| `doctor` | Report read-only health checks for the effective configuration; identical content to `contextos doctor` |
| `doctor_resolve` | Resolve every currently auto-fixable doctor finding |
| `vault_info` | Report server, transport, effective vault configuration, and substrate health, without exposing secrets |
| `vault_index_rebuild` | Rebuild managed `index.md` files for every non-excluded folder in a subtree |
| `vault_log_append` | Append one explicit manual entry to the shared daily operation log |

## Ephemeris (optional)

Registered only when `[server] astro` (or `--astro`) is enabled; see [`configuration.md`](configuration.md). Every calculation is computed offline, with no network access.

| Tool | Purpose |
| --- | --- |
| `ephemeris_moon_phase` | Compute the Moon's phase for a calendar date: name, illumination fraction, and proximity to each primary phase |
| `ephemeris_solar_events` | Compute the exact UTC instant of both solstices and both equinoxes for a calendar year |
| `ephemeris_wheel_of_year` | Compute all eight Wheel-of-the-Year points for a calendar year, hemisphere-correctly named |
| `ephemeris_personal_year_period` | Compute which of the seven annually-recurring personal-year periods contains a given date, and its ruling planet |
| `ephemeris_boundaries` | Aggregate every horizon boundary or checkpoint crossed within a date range in one call |
