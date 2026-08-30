# Security and Data-Integrity Rules

Assume all tool parameters, vault content, paths, frontmatter, Git refs, HTTP headers, and extension-module requests are untrusted.

## Path confinement

- Allowed roots come only from trusted configuration or CLI input, never a tool call.
- `VaultPath::try_new` is the sole construction boundary for tool paths.
- Resolve roots and targets consistently, including intermediate symlinks and non-existent final components needed for create operations.
- Reject traversal, root-prefix confusion, symlink escapes, Windows verbatim path tricks, drive/UNC mismatches, and alternate data streams.
- Revalidate confinement at the point of mutation to reduce check/use races.
- Never use lexical `starts_with` on unnormalised user input as an authorisation check.

## Persistence

- Write a uniquely named temporary file in the target directory, flush it as required by the platform contract, then atomically replace the target.
- Conflict checks compare the version the caller observed with current content. `force` must be explicit and must not bypass root or schema validation.
- Multi-operation Base and Canvas updates are transactional.
- Deletes default to recoverable platform trash semantics where supported; hard deletion is separately configured and explicit.
- A Git restore creates a new mutation and commit. Never rewrite history.

## Secrets and exposure

- Configuration stores environment variable names, not secret values.
- Require bearer authentication for every non-loopback HTTP bind and compare tokens without timing leakage.
- Default to no CORS and a bounded request body.
- Never return filesystem content through metadata, diagnostics, or logs beyond the explicit read/attach contract.
- Test redaction and rejection behaviour, including error paths.

Security-relevant defects require a regression test at the lowest affected layer and an end-to-end rejection test at the tool boundary.

