// contextos-web MCP proxy client (FR-211).
//
// Wraps `POST /mcp/{server_name}/{tool_name}` (FR-210) as
// `callTool(serverName, toolName, args)`, returning a Promise: the same
// deterministic, non-model-mediated tool-calling surface Cowork live
// artefacts previously reached via `callMcpTool`, with no LLM step between
// an app's own JavaScript and the tool result.
//
// Served as a plain script (no bundler assumed): including it via
// `<script src="/static/contextos-web-client.js"></script>` exposes
// `window.contextosWeb.callTool`. It also exports via `module.exports` for
// CommonJS consumers (used by this crate's own Node-based contract test).
(function (root) {
  "use strict";

  /**
   * Calls `toolName` on the configured MCP server `serverName` with `args`.
   *
   * Resolves with the tool's MCP result content (the same JSON body
   * `POST /mcp/{server_name}/{tool_name}` returns on success or an
   * MCP-level tool error, FR-213). Rejects only on a transport-level
   * failure or rejection (unconfigured server, malformed request, or the
   * proxied server being unreachable): the rejection error carries `status`
   * (the HTTP status code) and `body` (the parsed JSON error body) fields.
   *
   * `options.baseUrl` overrides the request's origin (default: relative to
   * the page's own origin), for use outside a browser document context.
   */
  async function callTool(serverName, toolName, args, options) {
    const payload = args === undefined ? {} : args;
    const baseUrl = (options && options.baseUrl) || "";
    const path =
      "/mcp/" +
      encodeURIComponent(serverName) +
      "/" +
      encodeURIComponent(toolName);

    const response = await fetch(baseUrl + path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const body = await response.json();

    if (!response.ok) {
      const message =
        (body && body.error) ||
        "contextos-web proxy request failed (" + response.status + ")";
      const error = new Error(message);
      error.status = response.status;
      error.body = body;
      throw error;
    }
    return body;
  }

  root.contextosWeb = { callTool: callTool };
  if (typeof module !== "undefined" && module.exports) {
    module.exports = { callTool: callTool };
  }
})(typeof window !== "undefined" ? window : globalThis);
