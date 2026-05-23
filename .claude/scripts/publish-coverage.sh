#!/usr/bin/env bash
# Publish the coverage percentage to the orphan 'badges' branch as a Shields.io
# endpoint payload. Switches branches; intended for CI / release use only.
# Local runs that just need the number should use `task coverage:check`.
# Usage: publish-coverage.sh <cobertura.xml>
set -euo pipefail

XML="${1:?Usage: publish-coverage.sh <cobertura.xml>}"

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

git config user.email "github-actions[bot]@users.noreply.github.com"
git config user.name "github-actions[bot]"
git fetch origin badges:badges 2>/dev/null || true
if git show-ref --verify --quiet refs/heads/badges; then
  git checkout badges
else
  git checkout --orphan badges
  git rm -rf . > /dev/null 2>&1 || true
fi

BADGE="{\"schemaVersion\":1,\"label\":\"coverage\",\"message\":\"${COVERAGE}%\",\"color\":\"${COLOR}\"}"
echo "$BADGE" > coverage.json

git add coverage.json
git diff --cached --quiet || git commit -m "Update coverage to ${COVERAGE}%"
git push origin badges --force
