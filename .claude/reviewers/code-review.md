# Code Review Lens

Review the diff, not the author's intent. Report concrete findings ordered by severity, with file/line evidence and the violated invariant.

## Correctness and integrity

- Does every mutation take the validated, conflict-aware, atomic pipeline?
- Can partial, cancelled, or secondary-service failure leave inconsistent data?
- Are moves, deletes, restores, and external modifications handled explicitly?
- Are error codes stable and actionable rather than collapsed into strings?

## Security and privacy

- Can any path form, symlink, race, Git ref, or HTTP option escape its authority?
- Are limits enforced before allocation or content reads?
- Can logs, warnings, MCP errors, or config summaries disclose content/secrets?
- Are dangerous behaviours gated independently from ordinary write access?

## Architecture and Rust

- Is dependency direction preserved and are handlers thin?
- Does every project type conversion use `From` or `TryFrom`, without ad hoc conversion helpers or unchecked semantic casts?
- Are traits narrow, object-safe when needed, and owned by the consumer/domain?
- Is blocking work kept off async executor threads?
- Is production code free of unsafe, panic shortcuts, debug output, and broad
  lint suppressions?

## Tests

- Is there evidence of the right test at the right layer, including the failure path and prohibited side effects?
- Are filesystem and Git semantics tested with real isolated adapters?
- Are time, network, environment, and ordering deterministic?
- Would the test fail if the new production behaviour were removed?

If there are no findings, state that explicitly and list residual untested risks. Do not inflate style preferences into defects.

Before concluding, consider the change holistically. Do not accept a narrow local improvement that transfers risk or complexity into security, another crate, another platform, operations, users, tests, or future maintenance.
