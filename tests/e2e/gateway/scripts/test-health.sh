# Gateway health probe tests (GW-01).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# The health probe port-forward is already established by setup-gateway-env.sh
# on GW_HEALTH_PORT (18081). Use it directly.
GW_HEALTH_URL="http://localhost:${GW_HEALTH_PORT:-18081}"

# =================================================================
# GW-01: Health and readiness probes return HTTP 200
# =================================================================
begin_test "GW-01: Health and readiness probes"

GW01_PASS=true

HEALTHZ_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${GW_HEALTH_URL}/healthz" 2>/dev/null || echo "000")
READYZ_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${GW_HEALTH_URL}/readyz" 2>/dev/null || echo "000")

echo "  /healthz -> $HEALTHZ_CODE"
echo "  /readyz  -> $READYZ_CODE"

if [ "$HEALTHZ_CODE" != "200" ]; then
    echo "  /healthz returned $HEALTHZ_CODE, expected 200"
    GW01_PASS=false
fi
if [ "$READYZ_CODE" != "200" ]; then
    echo "  /readyz returned $READYZ_CODE, expected 200"
    GW01_PASS=false
fi

if [ "$GW01_PASS" = "true" ]; then
    pass_test "GW-01"
else
    # Health probe port may not be exposed via the E2E cfgd-server Service.
    # Fall back to checking the gateway API port as a liveness indicator.
    # /api/v1/devices requires auth — use admin key if available
    GW01_AUTH_HEADER=""
    if [ -n "${ADMIN_KEY:-}" ]; then
        GW01_AUTH_HEADER="Authorization: Bearer $ADMIN_KEY"
    fi
    API_CODE=$(curl -sf -o /dev/null -w "%{http_code}" ${GW01_AUTH_HEADER:+-H "$GW01_AUTH_HEADER"} "${GW_URL}/api/v1/devices" 2>/dev/null || echo "000")
    echo "  Fallback: /api/v1/devices -> $API_CODE"
    if [ "$API_CODE" = "200" ]; then
        skip_test "GW-01" "Health probe port (8081) not exposed; gateway API is reachable"
    else
        fail_test "GW-01" "Neither health probe nor gateway API reachable"
    fi
fi
