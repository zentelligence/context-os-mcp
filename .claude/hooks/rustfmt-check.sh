#!/usr/bin/env bash
# PostToolUse hook: report rustfmt drift immediately after a Rust file edit.
# Exit 2 feeds the message back to Claude without blocking the completed edit.
set -uo pipefail

payload="$(cat)"
file_path="$(jq -r '.tool_input.file_path // empty' <<<"${payload}")"

case "${file_path}" in
  *.rs) ;;
  *) exit 0 ;;
esac

[[ -f "${file_path}" ]] || exit 0

if ! rustfmt --check "${file_path}" >/dev/null 2>&1; then
  printf 'rustfmt drift in %s: run "cargo fmt --all" before the quality gate.\n' \
    "${file_path}" >&2
  exit 2
fi

exit 0
