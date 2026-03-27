#!/usr/bin/env bash
# Shared setup for full-stack E2E tests.
# Sourced by run-all.sh (and domain files are sourced into that same process).
# Verifies persistent infrastructure, creates ephemeral namespace + test pod,
# copies fixtures, builds cfgd binary, sets up kubectl plugin, checks gateway.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
NODE_FIXTURES="$SCRIPT_DIR/../../node/fixtures"

echo "=== cfgd Full-Stack E2E Tests ==="

# --- Verify infrastructure ---
echo "Verifying persistent infrastructure..."
kubectl wait --for=condition=available deployment/cfgd-operator -n cfgd-system --timeout=30s
kubectl wait --for=condition=available deployment/cfgd-server -n cfgd-system --timeout=30s
echo "All persistent components running"

# Check CSI driver
CSI_READY=$(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver \
    -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo "0")
CSI_AVAILABLE=true
if [ "$CSI_READY" = "0" ] || [ -z "$CSI_READY" ]; then
    echo "WARN: CSI driver not ready, CSI tests will be skipped"
    CSI_AVAILABLE=false
fi

# Set up ephemeral namespace and test pod
create_e2e_namespace

cleanup_fullstack() {
    # Additional namespace cleanup
    for ns in "e2e-csi-test-${E2E_RUN_ID}" "e2e-csi-multi-${E2E_RUN_ID}" "e2e-csi-cache-${E2E_RUN_ID}" "e2e-csi-invalid-${E2E_RUN_ID}" "e2e-csi-update-${E2E_RUN_ID}" "e2e-csi-unmount-${E2E_RUN_ID}" "e2e-csi-ro-${E2E_RUN_ID}" "e2e-plugin-test-${E2E_RUN_ID}" "e2e-debug-flow-${E2E_RUN_ID}"; do
        kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true
    done
    # Delete run-scoped cluster-scoped resources
    kubectl delete module "csi-test-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-multi-a-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-multi-b-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-update-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "debug-tools-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    # Clean up namespaced CRDs in test and cfgd-system namespaces
    for ns in "$E2E_NAMESPACE" cfgd-system; do
        for kind in machineconfig configpolicy driftalert; do
            kubectl delete "$kind" -l "$E2E_RUN_LABEL" -n "$ns" --ignore-not-found 2>/dev/null || true
        done
    done
    cleanup_e2e
}
trap 'cleanup_fullstack' EXIT

ensure_test_pod

# Copy fixtures to test pod
echo "Copying fixtures..."
exec_in_pod mkdir -p /etc/cfgd/profiles
cp_to_pod "$NODE_FIXTURES/configs/cfgd.yaml" /etc/cfgd/cfgd.yaml
for f in "$NODE_FIXTURES/profiles/"*.yaml; do
    cp_to_pod "$f" "/etc/cfgd/profiles/$(basename "$f")"
done

# Build cfgd binary on host for kubectl plugin tests
ensure_cfgd_binary
KUBECTL_CFGD="/tmp/kubectl-cfgd"
ln -sf "$CFGD_BIN" "$KUBECTL_CFGD"

SERVER_URL="http://cfgd-server.cfgd-system.svc.cluster.local:8080"
echo "Device gateway URL: $SERVER_URL"

# Wait for gateway reachability from test pod
echo "Waiting for device gateway..."
GATEWAY_READY=false
for i in $(seq 1 60); do
    if exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1; then
        GATEWAY_READY=true
        break
    fi
    sleep 2
done

if [ "$GATEWAY_READY" = "false" ]; then
    echo "ERROR: Device gateway not reachable after 120s"
    exit 1
fi
echo "All components are running"
