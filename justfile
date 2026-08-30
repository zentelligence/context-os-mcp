set shell := ["bash", "-euo", "pipefail", "-c"]

fmt:
    cargo fmt --all --check

clippy:
    # `--features semantic-local` rather than `--all-features`: the only other
    # feature in the workspace is contextos-search's `embedvec-unreleased-flush-sync`,
    # deliberately off by default because it calls a method that exists only on
    # Peter's local, unmerged `embedvec` checkout, not the published crate this
    # workspace's dev-dependency pins. `--all-features` would force it on and
    # break the lint run against a dependency that can't build it.
    cargo clippy --workspace --all-targets --features semantic-local -- -D warnings

test:
    cargo test --workspace --all-features

audit:
    cargo audit

ci: fmt clippy test audit
    shellcheck .claude/scripts/check.sh .claude/hooks/rustfmt-check.sh .claude/hooks/markdown-check.sh

