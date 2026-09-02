set shell := ["bash", "-euo", "pipefail", "-c"]

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --all-features

audit:
    cargo audit

ci: fmt clippy test audit
    shellcheck .claude/scripts/check.sh .claude/hooks/rustfmt-check.sh .claude/hooks/markdown-check.sh

