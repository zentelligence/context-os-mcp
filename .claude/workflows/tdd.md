# Test-Driven Delivery Workflow

Use this loop for each observable behaviour, not once per broad feature.

## 1. Frame

- Select one requirement and one acceptance example.
- State the precondition, action, observable result, and prohibited side effect.
- Choose the lowest test layer that can prove the behaviour without hiding the
  real boundary at risk.

## 2. Red

- Add one focused test with a behaviour-oriented name.
- Run that test and observe failure.
- Confirm the failure is caused by missing or incorrect behaviour, not a syntax
  error, broken fixture, or unrelated failure.
- If the test unexpectedly passes, improve the test or show that the behaviour
  already exists. Do not add production code without a meaningful red state.

## 3. Green

- Implement the smallest complete behaviour that makes the test pass.
- Run the focused test, then nearby tests.
- Do not add speculative configuration, generic abstractions, or future-phase
  functionality.

## 4. Refactor

- Improve names, duplication, boundaries, and error clarity while tests remain
  green.
- Add table/property cases when the first example reveals a meaningful input
  matrix.
- Run the affected crate suite after structural changes.

## 5. Integrate

- Add a contract or integration test when the behaviour crosses an adapter.
- Update requirement traceability and documentation.
- Run the completion gate in `quality-gate.md`.

Documentation-only, formatting-only, and generated artefact changes do not need
an artificial failing unit test. They still require an appropriate validation
check. Characterisation work may begin with a passing test only when its stated
purpose is to lock down existing behaviour before a later red test.

