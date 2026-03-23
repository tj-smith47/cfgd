#!/usr/bin/env bash
# E2E Helm chart tests for cfgd DaemonSet deployment.
# Prereqs: k3s cluster running, cfgd image available, device gateway deployed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
CHART_DIR="$REPO_ROOT/chart/cfgd"
VALUES_FILE="$SCRIPT_DIR/../../helm/values-e2e-helm-test.yaml"

echo "=== cfgd Helm Tests ==="

create_e2e_namespace
trap 'helm uninstall cfgd -n "$E2E_NAMESPACE" 2>/dev/null || true; cleanup_e2e' EXIT

# =================================================================
# T20: Helm install creates DaemonSet
# =================================================================
begin_test "T20: Helm install"
helm install cfgd "$CHART_DIR" \
    -f "$VALUES_FILE" \
    -n "$E2E_NAMESPACE" \
    --set agent.image.tag=$IMAGE_TAG \
    --set agent.serverUrl=http://cfgd-server.cfgd-system.svc.cluster.local:8080 \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set operator.enabled=false \
    --wait --timeout 120s 2>&1 || true

DS_STATUS=$(kubectl get ds -n "$E2E_NAMESPACE" -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  DaemonSets: $DS_STATUS"

if echo "$DS_STATUS" | grep -q "cfgd"; then
    pass_test "T20"
else
    fail_test "T20" "DaemonSet not found after helm install"
    kubectl get all -n "$E2E_NAMESPACE" 2>/dev/null || true
fi

# =================================================================
# T21: DaemonSet pod is running
# =================================================================
begin_test "T21: DaemonSet pod running"
if wait_for_pod "$E2E_NAMESPACE" "app.kubernetes.io/name=cfgd" 120; then
    pass_test "T21"
else
    fail_test "T21" "DaemonSet pod did not reach Running state"
    kubectl describe pods -n "$E2E_NAMESPACE" -l "app.kubernetes.io/name=cfgd" 2>/dev/null | tail -30 || true
fi

# =================================================================
# T22: Pod logs show daemon activity
# =================================================================
begin_test "T22: Pod logs show daemon activity"
sleep 5  # let the daemon run at least one tick

POD=$(kubectl get pods -n "$E2E_NAMESPACE" -l "app.kubernetes.io/name=cfgd" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")

if [ -z "$POD" ]; then
    fail_test "T22" "No pod found"
else
    LOGS=$(kubectl logs "$POD" -n "$E2E_NAMESPACE" --tail=50 2>&1 || echo "")
    echo "  Pod logs (last 10 lines):"
    echo "$LOGS" | tail -10 | sed 's/^/    /'

    # The daemon should produce some output — either reconciliation or errors
    if [ -n "$LOGS" ]; then
        pass_test "T22"
    else
        fail_test "T22" "Pod logs are empty"
    fi
fi

# =================================================================
# T23: DaemonSet desired=ready
# =================================================================
begin_test "T23: DaemonSet desired equals ready"
DESIRED=$(kubectl get ds -n "$E2E_NAMESPACE" -l "app.kubernetes.io/name=cfgd" \
    -o jsonpath='{.items[0].status.desiredNumberScheduled}' 2>/dev/null || echo "0")
READY=$(kubectl get ds -n "$E2E_NAMESPACE" -l "app.kubernetes.io/name=cfgd" \
    -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo "0")

echo "  Desired: $DESIRED, Ready: $READY"
if [ "$DESIRED" != "0" ] && [ "$DESIRED" = "$READY" ]; then
    pass_test "T23"
else
    fail_test "T23" "DaemonSet not fully ready"
fi

# =================================================================
# T24: Helm upgrade succeeds
# =================================================================
begin_test "T24: Helm upgrade"
OUTPUT=$(helm upgrade cfgd "$CHART_DIR" \
    -f "$VALUES_FILE" \
    --set agent.image.tag=$IMAGE_TAG \
    --set agent.serverUrl=http://cfgd-server.cfgd-system.svc.cluster.local:8080 \
    --set agent.reconcileInterval="15s" \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set operator.enabled=false \
    -n "$E2E_NAMESPACE" \
    --wait --timeout 120s 2>&1) || true

if echo "$OUTPUT" | grep -qi "has been upgraded\|STATUS: deployed"; then
    pass_test "T24"
else
    # Helm may not print "upgraded" in all versions; check status instead
    STATUS=$(helm status cfgd -n "$E2E_NAMESPACE" 2>/dev/null | grep STATUS || echo "")
    if echo "$STATUS" | grep -qi "deployed"; then
        pass_test "T24"
    else
        fail_test "T24" "Helm upgrade did not succeed"
    fi
fi

# =================================================================
# T25: Helm uninstall cleans up
# =================================================================
begin_test "T25: Helm uninstall"
helm uninstall cfgd -n "$E2E_NAMESPACE" 2>&1 || true
sleep 5

# Check that no cfgd DaemonSet remains
DS_NAMES=$(kubectl get ds -n "$E2E_NAMESPACE" -l "app.kubernetes.io/name=cfgd" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")

if [ -z "$DS_NAMES" ]; then
    pass_test "T25"
else
    fail_test "T25" "DaemonSet still present after uninstall: $DS_NAMES"
fi

# --- Summary ---
print_summary "Helm Tests"
