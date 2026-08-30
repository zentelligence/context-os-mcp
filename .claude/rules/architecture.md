# Architecture Rules

The architecture is hexagonal. Domain contracts remain independent of MCP, Tokio, filesystem, Git, database, and transport implementations.

## Dependency boundaries

- `contextos-core` owns `VaultPath`, vault identity, operation events, domain errors, and ports. It has no adapter dependencies.
- Capability crates depend on core and, only where specified, lower-level codec crates. They never import `contextos-server`.
- `contextos-server` is the composition root and MCP adapter. Tool handlers translate requests to domain types, call services, and translate results.
- Extension modules use injected core services. They do not bypass validation, logging, versioning, or indexing.
- Keep transport types out of service signatures. Convert at the adapter edge with `From` or `TryFrom` and validate before calling a service. Do not hide boundary translation in free-form helper functions.

## Mutation boundary

Every mutation follows this order:

```text
request -> validate -> conflict check -> temp write + fsync + atomic rename
        -> OperationEvent -> index / operation log / Git / search
```

The completed write is the primary contract. Downstream service failures become typed warnings and self-healing work, not a false report that the write failed. Do not roll back a successful user write because a secondary service failed.

`Origin::Internal` prevents index and operation-log recursion. Internal writes still reach Git so one debounced commit includes the initiating operation and its derived changes. Express this routing in types or explicit policy, not scattered string comparisons.

## Concurrency and testability

- Serialise mutations per vault; allow independent reads to proceed.
- Do not hold a vault lock across unrelated network or embedding work.
- Inject clocks, filesystem/atomic-write ports, commit schedulers, hashes, and external providers where determinism or failure testing requires it.
- Prefer small capability traits and domain-specific fakes. Do not create a generic service locator.
- Keep derived data rebuildable and confined to `.contextos/`.

Any proposed boundary change must state affected `D-*` decisions, dependency direction, failure policy, and how it will be tested before implementation.
