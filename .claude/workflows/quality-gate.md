# Quality Gate

Verification is proportional during development and comprehensive before a
change is declared complete.

Passing commands is necessary but not sufficient. Completion also requires a
holistic review of correctness, security, data integrity, architecture,
cross-platform behaviour, interoperability, performance, operability,
maintainability, and documentation wherever the change can affect them.

## During the loop

Run the smallest target that demonstrates red or green, for example:

```sh
cargo test -p contextos-core vault_path_rejects_parent_escape -- --exact
cargo test -p contextos-fs atomic_write
```

Use the actual test target and name; do not cargo-cult these examples.

## Before completion

If the repository supplies `just ci`, it is the authoritative completion gate. Otherwise run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo audit
```

Also run any applicable phase gate, contract suite, platform suite, or manual
acceptance procedure from the delivery specification. Documentation-only work
must at least run `.claude/scripts/check.sh` and inspect rendered links.

## Versioning

Bump `[workspace.package] version` in the root `Cargo.toml` as part of closing any change that adds, removes, or alters a tool, schema, config field, or other operator-visible behaviour; a documentation-only or purely internal refactor does not need a bump. Per the delivery plan's "Packaging and release" section (location recorded in `CLAUDE.local.md`), the scheme is `MAJOR.MINOR.PATCH`; while the project remains pre-1.0, bump MINOR for a completed delivery-plan phase or any behaviour-visible change, and PATCH for a fix within an already-closed phase. Move to `1.0.0` only once every delivery-plan phase and its environment-specific acceptance work (`CLAUDE.md` "Current state") has closed, not merely once Linux CI is green. This is a separate number from `protocol_version` in `vault_info`, which tracks `rmcp::model::ProtocolVersion::LATEST` automatically and is never hand-edited.

## Completion evidence

The final report distinguishes:

- checks that passed;
- checks that failed and why;
- checks not run and why; and
- manual or platform-specific validation still required.

Warnings, flaky reruns, ignored tests, and unavailable audit tooling are not a
clean pass. Never claim that a phase gate passed from a narrower unit suite.
