#!/usr/bin/env bash
# Deploy a merged commit of this repo to the live daemon's code checkout, then restart it.
#
# The live daemon runs from a dedicated git worktree pinned to one commit (detached),
# not from the Claude Code plugin cache, so what runs is always a commit on main that
# passed CI. This script moves that pin and restarts the daemon.
#
#   LIVE_DIR=<worktree> LAUNCHD_LABEL=<label> scripts/deploy-live.sh            # deploy origin/main
#   LIVE_DIR=<worktree> LAUNCHD_LABEL=<label> scripts/deploy-live.sh <ref>      # deploy a ref on main
#   LIVE_DIR=<worktree> LAUNCHD_LABEL=<label> scripts/deploy-live.sh --rollback # back to the previous pin
#
# One-time setup: git worktree add --detach "$LIVE_DIR" origin/main
# and point the service's entry point at "$LIVE_DIR/src/index.ts".
set -euo pipefail

: "${LIVE_DIR:?set LIVE_DIR to the live worktree path}"
: "${LAUNCHD_LABEL:?set LAUNCHD_LABEL to the launchd label of the daemon}"
repo="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
prev_file="$(git -C "$LIVE_DIR" rev-parse --git-dir)/live-previous"

git -C "$repo" fetch -q origin

if [[ "${1:-}" == "--rollback" ]]; then
  [[ -s "$prev_file" ]] || { echo "deploy-live: no previous pin recorded" >&2; exit 1; }
  target="$(cat "$prev_file")"
else
  target="$(git -C "$repo" rev-parse --verify "${1:-origin/main}^{commit}")"
  # Only commits that reached main (and so passed the ruleset's required checks).
  git -C "$repo" merge-base --is-ancestor "$target" origin/main \
    || { echo "deploy-live: $target is not on origin/main; merge it through a PR first" >&2; exit 1; }
fi

current="$(git -C "$LIVE_DIR" rev-parse HEAD)"
if [[ "$current" == "$target" ]]; then
  echo "deploy-live: already at ${current:0:7}"
  exit 0
fi

git -C "$LIVE_DIR" checkout -q --detach "$target"
(cd "$LIVE_DIR" && bun install --frozen-lockfile --production >/dev/null)
echo "$current" > "$prev_file"

launchctl kickstart -k "gui/$(id -u)/$LAUNCHD_LABEL"
now="$(git -C "$LIVE_DIR" log -1 --format='%h %s')"
echo "deploy-live: ${now} [previous ${current:0:7}]"
echo "deploy-live: restarted $LAUNCHD_LABEL; check its startup log before walking away"
