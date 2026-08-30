#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
cd "${repo_root}"

required_files=(
  AGENTS.md
  README.md
  .claude/README.md
  .claude/rules/00-index.md
  .claude/rules/architecture.md
  .claude/rules/mcp-contracts.md
  .claude/rules/memory.md
  .claude/rules/rust-quality.md
  .claude/rules/security.md
  .claude/rules/testing.md
  .claude/workflows/change-loop.md
  .claude/workflows/quality-gate.md
  .claude/workflows/tdd.md
  .claude/reviewers/code-review.md
  .claude/reviewers/specification-review.md
  .claude/templates/change-brief.md
  .claude/templates/requirement-test-matrix.md
)

for required_file in "${required_files[@]}"; do
  if [[ ! -s "${required_file}" ]]; then
    printf 'missing or empty required file: %s\n' "${required_file}" >&2
    exit 1
  fi
done

if ! command -v rg >/dev/null 2>&1; then
  printf 'ripgrep (rg) is required to validate repository Markdown files.\n' >&2
  exit 1
fi

if rg --line-number '[[:blank:]]+$' AGENTS.md README.md .claude --glob '*.md'; then
  printf 'trailing whitespace found in Markdown files\n' >&2
  exit 1
fi

# THIRD_PARTY_LICENSES.md reproduces upstream licence texts verbatim; those texts can contain
# bracket-paren sequences that look like Markdown links but point at paths inside the original
# upstream source tree, not this repository. Excluded from link validation for that reason only.
mapfile -t markdown_files < <(rg --files -g '*.md' -g '!THIRD_PARTY_LICENSES.md' . | sort)
for markdown_file in "${markdown_files[@]}"; do
  while IFS= read -r markdown_link; do
    link_target="${markdown_link#*](}"
    link_target="${link_target%)}"
    link_target="${link_target%%#*}"

    case "${link_target}" in
      ''|'#'*|http:*|https:*|mailto:*) continue ;;
    esac

    link_target="${link_target#<}"
    link_target="${link_target%>}"
    link_source_dir="$(dirname -- "${markdown_file}")"
    if [[ ! -e "${link_source_dir}/${link_target}" ]]; then
      printf 'broken local Markdown link: %s -> %s\n' \
        "${markdown_file}" "${link_target}" >&2
      exit 1
    fi
  done < <(rg --no-filename --only-matching \
    '\[[^]]+\]\([^)]*\)' "${markdown_file}" || true)
done

if ! command -v shellcheck >/dev/null 2>&1; then
  printf 'shellcheck is required to validate repository shell scripts.\n' >&2
  exit 1
fi

shellcheck .claude/scripts/check.sh .claude/hooks/rustfmt-check.sh .claude/hooks/markdown-check.sh

if [[ ! -f Cargo.toml ]]; then
  printf 'Guidance checks passed. Cargo workspace not yet present; Rust checks skipped.\n'
  exit 0
fi

if command -v just >/dev/null 2>&1 && just --list 2>/dev/null | rg --quiet '^\s*ci\b'; then
  just ci
  exit 0
fi

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

if ! command -v cargo-audit >/dev/null 2>&1; then
  printf 'cargo-audit is required for the completion gate but is not installed.\n' >&2
  exit 1
fi

cargo audit
