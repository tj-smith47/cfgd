#!/usr/bin/env bash
# Shared setup for gateway E2E tests.
# Sourced by run-all.sh BEFORE domain test files.
# Verifies gateway deployment, creates ephemeral namespace,
# port-forwards to gateway, extracts admin key, creates bootstrap token.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"

command -v jq >/dev/null 2>&1 || { echo "ERROR: jq is required for gateway tests"; exit 1; }

echo "=== cfgd Gateway E2E Tests ==="

# --- Verify gateway deployment is running ---
echo "Verifying gateway deployment..."
kubectl wait --for=condition=available deployment/cfgd-server \
    -n cfgd-system --timeout=60s
echo "Gateway deployment is running"

# --- Create ephemeral namespace ---
create_e2e_namespace

# --- Port-forward to gateway ---
GW_PORT=18080
GW_HEALTH_PORT=18081
echo "Port-forwarding to gateway on localhost:$GW_PORT..."
PF_PID=$(port_forward cfgd-system cfgd-server "$GW_PORT" 8080)
PF_HEALTH_PID=$(port_forward cfgd-system cfgd-server "$GW_HEALTH_PORT" 8081)
GW_URL="http://localhost:$GW_PORT"

# Wait for gateway to be reachable via port-forward (use health endpoint — API requires auth)
wait_for_url "http://localhost:$GW_HEALTH_PORT/readyz" 30

echo "Gateway reachable at $GW_URL"

# --- Extract admin API key from deployment env var ---
ADMIN_KEY=$(kubectl get deployment cfgd-server -n cfgd-system \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CFGD_API_KEY")].value}' 2>/dev/null || echo "")

if [ -z "$ADMIN_KEY" ]; then
    # CFGD_API_KEY not set on deployment — gateway runs in open mode (all requests are admin)
    echo "WARN: CFGD_API_KEY not set on deployment, gateway in open mode"
    ADMIN_KEY=""
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
        # Open mode — no auth needed, but curl -H "" is harmless
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
GW_DEVICE_ID="e2e-device-${E2E_RUN_ID}"
# `|| true` because this file runs under `set -e` and a refused mint is a warning
# here: the cases that need a token check for one themselves.
BOOTSTRAP_TOKEN=$(gw_create_bootstrap_token "e2e-user" || true)

if [ -n "$BOOTSTRAP_TOKEN" ]; then
    echo "Bootstrap token created"
else
    echo "WARN: Failed to create bootstrap token"
fi

# --- Export environment for domain test files ---
export GW_URL GW_PORT GW_HEALTH_PORT PF_PID PF_HEALTH_PID ADMIN_KEY BOOTSTRAP_TOKEN GW_DEVICE_ID

echo "Gateway URL: $GW_URL"
echo "Bootstrap token available: $([ -n "$BOOTSTRAP_TOKEN" ] && echo yes || echo no)"
echo "Admin key available: $([ -n "$ADMIN_KEY" ] && echo yes || echo no)"
echo "Device ID: $GW_DEVICE_ID"
