# shellcheck shell=bash
# Gateway health probe tests (GW-01).
# Sourced by run-all.sh: no shebang, no set, no source, no traps, no print_summary.

# setup-gateway-env.sh forwards GW_HEALTH_PORT (18081) to the probe port of
# the operator pod serving the gateway. That pod's /readyz says the operator
# serves its webhook and leaves the gateway out, so GW-01 also asks the gateway
# itself through GW_URL.
GW_HEALTH_URL="http://localhost:${GW_HEALTH_PORT:-18081}"

# =================================================================
# GW-01: Health and readiness probes and the gateway answer HTTP 200
# =================================================================
begin_test "GW-01: Health and readiness probes and the gateway"

GW01_PASS=true

HEALTHZ_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${GW_HEALTH_URL}/healthz" 2>/dev/null || echo "000")
READYZ_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${GW_HEALTH_URL}/readyz" 2>/dev/null || echo "000")
ENROLL_INFO_CODE=$(curl -s -o /dev/null -w "%{http_code}" "${GW_URL}/api/v1/enroll/info" 2>/dev/null || echo "000")

echo "  /healthz            -> $HEALTHZ_CODE"
echo "  /readyz             -> $READYZ_CODE"
echo "  /api/v1/enroll/info -> $ENROLL_INFO_CODE"

if [ "$HEALTHZ_CODE" != "200" ]; then
    echo "  /healthz returned $HEALTHZ_CODE, expected 200"
    GW01_PASS=false
fi
if [ "$READYZ_CODE" != "200" ]; then
    echo "  /readyz returned $READYZ_CODE, expected 200"
    GW01_PASS=false
fi
if [ "$ENROLL_INFO_CODE" != "200" ]; then
    echo "  the gateway's /api/v1/enroll/info returned $ENROLL_INFO_CODE, expected 200"
    GW01_PASS=false
fi

if [ "$GW01_PASS" = "true" ]; then
    pass_test "GW-01"
else
    fail_test "GW-01" "pod/$GW_LEADER_POD answered /healthz $HEALTHZ_CODE and /readyz $READYZ_CODE, and the gateway /api/v1/enroll/info $ENROLL_INFO_CODE, want 200 from all three"
fi
