//! Exact-result-field contract test for every tool advertising a
//! `fallible_output_schema_for` output schema: asserts each tool's
//! complete result-field set against an explicit expected list, so a
//! silent field regression anywhere in the catalogue is caught. Split
//! from `tool_contract.rs` to keep both files under the project's
//! file-size limit.

use contextos_mcp::ContextOsServer;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one explicit schema matrix keeps the complete MCP contract auditable"
)]
fn every_delivered_tool_advertises_its_exact_result_fields() -> Result<(), Box<dyn std::error::Error>> {
    let expected = [
        (
            "fs_read_text_file",
            vec![
                "code",
                "content",
                "content_hash",
                "line_count",
                "message",
                "path",
                "remediation",
                "truncated",
            ],
        ),
        (
            "fs_read_multiple_files",
            vec!["code", "files", "message", "path", "remediation"],
        ),
        // Every tool below (like `fs_read_text_file`/`fs_read_multiple_files`
        // above) advertises the flat, merged success-and-`ToolFailure`
        // output schema `fallible_output_schema_for` builds: `code`,
        // `message`, `path`, and `remediation` are `ToolFailure`'s own
        // fields, present in every entry because every one of these tools'
        // error path populates `structured_content` with that shape too.
        (
            "fs_write_file",
            vec![
                "bytes_written",
                "code",
                "content_hash",
                "created",
                "message",
                "path",
                "remediation",
                "warnings",
            ],
        ),
        (
            "fs_edit_file",
            vec![
                "applied",
                "code",
                "content_hash",
                "diff",
                "message",
                "path",
                "remediation",
                "warnings",
            ],
        ),
        (
            "fs_create_directory",
            vec!["code", "created", "message", "path", "remediation", "warnings"],
        ),
        (
            "fs_list_directory",
            vec!["code", "entries", "message", "path", "remediation", "rendered"],
        ),
        (
            "fs_move_file",
            vec![
                "code",
                "destination",
                "message",
                "path",
                "remediation",
                "source",
                "warnings",
            ],
        ),
        (
            "fs_delete_file",
            vec![
                "code",
                "deleted",
                "message",
                "path",
                "remediation",
                "results",
                "trashed",
                "warnings",
            ],
        ),
        (
            "fs_search_files",
            vec!["code", "message", "path", "paths", "remediation"],
        ),
        (
            "fs_get_file_info",
            vec![
                "accessed",
                "code",
                "content_hash",
                "created",
                "kind",
                "message",
                "modified",
                "path",
                "readonly",
                "remediation",
                "size",
            ],
        ),
        (
            "fs_list_allowed_directories",
            vec!["code", "directories", "message", "path", "remediation"],
        ),
        (
            "vault_index_rebuild",
            vec![
                "code",
                "directories_scanned",
                "indexes_created",
                "indexes_updated",
                "message",
                "path",
                "remediation",
                "skipped",
            ],
        ),
        (
            "vault_log_append",
            vec!["appended", "code", "message", "path", "remediation", "warnings"],
        ),
        (
            "vault_info",
            vec![
                "code",
                "message",
                "path",
                "protocol_version",
                "remediation",
                "resource_link_threshold_kb",
                "transports",
                "vaults",
                "version",
            ],
        ),
        (
            "note_create",
            vec![
                "code",
                "content_hash",
                "message",
                "path",
                "remediation",
                "validation",
                "warnings",
            ],
        ),
        (
            "frontmatter_read",
            vec![
                "body_start_line",
                "code",
                "content_hash",
                "frontmatter",
                "message",
                "path",
                "remediation",
            ],
        ),
        (
            "frontmatter_update",
            vec!["code", "content_hash", "message", "path", "remediation", "warnings"],
        ),
        (
            "base_create",
            vec!["code", "content_hash", "message", "path", "remediation", "warnings"],
        ),
        (
            "base_read",
            vec![
                "code",
                "content_hash",
                "definition",
                "diagnostics",
                "message",
                "path",
                "remediation",
            ],
        ),
        (
            "base_apply",
            vec!["code", "content_hash", "message", "path", "remediation", "warnings"],
        ),
        (
            "canvas_create",
            vec!["code", "content_hash", "message", "path", "remediation", "warnings"],
        ),
        (
            "canvas_read",
            vec![
                "code",
                "content_hash",
                "diagnostics",
                "edges",
                "message",
                "nodes",
                "path",
                "remediation",
            ],
        ),
        (
            "canvas_apply",
            vec!["code", "content_hash", "message", "path", "remediation", "warnings"],
        ),
        (
            "links_read",
            vec!["code", "message", "outgoing", "path", "remediation", "unresolved"],
        ),
        (
            "git_init",
            vec!["code", "commit_id", "initialised", "message", "path", "remediation"],
        ),
        (
            // `GitCommitToolResult.message` (the commit message) and
            // `ToolFailure.message` (the error message) share one property
            // name: the merge collapses them into a single `message` key,
            // same as any other same-named success/failure field would.
            "git_commit",
            vec!["code", "commit_id", "committed_paths", "message", "path", "remediation"],
        ),
        (
            "git_restore",
            vec!["applied", "code", "diff", "message", "path", "remediation", "warnings"],
        ),
        (
            "git_status",
            vec![
                "ahead",
                "behind",
                "branch",
                "code",
                "message",
                "path",
                "pending_paths",
                "remediation",
                "seconds_until_auto_commit",
                "staged",
                "unstaged",
                "untracked",
            ],
        ),
        ("git_log", vec!["code", "entries", "message", "path", "remediation"]),
        (
            "git_diff",
            vec!["code", "content", "message", "path", "remediation", "truncated"],
        ),
    ];
    let catalogue = ContextOsServer::catalogue();

    for (name, expected_fields) in expected {
        let tool = catalogue
            .get(name)
            .ok_or_else(|| std::io::Error::other(format!("missing tool {name}")))?;
        let schema = tool
            .output_schema
            .as_ref()
            .ok_or_else(|| std::io::Error::other(format!("{name} omitted output schema")))?;
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| std::io::Error::other(format!("{name} omitted output properties")))?;
        let mut actual_fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
        actual_fields.sort_unstable();

        assert_eq!(actual_fields, expected_fields, "{name} schema drifted");
    }
    Ok(())
}
