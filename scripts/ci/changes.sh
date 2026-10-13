#!/usr/bin/env bash
# Path flags for CI (#35 §2). Workflows filter jobs on these flags instead of `on.paths`, so a
# required aggregate (`ci-ok`, `parity-ok`) always runs and a skipped job reports as skipped.
#
#   BASE_SHA=<base> HEAD_SHA=<head> scripts/ci/changes.sh
#
# On a pull_request it diffs HEAD_SHA against its merge base with BASE_SHA and writes
# `<flag>=true|false` lines to $GITHUB_OUTPUT (stdout when unset). Any other event, or a change
# to a workflow or to this script, sets every flag.
set -euo pipefail

pattern() {
  case "$1" in
    rust) echo '^(Cargo\.(toml|lock)|rust-toolchain\.toml|deny\.toml|\.cargo/|crates/|xtask/)' ;;
    desktop) echo '^(Cargo\.(toml|lock)|rust-toolchain\.toml|crates/(desktop|client|api)/)' ;;
    ts) echo '^(src/|tests/|prompts/|package\.json$|bun\.lock$|tsconfig\.json$|bunfig\.toml$)|\.(ts|mjs)$' ;;
    parity) echo '^(parity/|crates/parity/)' ;;
    brew) echo '^(Formula/|Casks/|scripts/release/brew\.sh$)' ;;
    docs) echo '^docs/|\.md$' ;;
    lockfile) echo '(^|/)(Cargo\.lock|package-lock\.json)$' ;;
  esac
}

all=false
changed=""
if [[ "${GITHUB_EVENT_NAME:-}" != "pull_request" ]]; then
  all=true
else
  changed="$(git diff --name-only "$BASE_SHA...$HEAD_SHA")"
  printf 'Changed files:\n%s\n' "$changed" >&2
  if grep -Eq '^(\.github/workflows/|scripts/ci/)' <<< "$changed"; then all=true; fi
fi

for flag in rust desktop ts parity brew docs lockfile; do
  value=$all
  if [[ $all == false ]] && grep -Eq "$(pattern "$flag")" <<< "$changed"; then value=true; fi
  echo "$flag=$value" >> "${GITHUB_OUTPUT:-/dev/stdout}"
done
