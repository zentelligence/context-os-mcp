# Rule Index

Always read `AGENTS.md` first. Then route the task through this index.

| When the task touches | Read |
| --- | --- |
| Any production behaviour | `testing.md`, `../workflows/tdd.md`, `../workflows/change-loop.md` |
| Crate boundaries, traits, or services | `architecture.md` |
| MCP tools, schemas, resources, or transports | `mcp-contracts.md` |
| Paths, writes, HTTP, secrets, or external content | `security.md` |
| Rust code, dependencies, errors, async, or logging | `rust-quality.md` |
| Completion or CI | `../workflows/quality-gate.md` |
| Session memory, diaries, or vault knowledge stores | `memory.md` |
| Code review | `../reviewers/code-review.md` |
| Requirement or phase review | `../reviewers/specification-review.md` |

For an implementation task, identify the source requirement IDs and fill a small change brief using `../templates/change-brief.md`. For a feature spanning several contracts, maintain a matrix based on `../templates/requirement-test-matrix.md`.

Source-of-truth precedence:

1. Explicit user instruction.
2. The closest applicable `AGENTS.md`.
3. Numbered requirements and the implementation specification.
4. `.claude` guidance.
5. Existing code and tests, which may reveal drift but do not silently override the written contract.

When two authoritative sources conflict, stop the affected design decision, show the exact conflict and requirement IDs, and ask for resolution. Continue with independent work when safe.

## Language

Australian English spelling, grammar, and punctuation.
Never use 'canonical' or its variants.
Never use em dashes or double-hyphens.
