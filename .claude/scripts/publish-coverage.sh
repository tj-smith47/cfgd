#!/usr/bin/env bash
# Publish the coverage percentage to the orphan 'badges' branch as a Shields.io
# endpoint payload. Intended for CI / release use only. Local runs that just
# need the number should use `task coverage:check`.
# Usage: publish-coverage.sh <cobertura.xml>
#
# The badge lives on an orphan `badges` branch that carries only coverage.json
# and shares no history with master. Publishing it must NOT touch the primary
# checkout: GitHub Actions runs a post-step for the local composite action
# `./.github/actions/setup-rust` when the job ends, and that step needs
# `.github/actions/setup-rust/action.yml` present in the workspace. A
# `git checkout badges` in-place would replace the working tree with the
# orphan branch's contents (no action.yml), breaking that post-step. So all
# branch mutation happens in a throwaway `git worktree` under a temp dir; the
# primary checkout stays on the CI commit the whole time.
set -euo pipefail

XML="${1:?Usage: publish-coverage.sh <cobertura.xml>}"

# Refuse to run outside CI — this script pushes to origin and is meant for the
# release/CI pipeline only. Local dev should use `task coverage:check` instead.
if [ -z "${CI:-}${GITHUB_ACTIONS:-}" ]; then
  echo "error: publish-coverage.sh is CI-only (CI or GITHUB_ACTIONS env var required)." >&2
  echo "       For local coverage, run \`task coverage:check\` instead." >&2
  exit 2
fi

if [ ! -f "$XML" ]; then
  echo "::error::Coverage XML not found: $XML"
  exit 1
fi

# Reuse the same percentage extraction `task coverage:check` prints, so the
# README badge and local stdout never drift.
COVERAGE=$(bash "$(dirname "$0")/coverage-percent.sh" "$XML")
COVERAGE="${COVERAGE%\%}"

if (( $(echo "$COVERAGE >= 90" | bc -l) )); then COLOR="brightgreen"
elif (( $(echo "$COVERAGE >= 80" | bc -l) )); then COLOR="green"
elif (( $(echo "$COVERAGE >= 70" | bc -l) )); then COLOR="yellowgreen"
elif (( $(echo "$COVERAGE >= 60" | bc -l) )); then COLOR="yellow"
else COLOR="red"; fi

BADGE="{\"schemaVersion\":1,\"label\":\"coverage\",\"message\":\"${COVERAGE}%\",\"color\":\"${COLOR}\"}"

# Stage the badge work in an isolated worktree so the primary checkout never
# changes branch. The worktree (and any branch it checks out) is removed on
# exit regardless of outcome.
WORKTREE_DIR="$(mktemp -d)"
cleanup() {
  git worktree remove --force "$WORKTREE_DIR" 2>/dev/null || true
  rm -rf "$WORKTREE_DIR" 2>/dev/null || true
  git worktree prune 2>/dev/null || true
}
trap cleanup EXIT

git fetch origin badges 2>/dev/null || true

if git show-ref --verify --quiet refs/remotes/origin/badges; then
  # Existing branch: check it out into the worktree, tracking the remote tip.
  git worktree add --force -B badges "$WORKTREE_DIR" origin/badges
else
  # First publish: an orphan worktree with no parent history. `--orphan`
  # creates the branch with an empty index; the badge is its initial content.
  git worktree add --orphan -b badges "$WORKTREE_DIR"
fi

echo "$BADGE" > "$WORKTREE_DIR/coverage.json"

git -C "$WORKTREE_DIR" add coverage.json
if ! git -C "$WORKTREE_DIR" diff --cached --quiet; then
  git -C "$WORKTREE_DIR" \
    -c user.email="github-actions[bot]@users.noreply.github.com" \
    -c user.name="github-actions[bot]" \
    commit -m "Update coverage to ${COVERAGE}%"
  git -C "$WORKTREE_DIR" push origin badges --force
else
  echo "coverage unchanged at ${COVERAGE}%; nothing to publish"
fi
