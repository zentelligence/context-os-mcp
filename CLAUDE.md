@AGENTS.md

# ContextOS Server (`context-os-mcp`)

Rust MCP server for operating ContextOS knowledge vaults: safe filesystem parity plus vault-aware indexing, operation logging, local Git recovery, Obsidian operations, and layered query services. Cargo workspace of library crates behind the `contextos-mcp` binary; the installed binary name is `contextos`.

**`AGENTS.md` is the binding contract for every substantive task in this repository. Read it before changing anything; nothing in this file weakens it.** The `.claude/` directory holds the detailed rules, workflows, review lenses, and templates it references, applied automatically through Claude Code's skills, subagents, and hooks; Codex sessions read the same files directly as a secondary, occasional surface.

## Current state

- Phases 1 to 7 are implemented with the automated Linux gate green: filesystem parity, substrate services (index, oplog, Git), Obsidian Bases and Canvas tools, the query layer and HTTP surface, semantic search and the `ServerModule` extension API, Mermaid diagram support, and the generalised resource capability (any-file `resources/read`, `hidden` enumeration filtering, `resource_link` attachment for oversized results).
- Phase 8 (Doctor support via MCP: `doctor`, `doctor_resolve`, the `--resolve`/`--dry-run` CLI flags, and the vault-wide frontmatter   validity check) is implemented with `just ci` green on **Windows** (this session's environment); Linux CI evidence for Phase 8 specifically has not yet been separately gathered; do not claim the automated Linux gate for Phase 8 until it has.
- Every phase in the delivery plan is implemented; what remains is the cross-cutting "Packaging and release" work (CI matrix, release artefacts, versioning discipline) plus the environment-specific acceptance below. See `CLAUDE.local.md` for the delivery plan's location.
- Environment-specific acceptance work (Windows and macOS matrices, Obsidian desktop spot check, host acceptance) remains open. Never claim those gates from Linux-only evidence.

## Authoritative inputs

Product behaviour (requirements and specification) is specified outside this repository. See `CLAUDE.local.md` for the exact location if present in your checkout, or ask the repository owner. Read the governing requirement and component specification before changing product behaviour. If the sources are unavailable, record the uncertainty and ask; never invent missing semantics.

## Commands

| Purpose | Command |
| --- | --- |
| Completion gate | `just ci` |
| Guidance checks plus gate | `.claude/scripts/check.sh` |
| Targeted red/green test | `cargo test -p <crate> <test_name> -- --exact` |
| Run against a disposable vault | `cargo run -p contextos-mcp --bin contextos -- --vault <dir>` |
| Support check | `cargo run -p contextos-mcp --bin contextos -- --config <path> doctor` |

Never claim a check passed unless it actually ran and succeeded. Report any
skipped check and the reason.

## Architecture in one screen

- Hexagonal workspace; every dependency edge points towards `contextos-core`. Library crates never depend on `contextos-mcp`.
- `contextos-core` owns `VaultPath`, `VaultSet`, operation events, routing policy, domain errors, and the write-pipeline contracts. Capability crates: `contextos-fs`, `contextos-obsidian`, `contextos-index`, `contextos-oplog`, `contextos-git`, and (Phase 4) `contextos-search`. `contextos-mcp` is the composition root and MCP adapter.
- Every mutation flows through one pipeline: validate → conflict check → temp write + fsync + atomic rename → `OperationEvent` → index / oplog / Git / search. The completed write is the contract; downstream failures become typed warnings, never a rolled-back write.
- `Origin::Internal` is the recursion guard: internal writes skip index and oplog routing but still reach Git. The routing policy lives in `contextos-core::routing`, expressed in types, not string comparisons.
- All tool paths become a validated `VaultPath` before entering a service. `VaultPath::try_new` is the sole security boundary; no raw `Path` or `PathBuf` crosses a domain or service boundary.

## Non-negotiables (summary; `AGENTS.md` is authoritative)

- Trait-first design and test-driven delivery: observe the failing test for the intended reason before writing production behaviour. Bug fixes require a regression test.
- No `unsafe`, `unwrap`, or `expect` in production paths. One focused `thiserror` enum per crate with stable machine-readable error codes.
- Every project type conversion is `From<T>` or `TryFrom<T>`; no free-form conversion helpers (`to_*`, `from_*`, `convert_*`, `parse_*`).
- Writes are atomic, conflict-aware, and root-confined. Symlinks, Windows paths, and external modifications are security boundaries.
- Blocking filesystem, Git, index, or model work never runs on an async executor thread. Tests are deterministic: inject clocks, hashes, and providers; never depend on network, wall-clock sleeps, or the operator's home directory.
- `tracing` only, never `println!`; no vault content, tokens, or secrets at INFO and above. Reject malformed or unknown input; never silently repair.
- Australian English spelling and punctuation for operator-facing text and documentation, including the Oxford comma.
- Markdown prose is not hard-wrapped: one line per paragraph. Wrap only where it adds real value (tables, code samples, fixed-width content); 120 characters if a hard limit is genuinely needed.
- Keep changes scoped to the requested problem. Do not commit, push, or rewrite history unless explicitly asked, and never add attributions (such as `Co-Authored-By`) to commits.

## Rule routing

| When the task touches | Read |
| --- | --- |
| Any production behaviour | `.claude/rules/testing.md`, `.claude/workflows/tdd.md`, `.claude/workflows/change-loop.md` |
| Crate boundaries, traits, or services | `.claude/rules/architecture.md` |
| MCP tools, schemas, resources, or transports | `.claude/rules/mcp-contracts.md` |
| Paths, writes, HTTP, secrets, or external content | `.claude/rules/security.md` |
| Rust code, dependencies, errors, async, or logging | `.claude/rules/rust-quality.md` |
| Completion or CI | `.claude/workflows/quality-gate.md` |
| Session memory, diaries, or vault knowledge stores | `.claude/rules/memory.md` |

Source-of-truth precedence: explicit user instruction → `AGENTS.md` → numbered requirements and specification → `.claude` guidance → existing code and tests. When authoritative sources conflict, stop that design decision, show the exact conflict with requirement IDs, and ask.

## Claude Code project surface

- Slash commands: `/brief` (change brief), `/tdd` (red-green-refactor loop), `/gate` (completion gate with evidence report), `/review-code` and `/review-spec` (the two `.claude` review lenses), and `/matrix` (requirement-to-test matrix).
- Subagents: `code-reviewer` and `spec-reviewer` apply the corresponding `.claude/reviewers/` lenses; use them proactively before declaring any behaviour change complete.
- Hook: after every edit to a `.rs` file, `rustfmt --check` runs automatically and reports drift. Fix drift with `cargo fmt --all` rather than accumulating it for the gate.

## Memory and session records

`.claude/rules/memory.md` governs the three knowledge stores; in short:

- `docs/` holds authoritative in-repo technical documentation intended for agent reference and consumption.
- mempalace (globally configured MCP) holds cross-session findings, learnings, decisions with rationale and diary entry notes under agent  `claude-contextos` (Codex uses `codex-contextos`; check both when orienting).
- The ContextOS vault records coding lessons in `memory/coding/YYYY/MM/YYYY-MM-DD-claude.md` (append-only; format in `memory/coding/index.md`).
- The vault's `memory/log/` is the append-only ContextOS operation log for vault operations only; never write coding-session records there. A misplaced entry is corrected by appending a correction, never by editing.

## Repository gotchas

- The binary name `contextos` is coupled to `env!("CARGO_BIN_EXE_contextos")` in `contextos-mcp` tests; a bin rename must update both or the change reverts under test.
- `.claude/scripts/check.sh` validates every relative Markdown link in the repository and rejects trailing whitespace in guidance files; keep new documentation clean or the gate fails.
- Per-phase change briefs and requirement-to-test matrices are maintained outside this repository (see `CLAUDE.local.md` for the location); keep them updated as implementation progresses rather than reconstructing them at the end.
