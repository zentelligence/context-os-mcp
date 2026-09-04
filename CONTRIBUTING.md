# Contributing to ContextOS Server

Thank you for your interest in contributing. This document is the entry point for both human and AI-agent contributors; it summarises the workflow enforced by [`AGENTS.md`](AGENTS.md), which remains the binding contract for every substantive change.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By participating, you are expected to uphold it.

## Before you start

1. Read [`AGENTS.md`](AGENTS.md) in full. It governs trait-first design, test-driven delivery, and the security and architecture rules every change must satisfy; nothing below weakens it.
2. Read [`.claude/rules/00-index.md`](.claude/rules/00-index.md) and load only the guidance relevant to the area you are changing (see the rule-routing table in `CLAUDE.md`).
3. Requirements and specification (`FR-*`, `NFR-*`, `D-*` identifiers) are maintained outside this repository. If you do not have access to them, say so in your pull request rather than guessing at intended behaviour.

## Reporting issues

Open an issue describing the problem or proposal before starting non-trivial work, so the approach can be agreed before code is written. Include reproduction steps for bugs, and the requirement or use case being addressed for feature proposals.

## Development workflow

1. Select a requirement and define its observable contract.
2. Add the smallest failing test and confirm it fails for the intended reason (red).
3. Implement only enough behaviour to make it pass (green).
4. Refactor with the suite green.
5. Run the affected crate's tests, then the workspace quality gate.

Bug fixes require a regression test that fails before the fix and passes after it.

## Quality gate

Run before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

Or run the full repository gate, including guidance checks:

```sh
.claude/scripts/check.sh
```

Never claim a check passed unless it actually ran and succeeded; note any skipped check and why in your pull request description.

## Engineering rules (summary)

The full rules live in `AGENTS.md` and `.claude/rules/`. In brief:

- No `unsafe`, `unwrap`, or `expect` in production paths; use typed errors with stable, machine-readable error codes.
- All tool paths become a validated `VaultPath` before entering a service; never accept a raw `Path`/`PathBuf` at a domain or service boundary.
- All mutations pass through the single write pipeline (validate, conflict check, atomic write, event routing). No handler or extension module writes to the filesystem directly.
- Project type conversions use `From`/`TryFrom` only; no free-form `to_*`/`from_*`/`convert_*` helpers.
- Keep dependency direction hexagonal, strictly towards `contextos-core`; library crates never depend on `contextos-mcp`.
- Use `tracing`, never `println!`/`eprintln!`; never log vault content, tokens, secrets, or full sensitive paths at INFO or above.
- Australian English spelling and grammar for operator-facing text and documentation, including the Oxford comma. Markdown prose is soft-wrapped, one line per paragraph.
- Maximum 1000 lines per code file.

## Commit and pull request expectations

- Keep changes scoped to the problem described in your issue or pull request.
- Do not rewrite history on shared branches.
- Do not add commit attributions such as `Co-Authored-By` trailers.
- Describe, in the pull request, what changed, what verification you ran (including any skipped checks and why), and any residual risk or deliberate follow-up.

## Licensing

By submitting a contribution, you agree that it is licensed under the terms in [`LICENSE`](LICENSE) (Apache License, Version 2.0).

## Getting help

For questions about the contribution process, open an issue or contact [support@zentelligence.com.au](mailto:support@zentelligence.com.au).
