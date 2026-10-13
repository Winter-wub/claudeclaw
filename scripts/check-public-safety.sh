#!/usr/bin/env bash
# Fails if text that must never reach this PUBLIC repo is about to be committed or is
# already tracked: absolute home paths, Discord-style snowflake IDs, and anything in the
# optional local deny list. Secrets (tokens, keys) are left to gitleaks, run alongside.
#
#   scripts/check-public-safety.sh --staged   # added lines in the index (pre-commit hook)
#   scripts/check-public-safety.sh --all      # every tracked file (CI)
#
# Placeholders that look like IDs are allowed by exact match in .public-safety-allow.
# Personal strings (names, emails, real IDs) go in .git/info/public-safety-deny, one per
# line: that file is local-only, so the deny list itself is never published.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

mode="${1:---staged}"
allow_file=".public-safety-allow"
deny_file="$(git rev-parse --git-dir)/info/public-safety-deny"

# Patterns: /Users/<name>/ or /home/<name>/ with a real-looking name (not "/Users/..."
# in prose), and 17-20 digit snowflake-style IDs.

case "$mode" in
  --staged) text="$(git diff --cached --no-color -U0 --diff-filter=ACMR -- . ':!bun.lock' | grep -E '^\+[^+]' || true)" ;;
  --all)    text="$(git grep -nI -e '' -- . ':!bun.lock' ':!.public-safety-allow' || true)" ;;
  *) echo "usage: $0 [--staged|--all]" >&2; exit 2 ;;
esac

# One grep pass for candidates; drop exact allow-listed placeholders, then re-check.
pattern='/(Users|home)/[A-Za-z0-9._-]*[A-Za-z0-9_]/|(^|[^0-9.])[0-9]{17,20}([^0-9]|$)'
candidates="$(printf '%s\n' "$text" | grep -E -- "$pattern" || true)"
# Candidates are few, so a per-line loop is cheap here.
allow=()
[[ -f "$allow_file" ]] && while IFS= read -r a; do
  [[ -z "$a" || "$a" == \#* ]] || allow+=("$a")
done < "$allow_file"
hits=""
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  stripped="$line"
  for a in "${allow[@]+"${allow[@]}"}"; do stripped="${stripped//"$a"/}"; done
  if printf '%s\n' "$stripped" | grep -Eq -- "$pattern"; then hits+="$line"$'\n'; fi
done <<< "$candidates"

if [[ -f "$deny_file" ]]; then
  while IFS= read -r d; do
    [[ -z "$d" || "$d" == \#* ]] && continue
    found="$(printf '%s\n' "$text" | grep -F -- "$d" || true)"
    [[ -n "$found" ]] && hits+="$found"$'\n'
  done < "$deny_file"
fi

if [[ -n "$hits" ]]; then
  echo "public-safety: this repo is PUBLIC; these lines look like personal data:" >&2
  printf '%s' "$hits" | sort -u | cut -c1-200 >&2
  echo "Replace with a placeholder, or add an exact placeholder string to $allow_file." >&2
  exit 1
fi

if [[ "$mode" == "--staged" ]] && command -v gitleaks >/dev/null 2>&1; then
  gitleaks git --staged --no-banner --redact || { echo "public-safety: gitleaks found a secret" >&2; exit 1; }
fi
echo "public-safety: ok ($mode)"
