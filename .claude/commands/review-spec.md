---
description: Check behaviour, schemas, and delivery claims against the specification
argument-hint: <requirement identifiers, tool names, or a phase>
---

Run a specification conformance review of: $ARGUMENTS

1. Use the `spec-reviewer` subagent to apply `.claude/reviewers/specification-review.md`. The authoritative inputs are at the location recorded in `CLAUDE.local.md` (or ask the repository owner if that file isn't present in your checkout). If they are unreadable, report  that and stop; never review against remembered or invented semantics.
2. Relay each gap classified as violation, missing, drift, ambiguity, or future scope, with the affected `FR-*`, `NFR-*`, and `D-*` identifiers and the evidence.
3. For ambiguities, present the alternatives and compatibility consequences; never resolve one by silently choosing the easiest implementation.
4. State whether any claimed delivery phase gate is fully satisfied, and which gate items lack evidence.
