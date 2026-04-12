#!/usr/bin/env bash
# Update the coverage badge on the orphan 'badges' branch.
# Usage: update-coverage-badge.sh <cobertura.xml>
set -euo pipefail

XML="${1:?Usage: update-coverage-badge.sh <cobertura.xml>}"

if [ ! -f "$XML" ]; then
  echo "::error::Coverage XML not found: $XML"
  exit 1
fi

COVERAGE=$(grep -oP 'line-rate="\K[^"]+' "$XML" | head -1 | awk '{printf "%.1f", $1 * 100}')

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
