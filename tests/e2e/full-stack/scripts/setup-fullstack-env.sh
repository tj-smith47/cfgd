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

command -v cosign >/dev/null 2>&1 || {
    echo "ERROR: cosign is required: OCI-E2E-02 signs a module with it and OCI-E2E-03 needs it for the signature policy. Install it (https://docs.sigstore.dev/cosign/system_config/installation/, or go install github.com/sigstore/cosign/v2/cmd/cosign@latest), then rerun the suite." >&2
    exit 1
}

require_release_webhooks_scoped || exit 1

# --- Verify infrastructure ---
echo "Verifying persistent infrastructure..."
kubectl wait --for=condition=available deployment/"$E2E_OPERATOR_DEPLOY" -n "$E2E_INSTALL_NS" --timeout=120s
kubectl wait --for=condition=available deployment/cfgd-server -n cfgd-system --timeout=120s
# Deployment Available can flip True before Service Endpoints repopulate during
# a rolling update, which makes admission webhook calls fail transiently with
# "no endpoints available for service". Block on real endpoints.
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_WEBHOOK_SVC" 120
wait_for_service_endpoints cfgd-system cfgd-server 120
wait_for_daemonset "$E2E_INSTALL_NS" "$E2E_CSI_DS" 120 || { echo "ERROR: PR CSI driver not ready" >&2; exit 1; }
echo "All persistent components running"

# Set up ephemeral namespace and test pod
create_e2e_namespace

cleanup_fullstack() {
    # Additional namespace cleanup
    for ns in "e2e-csi-test-${E2E_RUN_ID}" "e2e-csi-multi-${E2E_RUN_ID}" "e2e-csi-cache-${E2E_RUN_ID}" "e2e-csi-invalid-${E2E_RUN_ID}" "e2e-csi-update-${E2E_RUN_ID}" "e2e-csi-unmount-${E2E_RUN_ID}" "e2e-csi-ro-${E2E_RUN_ID}" "e2e-plugin-test-${E2E_RUN_ID}" "e2e-debug-flow-${E2E_RUN_ID}" "e2e-helm-01-${E2E_RUN_ID}" "e2e-helm-02-${E2E_RUN_ID}" "e2e-helm-03-${E2E_RUN_ID}" "e2e-helm-04-${E2E_RUN_ID}" "e2e-helm-05-${E2E_RUN_ID}" "e2e-helm-06-${E2E_RUN_ID}" "e2e-helm-08-${E2E_RUN_ID}" "e2e-helm-09-${E2E_RUN_ID}" "e2e-oci01-${E2E_RUN_ID}" "e2e-oci02-${E2E_RUN_ID}" "e2e-oci05-${E2E_RUN_ID}" "e2e-oci06-${E2E_RUN_ID}"; do
        kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true
    done
    # Delete run-scoped cluster-scoped resources
    kubectl delete module "csi-test-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-multi-a-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-multi-b-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "csi-update-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "debug-tools-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci01-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci02-signed-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci03-unsigned-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci04-multi-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci05-digest-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "oci06-auth-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete clusterconfigpolicy "oci03-no-unsigned-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
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
GW_API_KEY="${CFGD_E2E_API_KEY:-cfgd-e2e-admin-key}"
export GW_API_KEY
HEALTH_URL="http://cfgd-server.cfgd-system.svc.cluster.local:8081"
echo "Device gateway URL: $SERVER_URL"

# Wait for gateway reachability from test pod (use health endpoint — API requires auth)
echo "Waiting for device gateway..."
if ! wait_until 120 2 "${HEALTH_URL}/readyz from the test pod" \
    exec_in_pod curl -sf -o /dev/null "${HEALTH_URL}/readyz"; then
    echo "ERROR: Device gateway not reachable after 120s" >&2
    exit 1
fi
echo "All components are running"
