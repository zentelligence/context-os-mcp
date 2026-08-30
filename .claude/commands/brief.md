---
description: Draft a change brief for a requirement before implementation
argument-hint: <FR/NFR/D identifiers or a feature description>
---

Draft a change brief for: $ARGUMENTS

1. Read `.claude/templates/change-brief.md` for the required structure, and
   `.claude/workflows/change-loop.md` for the discovery steps.
2. Identify the governing `FR-*`, `NFR-*`, and `D-*` identifiers and the delivery phase. Read the requirement rows and the relevant component specification from the location recorded in `CLAUDE.local.md` (or ask the repository owner if that file isn't present in your checkout). If the sources are unavailable, record the uncertainty and stop; do not invent semantics.
3. Inspect the working tree and the code and tests nearest the change so the brief reflects the actual baseline, not an assumed one.
4. Fill every section of the template: traceability, behaviour (given, when, then, must not), scope, holistic quality assessment, planned TDD evidence, verification commands, and risks.
5. Surface contract ambiguities explicitly per the source-of-truth precedence in `.claude/rules/00-index.md` rather than resolving them silently.
6. Present the brief in the conversation. Save it under the `development/` location recorded in `CLAUDE.local.md` (matching the existing `phase-N-change-brief.md` naming) only when the work is phase-scale or the user asks.

Stop after the brief. Do not begin implementation until the user confirms scope, or the task explicitly authorised implementation.
