# Obsidian format conformance fixtures

These fixtures encode the Obsidian-flavoured format authorities `contextos` implements: Obsidian Bases, JSON Canvas, and Mermaid diagrams embedded in vault notes.

- `bases/` — Obsidian Bases schema and examples from the ContextOS `obsidian-bases` skill, covering the `FR-44` to `FR-46` acceptance statements.
- `canvases/` — JSON Canvas 1.0 and the ContextOS `json-canvas` skill, covering the same `FR-44` to `FR-46` acceptance statements.
- `mermaid/` — Mermaid flowchart source used by the `FR-70`/`FR-71` `mermaid_validate`/`mermaid_render` acceptance statements.

`bases/every-feature.base` covers recursive global and view filters, formulas, property presentation, a custom summary, grouping, sorting, limits, standard summaries, all four view types, quoting-sensitive expressions, and preserved Maps-plugin settings.

`canvases/every-feature.canvas` covers all four node types, generic geometry, file subpaths, group backgrounds, both colour forms, and every optional edge attribute. `canvases/group-nesting.canvas` covers nested groups, and `canvases/dangling-edge.canvas` is expected to produce exactly one `canvas/dangling-edge` diagnostic at `edges[0].toNode`.

`mermaid/valid-flowchart.mmd` is a well-formed flowchart used across the `mermaid_validate`/`mermaid_render` contract tests as the happy-path diagram. `mermaid/invalid-flowchart.mmd` has a dangling edge and is used to exercise the `mermaid/diagram-parse` diagnostic on both tools.

The Bases and Canvas fixture tests parse, validate, serialise, reparse, and compare ordered definitions. They deliberately avoid normalising through Obsidian so that a round trip cannot conceal information loss.
