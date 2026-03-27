#!/usr/bin/env bash
# Shared setup for operator E2E tests.
# Sourced by run-all.sh BEFORE domain test files.
# Sets up: helpers, infrastructure verification, namespace, cleanup trap, apply_yaml().

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
MANIFESTS="$SCRIPT_DIR/../manifests"

echo "=== cfgd Operator E2E Tests ==="

# --- Verify infrastructure is ready ---
echo "Verifying persistent infrastructure..."
kubectl wait --for=condition=available deployment/cfgd-operator \
    -n cfgd-system --timeout=30s
echo "Operator is running"

kubectl get validatingwebhookconfiguration cfgd-validating-webhooks > /dev/null 2>&1 || {
    echo "ERROR: Webhook configurations not found. Run setup-cluster.sh first."
    exit 1
}

# Wrapper: apply YAML and fail the current test (not the whole script) on error.
# Usage: apply_yaml "T03" <<'EOF' ... EOF
apply_yaml() {
    local test_id="$1"
    local yaml
    yaml=$(cat)
    local output
    if ! output=$(echo "$yaml" | kubectl apply -f - 2>&1); then
        echo "  kubectl apply failed: $output"
        fail_test "$test_id" "kubectl apply failed"
        return 1
    fi
    return 0
}

# Set up ephemeral namespace for test resources
create_e2e_namespace

# Disable set -e for the test body -- individual test failures are tracked by
# fail_test/pass_test, and print_summary returns non-zero if any test failed.
# This prevents a single webhook rejection or transient error from aborting all
# remaining tests with no summary.
set +e
trap 'cleanup_e2e; for ns in "e2e-team-alpha-${E2E_RUN_ID}" "e2e-team-beta-${E2E_RUN_ID}" "e2e-inject-${E2E_RUN_ID}"; do kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true; done' EXIT
