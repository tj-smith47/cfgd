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
echo "Port-forwarding to gateway on localhost:$GW_PORT..."
PF_PID=$(port_forward cfgd-system cfgd-server "$GW_PORT" 8080)
GW_URL="http://localhost:$GW_PORT"

# Wait for gateway to be reachable via port-forward
wait_for_url "$GW_URL/api/v1/devices" 30

echo "Gateway reachable at $GW_URL"

# --- Extract admin API key from deployment env var ---
ADMIN_KEY=$(kubectl get deployment cfgd-server -n cfgd-system \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CFGD_API_KEY")].value}' 2>/dev/null || echo "")

if [ -z "$ADMIN_KEY" ]; then
    # CFGD_API_KEY not set on deployment — gateway runs in open mode (all requests are admin)
    echo "WARN: CFGD_API_KEY not set on deployment, gateway in open mode"
    ADMIN_KEY=""
fi

# --- Create bootstrap token for enrollment tests ---
BOOTSTRAP_TOKEN=""
GW_DEVICE_ID="e2e-device-${E2E_RUN_ID}"

if [ -n "$ADMIN_KEY" ]; then
    TOKEN_RESPONSE=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $ADMIN_KEY" \
        -d '{"username":"e2e-user","team":"e2e-team","expiresIn":3600}')
else
    # Open mode — no auth header needed
    TOKEN_RESPONSE=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
        -H "Content-Type: application/json" \
        -d '{"username":"e2e-user","team":"e2e-team","expiresIn":3600}')
fi

if [ -n "$TOKEN_RESPONSE" ]; then
    BOOTSTRAP_TOKEN=$(echo "$TOKEN_RESPONSE" | jq -r '.token // empty')
    if [ -n "$BOOTSTRAP_TOKEN" ]; then
        echo "Bootstrap token created"
    else
        echo "WARN: Failed to extract token from response: $TOKEN_RESPONSE"
    fi
else
    echo "WARN: Failed to create bootstrap token"
fi

# --- Export environment for domain test files ---
export GW_URL GW_PORT PF_PID ADMIN_KEY BOOTSTRAP_TOKEN GW_DEVICE_ID

echo "Gateway URL: $GW_URL"
echo "Bootstrap token available: $([ -n "$BOOTSTRAP_TOKEN" ] && echo yes || echo no)"
echo "Admin key available: $([ -n "$ADMIN_KEY" ] && echo yes || echo no)"
echo "Device ID: $GW_DEVICE_ID"
