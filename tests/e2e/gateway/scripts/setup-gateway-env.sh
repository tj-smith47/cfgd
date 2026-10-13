#!/usr/bin/env bash
# Shared setup for gateway E2E tests.
# Sourced by run-all.sh BEFORE domain test files.
# Waits for the PR install's gateway, creates ephemeral namespace,
# port-forwards to gateway, reads admin key, creates bootstrap token.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/../../common/helpers.sh"

command -v jq >/dev/null 2>&1 || { echo "ERROR: jq is required for gateway tests" >&2; exit 1; }

require_release_webhooks_scoped || exit 1

echo "=== cfgd Gateway E2E Tests ==="

# --- Verify the install's gateway is serving ---
echo "Verifying the gateway of $E2E_INSTALL_RELEASE..."
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_GATEWAY_SVC" 60
echo "Gateway Service has its lease holder as endpoint"

# --- Create ephemeral namespace ---
create_e2e_namespace

# --- Port-forward to gateway ---
# The API goes through the Service; the probe port is the lease holder's own,
# since the gateway Service publishes only the gateway port.
GW_PORT=18080
GW_HEALTH_PORT=18081
echo "Port-forwarding to gateway on localhost:$GW_PORT..."
GW_LEADER_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_GATEWAY_LEADER_PODS" \
    -o jsonpath='{.items[*].metadata.name}')
if [ -z "$GW_LEADER_POD" ] || [ "$GW_LEADER_POD" != "${GW_LEADER_POD%% *}" ]; then
    echo "ERROR: want exactly one pod labelled $E2E_GATEWAY_LEADER_PODS in $E2E_INSTALL_NS, found [${GW_LEADER_POD}]" >&2
    exit 1
fi
PF_PID=$(port_forward "$E2E_INSTALL_NS" "svc/$E2E_GATEWAY_SVC" "$GW_PORT" "$E2E_GATEWAY_PORT")
PF_HEALTH_PID=$(port_forward "$E2E_INSTALL_NS" "pod/$GW_LEADER_POD" "$GW_HEALTH_PORT" 8081)
GW_URL="http://localhost:$GW_PORT"

wait_for_url "$GW_URL/api/v1/enroll/info" 30

echo "Gateway reachable at $GW_URL (lease holder pod/$GW_LEADER_POD)"

# --- Admin API key: the one setup gave the install ---
ADMIN_KEY=$(kubectl get deployment "$E2E_OPERATOR_DEPLOY" -n "$E2E_INSTALL_NS" \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CFGD_API_KEY")].value}')
if [ "$ADMIN_KEY" != "$E2E_GATEWAY_API_KEY" ]; then
    echo "ERROR: deployment/$E2E_OPERATOR_DEPLOY sets CFGD_API_KEY to [${ADMIN_KEY}], not the key setup passes as deviceGateway.apiKey; rerun setup" >&2
    exit 1
fi

# --- Admin-API helpers, defined here because this file sources first ---
# Every domain file sees them, so none carries its own guarded copy: a
# `declare -f` fallback in four files is four definitions to keep in step, and
# a case reaching the wrong copy would differ only in which header it sent.

# Build the auth header for admin API calls.
gw_admin_auth_header() {
    if [ -n "${ADMIN_KEY:-}" ]; then
        echo "Authorization: Bearer $ADMIN_KEY"
    else
        # Open mode: no auth needed, but curl -H "" is harmless
        echo "X-No-Auth: open-mode"
    fi
}

# Create a fresh bootstrap token via the admin API. Prints the token string, and
# nothing at all when the API refused.
gw_create_bootstrap_token() {
    local username="${1:-e2e-user}"
    local resp
    resp=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
        -H "Content-Type: application/json" \
        -H "$(gw_admin_auth_header)" \
        -d "{\"username\":\"$username\",\"team\":\"e2e-team\",\"expiresIn\":3600}" 2>/dev/null)
    echo "$resp" | jq -r '.token // empty' 2>/dev/null
}

# --- Create bootstrap token for enrollment tests ---
gateway_device GW_DEVICE_ID "e2e-device-${E2E_RUN_ID}"
# `|| true` because this file runs under `set -e` and a refused mint is a warning
# here: the cases that need a token check for one themselves.
BOOTSTRAP_TOKEN=$(gw_create_bootstrap_token "e2e-user" || true)

if [ -n "$BOOTSTRAP_TOKEN" ]; then
    echo "Bootstrap token created"
else
    echo "WARN: Failed to create bootstrap token"
fi

# --- Export environment for domain test files ---
export GW_URL GW_PORT GW_HEALTH_PORT GW_LEADER_POD PF_PID PF_HEALTH_PID ADMIN_KEY BOOTSTRAP_TOKEN GW_DEVICE_ID

echo "Gateway URL: $GW_URL"
echo "Bootstrap token available: $([ -n "$BOOTSTRAP_TOKEN" ] && echo yes || echo no)"
echo "Admin key available: $([ -n "$ADMIN_KEY" ] && echo yes || echo no)"
echo "Device ID: $GW_DEVICE_ID"
