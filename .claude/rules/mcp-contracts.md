# MCP Contract Rules

The MCP surface is a public compatibility boundary. Treat tool names, input schemas, result shapes, error codes, resource media types, and transport behaviour as versioned contracts.

## Tool implementation

- Keep one domain operation behind all transports. Stdio and streamable HTTP must expose identical catalogues and semantics.
- Deserialize into request DTOs, reject unknown or malformed fields as defined by the contract, then convert to validated domain types with `From` or `TryFrom`. Never use ad hoc conversion functions at this boundary.
- Return structured results. Do not make clients parse prose to obtain fields.
- Include non-fatal secondary-service failures in `warnings` after a successful mutation.
- Preserve stable machine-readable error codes and provide an actionable human message plus remediation hint.
- Enforce documented limits before allocating or reading full content.
- Batch tools isolate per-item failures when the specification promises partial success.

## Contract tests

For every tool, cover:

1. advertised name, description, and JSON schema;
2. smallest valid request and representative complete request;
3. response schema and content-block encoding;
4. every documented error code;
5. unknown fields, missing required fields, and incompatible options;
6. limits, conflicts, path rejection, and warning propagation; and
7. parity across transports once HTTP exists.

Tests should invoke the MCP adapter rather than calling only the underlying service. Keep separate service tests for domain behaviour.

Protocol revisions and SDK versions are temporally sensitive. Before changing them, consult the official MCP specification and official Rust SDK sources, record the protocol revision in the change, and add compatibility tests. Wrap pre-1.0 SDK types at the server boundary so SDK churn does not leak into core.
