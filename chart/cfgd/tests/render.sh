#!/usr/bin/env bash
# Render the operator Deployment for each case below and compare its update
# strategy and readiness probe with the golden file of the same name.
#
# Usage: chart/cfgd/tests/render.sh           compare against the goldens
#        UPDATE=1 chart/cfgd/tests/render.sh  rewrite the goldens
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
chart="$(dirname "$here")"

# name|helm arguments
cases=(
  "default|"
  "gateway-with-leader-election|--set deviceGateway.enabled=true"
  "gateway-without-leader-election|--set deviceGateway.enabled=true --set operator.leaderElection.enabled=false"
  "override-recreate|--set operator.strategy.type=Recreate"
  "override-rolling|--set operator.strategy.type=RollingUpdate --set operator.strategy.rollingUpdate.maxUnavailable=2 --set operator.strategy.rollingUpdate.maxSurge=0"
)

failed=0
for entry in "${cases[@]}"; do
  name="${entry%%|*}"
  read -r -a args <<< "${entry#*|}"
  golden="$here/golden/$name.yaml"
  rendered="$(helm template cfgd "$chart" "${args[@]}" --show-only templates/operator-deployment.yaml \
    | yq '{"strategy": .spec.strategy, "readinessProbe": .spec.template.spec.containers[0].readinessProbe}')"
  if [ "${UPDATE:-0}" = 1 ]; then
    printf '%s\n' "$rendered" > "$golden"
    echo "UPDATED  $name"
  elif diff -u --label "$golden" --label "rendered $name" "$golden" <(printf '%s\n' "$rendered"); then
    echo "OK       $name"
  else
    failed=1
  fi
done
exit "$failed"
