#!/usr/bin/env bash
# PostToolUse hook: report Markdown trailing whitespace immediately after an edit.
# Exit 2 feeds the message back to Claude without blocking the completed edit.
set -uo pipefail

payload="$(cat)"
file_path="$(jq -r '.tool_input.file_path // empty' <<<"${payload}")"

case "${file_path}" in
  *.md) ;;
  *) exit 0 ;;
esac

[[ -f "${file_path}" ]] || exit 0

if grep -qE '[[:blank:]]+$' "${file_path}"; then
  printf 'trailing whitespace in %s: fix before the completion gate.\n' \
    "${file_path}" >&2
  exit 2
fi

exit 0
