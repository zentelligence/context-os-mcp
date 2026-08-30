# Specification Conformance Lens

Use this review for behaviour, schema, architecture, or delivery claims.

1. List the affected `FR-*`, `NFR-*`, and `D-*` identifiers.
2. Compare request parameters, defaults, limits, results, warnings, and error codes with the relevant tool catalogue.
3. Check the write-pipeline, service failure policy, configuration precedence, managed-vault degradation, and transport parity where applicable.
4. Map each acceptance statement and documented error to a test.
5. Check whether the claimed delivery phase gate is fully satisfied.
6. Identify unspecified behaviour that became public or persisted accidentally.

Classify gaps as:

- **violation:** contradicts a requirement or decision;
- **missing:** required behaviour or test is absent;
- **drift:** code, docs, schemas, or tests disagree;
- **ambiguity:** authoritative inputs do not decide the behaviour; or
- **future scope:** valid idea outside the current delivery phase.

Never resolve an ambiguity by silently choosing the easiest implementation. Present the alternatives, affected identifiers, and compatibility consequences.

