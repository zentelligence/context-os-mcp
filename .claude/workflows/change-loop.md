# Change Loop

## Discover

1. Inspect repository status and nearby code/tests.
2. Read the requirement, design decision, component specification, and delivery gate that govern the change.
3. Write a concise change brief: in scope, out of scope, risks, and verification.
4. Resolve contract ambiguity before it reaches public schemas or persisted formats.
5. Assess system-wide effects: security, data integrity, architecture, interoperability, performance, operations, maintainability, and docs.

## Design

1. Place behaviour in the domain/service layer and translation at adapters.
2. Identify security boundaries, failure policy, concurrency, and cancellation.
3. List the test layers needed. Start with one red example.
4. Prefer the smallest complete change that leaves a coherent, production-grade public contract. Small scope never excuses incomplete quality.

## Implement

1. Follow the red-green-refactor loop.
2. Keep tool handlers thin and errors typed.
3. Run targeted tests frequently.
4. Inspect diffs for accidental scope, generated noise, debug output, and secret exposure.

## Verify and report

1. Run the applicable quality gate.
2. Review through both `reviewers/code-review.md` and `reviewers/specification-review.md` when product behaviour changes.
3. Report changed files, behaviour delivered, tests/checks run, and any unverified platform or manual acceptance work.

Do not opportunistically implement later delivery phases. Record useful follow-ups without broadening the current change.
