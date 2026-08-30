---
description: Run the completion quality gate and report evidence honestly
allowed-tools: Bash(.claude/scripts/check.sh), Bash(just:*), Bash(cargo fmt:*), Bash(cargo clippy:*), Bash(cargo test:*), Bash(cargo audit), Bash(git status:*), Bash(git diff:*)
---

Run the completion gate per `.claude/workflows/quality-gate.md`.

1. Run `.claude/scripts/check.sh` (guidance checks, then `just ci`: fmt, clippy `-D warnings`, workspace tests, audit, shellcheck).
2. Run any phase gate, contract suite, or fixture suite applicable to the current change, per the delivery plan.
3. Review the working diff for accidental scope, debug output, and secret exposure before reporting.

Report completion evidence in exactly four categories:

- checks that passed;
- checks that failed and why;
- checks not run and why; and
- manual or platform-specific validation still required.

Warnings, flaky reruns, ignored tests, and unavailable tooling are not a clean pass. Never claim a phase gate passed from a narrower suite, and never report a check as passed unless it ran in this session.
