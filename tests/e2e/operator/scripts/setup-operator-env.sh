#!/usr/bin/env bash
# Shared setup for operator E2E tests.
# Sourced by run-all.sh BEFORE domain test files.
# Sets up: helpers, infrastructure verification, namespace, cleanup trap, apply_yaml().
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"

echo "=== cfgd Operator E2E Tests ==="

kubectl get validatingwebhookconfiguration "$E2E_VALIDATING_WEBHOOK" > /dev/null 2>&1 || {
    echo "ERROR: validatingwebhookconfiguration $E2E_VALIDATING_WEBHOOK of the PR install not found. Run setup-cluster.sh first."
    exit 1
}

require_release_webhooks_scoped || exit 1

# Admission calls fail with "no endpoints available for service" while the
# webhook Service is between pods (a rollout, a node restart), so the suite
# starts only once it has a ready endpoint.
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_WEBHOOK_SVC" 120

# Wrapper: apply YAML and fail the current test (not the whole script) on error.
# Usage: apply_yaml "T03" <<EOF ... EOF
apply_yaml() {
    local test_id="$1"
    local output
    if ! output=$(kubectl apply -f - 2>&1); then
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
trap 'for ns in "e2e-team-alpha-${E2E_RUN_ID}" "e2e-team-beta-${E2E_RUN_ID}" "e2e-inject-${E2E_RUN_ID}" "e2e-ns-a-${E2E_RUN_ID}" "e2e-ns-b-${E2E_RUN_ID}" "e2e-backup-${E2E_RUN_ID}"; do kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true; done; cleanup_e2e' EXIT
