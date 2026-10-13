# shellcheck shell=bash
# Gateway health probe tests (GW-01).
# Sourced by run-all.sh: no shebang, no set, no source, no traps, no print_summary.

# setup-gateway-env.sh forwards GW_HEALTH_PORT (18081) to the probe port of
# the operator pod serving the gateway.
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
    fail_test "GW-01" "pod/$GW_LEADER_POD answered /healthz $HEALTHZ_CODE and /readyz $READYZ_CODE, want 200 from both"
fi
