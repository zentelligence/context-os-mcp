# ContextOS Server Agent Contract

This file governs all work in this repository. More specific `AGENTS.md` files may add constraints for their subtree, but may not weaken this contract.

## Mission and current state

Build `contextos`, a Rust MCP server that provides safe filesystem parity and ContextOS-aware indexing, logging, versioning, Obsidian operations, and query services. Phase 1 filesystem parity is under implementation; consult the delivery plan and requirement-to-test matrix for verified and pending gates.

The authoritative product input (requirements and specification) is maintained outside this repository. See `CLAUDE.local.md` for its exact location if present in your checkout, or ask the repository owner.

Read `requirements.md`, then the specification index and relevant component documents before changing product behaviour. If those files are unavailable, do not invent missing semantics. Record the uncertainty and ask for the required source.

## Quality mandate

**Holistic quality trumps everything else.** Optimise for the integrity of the whole product and its lifecycle, including correctness, security, data safety, architecture, interoperability, test strength, operability, maintainability, performance, accessibility of diagnostics, and documentation.

Delivery speed, diff size, convenience, token cost, and passing a narrow test are never sufficient reasons to weaken the result. Scope discipline still applies: solve the requested problem completely and coherently without adding unrequested product surface. However, any bias towards simplest and or smallest is explicitly overruled as violation of the quality mandate.

Prior existence of an error, deviation, or violation is no excuse for perpetuating or no addressing it. Identified issues are not "raised for later", the only permitted exception is a change with significant architectural implications that would require a multi-stage analysis, spec design, work breakdown plan, along with one or more full critique and refinement passes.

## Start every substantive task

1. Read this file and [`.claude/rules/00-index.md`](.claude/rules/00-index.md).
2. Inspect the working tree and preserve unrelated or user-authored changes.
3. Identify the applicable requirement IDs (`FR-*`, `NFR-*`, and `D-*`) and delivery phase.
4. Read only the `.claude` rules relevant to the task.
5. Define the smallest observable behaviour that can be driven by a failing    test.

## Non-negotiable engineering rules

- Follow trait-first development.
- Use test-driven delivery for every behaviour change: red, green, then refactor. See [`.claude/workflows/tdd.md`](.claude/workflows/tdd.md).
- Do not add production behaviour until a focused test has failed for the expected reason. Bug fixes require a regression test.
- Keep dependency direction hexagonal and strictly towards `contextos-core`. Library crates never depend on `contextos-mcp`.
- All tool paths must become a validated `VaultPath` before entering a service. Do not accept raw `Path` or `PathBuf` at domain or service boundaries.
- All mutations pass through the write pipeline. No tool handler or extension module writes directly to the filesystem.
- Production code contains no `unsafe`, `unwrap`, or `expect`. Use typed errors and preserve stable machine-readable error codes.
- Implement every project type conversion with `From<T>` when infallible or `TryFrom<T>` when fallible. Never create free-form conversion functions. Callers may use the corresponding `Into` or `TryInto` blanket traits.
- Writes are atomic, conflict-aware, and root-confined. Treat symlink handling, Windows path handling, and external modifications as security boundaries.
- Internal service writes must be structurally unable to recurse into index or operation-log processing.
- Use `tracing`, never `println!` or `eprintln!`, for runtime diagnostics. Never log vault content, tokens, secrets, or full sensitive paths at INFO or above.
- Reject malformed or unknown input. Never silently repair or degrade a write.
- Use Australian English spelling, grammar, and punctuation for operator-facing text and documentation, including the Oxford comma.
- Maximum 1000 lines per code file.
- Do not hard-wrap Markdown prose: one line per paragraph, soft-wrapped by the reader's editor. Wrap only where it adds real value (tables, code samples, or content that must align in a fixed-width view); where a hard limit is genuinely needed, use 120 characters.
- Keep changes scoped. Do not commit, push, rewrite history, or alter releases unless the user explicitly asks.
- Never add attribution to commit messages.

## Test standards

Tests are part of the design, not a closing activity.

- Unit tests cover pure domain rules and each error branch.
- Integration tests use real temporary vaults and, where relevant, real local Git repositories. Do not replace meaningful filesystem semantics with mocks.
- MCP contract tests exercise schemas, result shapes, stable errors, and both successful and rejected requests.
- Property tests cover path confinement, managed-block preservation, operation sequences, and other invariant-heavy behaviour.
- Cross-platform path tests include POSIX, Windows separators and prefixes, traversal, ADS rejection, and nested symlink escapes.
- Tests must be deterministic. Inject clocks, hashes, commit timers, and external providers. Never depend on network access, wall-clock sleeps, the operator's home directory, or test order.
- Do not weaken, delete, ignore, or over-broaden a test merely to make it pass.

Name or annotate tests with requirement IDs when the mapping is not obvious. Maintain the requirement-to-test matrix as the implementation grows.

## Rust quality gate

Run the narrowest relevant test during the red/green loop. Before declaring a code change complete, run the repository gate if present:

```sh
just ci
```

Until that task exists, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

Run [`.claude/scripts/check.sh`](.claude/scripts/check.sh) for repository guidance checks; once a Cargo workspace exists, it also runs the Rust quality gate. Never claim a check passed unless it ran successfully. Report any skipped check and its reason.

## Definition of done

A change is complete only when:

- applicable requirements and decisions are identified;
- the failing test was observed and the minimal implementation makes it pass;
- the change has been assessed holistically, not only against its focused test;
- relevant negative, security, and platform cases are covered;
- formatting, linting, tests, and dependency audit pass at the appropriate scope;
- operator-facing errors are actionable and secrets remain redacted;
- documentation and examples match behaviour; and
- the final report lists changed files, verification performed, and residual risks or deliberate follow-ups.

Never add attributions to commits.
