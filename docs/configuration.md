# Configuration

`contextos` reads a single TOML file, resolved in this order: the `--config` flag, then the `CONTEXTOS_MCP_CONFIG` environment variable, then `~/.config/contextos/config.toml`. [`config.example.toml`](config.example.toml) is a complete, annotated example covering every currently supported field; this page summarises the shape.

## `[server]`

| Field | Purpose |
| --- | --- |
| `transports` | Which transports to run: `"stdio"`, `"http"`, or both |
| `log_level` | `error`, `warn`, `info`, `debug`, or `trace` |
| `log_file` | Empty logs to stderr only |
| `resource_link_threshold_kb` | Size at or above which a text-reading tool attaches a resource link with a bounded preview, rather than returning the full content inline |
| `astro` | Registers the ephemeris tools (moon phase, solar events, Wheel-of-the-Year, personal-year period) into the advertised tool catalogue. Off by default; every handler is compiled in regardless, this only controls visibility, and it can be overridden per run with `--astro` |

### `[server.http]`

Only used when `"http"` is in `transports`. `bind` defaults to `127.0.0.1:7331`; a non-loopback bind refuses to start unless `token` (or the `CONTEXTOS_MCP_TOKEN` environment variable) is set, and a configured token is enforced on every request once bound. `max_body_kb` caps request body size.

## `[[vault]]`

One entry per vault. `path` is the vault root; `name` addresses it as `name://relative-path` on every path-accepting tool parameter and defaults to the root directory's basename (it must be unique and a valid URI scheme token). `managed` controls whether mutating tools may write to it at all — an unmanaged vault still gets read-only filesystem tools, but no index maintenance, operation log, Git recovery, or search. `hidden` and `resources_list_include` control what the enumeration and resource-listing surfaces show; `state_directory` overrides where derived state (indexes, embeddings) is stored.

### `[vault.limits]`

`max_read_mb` and `max_batch_files` bound single-call resource use.

### `[vault.index_md]`

`enabled` toggles managed `index.md` reconciliation; `exclude` lists paths it skips.

### `[vault.oplog]`

`enabled` toggles the append-only daily operation log; `path` is its location within the vault (typically `memory/log`).

### `[vault.git]`

`enabled` toggles local Git recovery. `commit_debounce_s` batches rapid mutations into one commit; `author_name`/`author_email` label them; `destructive_delete` controls whether `fs_delete_file` hard-deletes instead of moving to trash; `restore_exclude` lists paths `git_restore` never overwrites (the operation log's path is always added automatically, on top of whatever you list here). 

### `[vault.search]`

`text` and `graph` enable the corresponding indexes; `graph_backend` picks the link graph's persistence backend (`serde` for a single JSON file with no cross-process guarantee, `fjall` — the default — an embedded key-value store, fastest for the common single-process case, or `sqlite` in WAL mode when more than one `contextos` process opens the same vault at once). `semantic` enables opt-in vector search. `exclude` scopes the search corpus independently of `index_md.exclude`. `rebuild_budget_seconds` is the default time budget for `query_index_rebuild`'s semantic phase.

### `[vault.search.embedding]`

`provider` is `local` (offline ONNX inference via `fastembed`, no network access; run `contextos model` to manage the shared model cache) or `openai-compatible` (any endpoint speaking the OpenAI embeddings API shape, configured via `model`, `endpoint`, and `api_key_env`). `model_directory` points at the local provider's model cache, defaulting to the platform cache location.

## Environment overrides

| Variable | Overrides |
| --- | --- |
| `CONTEXTOS_MCP_CONFIG` | The configuration file path |
| `CONTEXTOS_MCP_TOKEN` | `[server.http].token` |
| `CONTEXTOS_MCP_LOG_LEVEL` | `[server].log_level` |
