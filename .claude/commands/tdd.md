---
description: Drive one behaviour through the red-green-refactor loop
argument-hint: <requirement identifier or behaviour description>
---

Deliver via test-driven development: $ARGUMENTS

1. Read `.claude/workflows/tdd.md` and the test-layer table in `.claude/rules/testing.md`.
2. Frame: one requirement, one acceptance example. State the precondition, action, observable result, and prohibited side effect. Choose the lowest test layer that proves the behaviour without hiding the real boundary at risk.
3. Red: add one focused, behaviour-named test. Run it with the narrowest target (`cargo test -p <crate> <name> -- --exact`) and show the failure output. Confirm the failure is the missing behaviour, not a syntax error or broken fixture. If the test unexpectedly passes, improve the test or demonstrate the behaviour already exists; never add production code without a meaningful red state.
4. Green: implement the smallest complete behaviour. No speculative configuration, generic abstractions, or future-phase functionality.
5. Refactor with the suite green; run the affected crate suite after structural changes.
6. Integrate: add a contract or integration test when the behaviour crosses an adapter; update the requirement-to-test matrix and documentation.

Report the red evidence, the green evidence, and which gate scope was run.
