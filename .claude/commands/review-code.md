---
description: Review the working diff through the .claude code-review lens
argument-hint: [optional ref range, defaults to the working diff]
---

Review the current change through the repository code-review lens.

1. Determine the diff under review: $ARGUMENTS if a ref range was given, otherwise the working tree diff (staged and unstaged) against HEAD.
2. Use the `code-reviewer` subagent to apply `.claude/reviewers/code-review.md` to that diff: correctness and integrity, security and privacy, architecture and Rust, and test evidence.
3. Relay the findings ordered by severity, each with file and line evidence and the violated invariant or requirement identifier.
4. If there are no findings, state that explicitly and list residual untested risks. Do not inflate style preferences into defects.

Review only; do not modify code as part of this command. Propose fixes as findings for the user to approve.
