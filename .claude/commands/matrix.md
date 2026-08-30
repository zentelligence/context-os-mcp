---
description: Create or update the requirement-to-test matrix for the current work
argument-hint: <phase or feature>
---

Maintain the requirement-to-test matrix for: $ARGUMENTS

1. Read `.claude/templates/requirement-test-matrix.md` for the required structure, and an existing `phase-N-requirement-test-matrix.md` in the `development/` location recorded in `CLAUDE.local.md` for the established level of detail.
2. Keep rows at observable-contract granularity: requirement, contract summary, test layer, and named tests once paths exist.
3. Maintain the error-coverage table (stable code, trigger, no-change assertion, contract test) and the gate-evidence table mapping each delivery-plan gate item to automated and manual evidence.
4. Set status honestly: `green` only when the named test exists and passed in a run you performed; `red` for an observed failing test; `pending` otherwise. Note environment-specific items (Windows, macOS, Obsidian desktop, host acceptance) as pending with the reason.
5. Save under the `development/` location recorded in `CLAUDE.local.md`, following the existing per-phase naming.
