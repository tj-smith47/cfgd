#!/usr/bin/env bash
# Render the chart for each case below and compare what the case covers with
# the golden file of the same name:
#   - deployment cases: the operator Deployment's update strategy and readiness probe
#   - cluster-scoped cases: every cluster-scoped object with its webhook selectors,
#     the CSI plugin paths on the node, and the env that names the CSI driver and
#     scopes the operator, since a second install beside a live release collides on these;
#     and the registry settings and login mount the operator and CSI driver read
#     module artifacts with
#
# Usage: chart/cfgd/tests/render.sh           compare against the goldens
#        UPDATE=1 chart/cfgd/tests/render.sh  rewrite the goldens
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
chart="$(dirname "$here")"
repo="$(cd "$chart/../.." && pwd)"

failed=0
check() {
  local name="$1" rendered="$2"
  local golden="$here/golden/$name.yaml"
  if [ "${UPDATE:-0}" = 1 ]; then
    printf '%s\n' "$rendered" > "$golden"
    echo "UPDATED  $name"
  elif diff -u --label "$golden" --label "rendered $name" "$golden" <(printf '%s\n' "$rendered"); then
    echo "OK       $name"
  else
    failed=1
  fi
}

# name|helm arguments
# One case per cell of the derived strategy (gateway, gateway persistence,
# leader election), then the explicit overrides.
deployment_cases=(
  "default|"
  "no-gateway-without-leader-election|--set operator.leaderElection.enabled=false"
  "gateway-with-leader-election|--set deviceGateway.enabled=true"
  "gateway-persistent-without-leader-election|--set deviceGateway.enabled=true --set operator.leaderElection.enabled=false"
  "gateway-ephemeral-without-leader-election|--set deviceGateway.enabled=true --set deviceGateway.persistence.enabled=false --set operator.leaderElection.enabled=false"
  "override-recreate|--set operator.strategy.type=Recreate"
  "override-rolling|--set operator.strategy.type=RollingUpdate --set operator.strategy.rollingUpdate.maxUnavailable=2 --set operator.strategy.rollingUpdate.maxSurge=0"
)

for entry in "${deployment_cases[@]}"; do
  name="${entry%%|*}"
  read -r -a args <<< "${entry#*|}"
  rendered="$(helm template cfgd "$chart" "${args[@]}" --show-only templates/operator-deployment.yaml \
    | yq '{"strategy": .spec.strategy, "readinessProbe": .spec.template.spec.containers[0].readinessProbe}')"
  check "$name" "$rendered"
done

# The e2e case carries JSON values, which a whitespace-split string cannot hold,
# so each case sets its own argument array. Each case also sets its release
# name, because the cluster-scoped object names derive from it.
cluster_scoped_args() {
  case "$1" in
    cluster-scoped-default)
      release=cfgd
      args=(--set csiDriver.enabled=true)
      ;;
    cluster-scoped-e2e)
      release=cfgd-e2e-42
      args=(
        -f "$repo/tests/e2e/manifests/pr-install-values.yaml"
        --set operator.image.repository=registry.example/cfgd-operator --set operator.image.tag=pr
        --set csiDriver.image.repository=registry.example/cfgd-csi --set csiDriver.image.tag=pr
        --set-string csiDriver.name=e2e.csi.cfgd.io
        --set-string operator.watchLabelSelector=cfgd.io/e2e-run=42
        --set-json 'webhook.objectSelector={"matchLabels":{"cfgd.io/e2e-run":"42"}}'
        --set-json 'mutatingWebhook.namespaceSelector={"matchExpressions":[{"key":"cfgd.io/inject-modules","operator":"In","values":["true"]},{"key":"cfgd.io/e2e-run","operator":"In","values":["42"]}]}'
      )
      ;;
  esac
}

# yq drops a key whose value is missing, so absent selectors fall back to an
# explicit null that the golden shows.
cluster_scoped_query='[.] |
  [.[] | select(.kind == "CSIDriver" or .kind == "ClusterRole" or .kind == "ClusterRoleBinding"
      or .kind == "ValidatingWebhookConfiguration" or .kind == "MutatingWebhookConfiguration")
    | {"kind": .kind, "name": .metadata.name,
       "selectors": [.webhooks[]? | {"name": .name,
         "objectSelector": (.objectSelector // null), "namespaceSelector": (.namespaceSelector // null)}]}]
  + [.[] | select(.kind == "DaemonSet")
    | {"kind": .kind, "name": .metadata.name,
       "registrationPath": [.spec.template.spec.containers[].args[]? | select(test("^--kubelet-registration-path="))],
       "pluginHostPath": [.spec.template.spec.volumes[] | select(.name == "plugin-dir") | .hostPath.path]}]
  + [.[] | select(.kind == "Deployment" or .kind == "DaemonSet")
    | {"kind": .kind, "name": .metadata.name,
       "env": [.spec.template.spec.containers[] | {"container": .name,
         "values": [.env[]? | select(.name == "CSI_DRIVER_NAME" or .name == "WATCH_LABEL_SELECTOR"
           or .name == "DOCKER_CONFIG")]}
         | select(.values | length > 0)]}]
  + [.[] | select(.kind == "Deployment" or .kind == "DaemonSet")
    | {"kind": .kind, "name": .metadata.name,
       "registryLogin": {
         "mounts": [.spec.template.spec.containers[] | {"container": .name,
           "mountPath": [.volumeMounts[]? | select(.name == "docker-config") | .mountPath]}
           | select(.mountPath | length > 0)],
         "secret": [.spec.template.spec.volumes[] | select(.name == "docker-config") | .secret.secretName]}}
    | select(.registryLogin.mounts | length > 0)]'

for name in cluster-scoped-default cluster-scoped-e2e; do
  cluster_scoped_args "$name"
  rendered="$(helm template "$release" "$chart" "${args[@]}" | yq ea "$cluster_scoped_query")"
  check "$name" "$rendered"
done
exit "$failed"
