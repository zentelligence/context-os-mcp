# Testing Strategy

Tests prove externally meaningful behaviour and architectural invariants. A large test count is not a substitute for covering the specified contracts.

## Test layers

| Layer | Purpose | Preferred dependencies |
| --- | --- | --- |
| Unit | Pure validation, parsing, reconciliation, state machines, and errors | In-memory values and focused fakes |
| Property | Path and sequence invariants over broad generated inputs | `proptest`-style generators with reproducible seeds |
| Integration | Real filesystem, atomic replacement, Git, indexes, and config | Isolated temporary directories and real local adapters |
| Contract | MCP schema, tool dispatch, results, warnings, and stable errors | In-process server composition with deterministic services |
| Transport | Framing, authentication, concurrency, shutdown, and parity | Spawned stdio/HTTP server on isolated resources |
| Acceptance | Host interoperability and vault recovery drills | A disposable copy of representative vault fixtures |

## Required patterns

- Use one temporary vault per test. Never read or mutate the operator's live vault, Git configuration, home directory, or global application data.
- Make fixture intent visible in its name and keep fixtures minimal. Put larger conformance corpora under `fixtures/` with provenance and expected results.
- Assert both state and side effects: file bytes, hashes, emitted events, warnings, staged paths, managed blocks, and logs as applicable.
- For failures, assert the stable code, relevant structured fields, and that no prohibited state changed.
- Test crash/cancellation boundaries without flaky process timing where possible. Use controllable fault injection around flush and rename steps.
- Keep manual acceptance scripts repeatable and document the exact host and MCP protocol revision used.

## Coverage priorities

Every documented error path must be reachable by a test. High-risk matrices take priority over line coverage: path forms and platforms, create/overwrite and conflict states, internal/tool origins, managed/unmanaged vaults, Git/no Git/degraded Git, and stdio/HTTP parity.

Never use snapshots as the sole assertion for security decisions. Review snapshot changes semantically, and keep stable contract fields asserted directly.

