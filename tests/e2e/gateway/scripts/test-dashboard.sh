# Gateway dashboard and enrollment info tests (GW-22, GW-23).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# =================================================================
# GW-22: Web dashboard loads
# =================================================================
begin_test "GW-22: Web dashboard loads"

# The web dashboard is behind web_auth_middleware, which checks for CFGD_API_KEY
# via query param (?key=...) or the same Bearer token.
if [ -n "$ADMIN_KEY" ]; then
    GW22_CODE=$(curl -s -o /tmp/gw22-body.txt -w "%{http_code}" \
        "$GW_URL/?key=$ADMIN_KEY" 2>/dev/null || echo "000")
else
    GW22_CODE=$(curl -s -o /tmp/gw22-body.txt -w "%{http_code}" \
        "$GW_URL/" 2>/dev/null || echo "000")
fi
GW22_BODY=$(cat /tmp/gw22-body.txt 2>/dev/null || echo "")
rm -f /tmp/gw22-body.txt

echo "  GET /: HTTP $GW22_CODE"

case "$GW22_CODE" in
    200)
        if echo "$GW22_BODY" | grep -qi '<html\|<!doctype'; then
            echo "  Response contains HTML"
            pass_test "GW-22"
        else
            echo "  Response (first 200 chars):"
            echo "$GW22_BODY" | head -c 200 | sed 's/^/    /'
            echo ""
            fail_test "GW-22" "Response is not HTML"
        fi
        ;;
    302|303|307)
        # Redirect is acceptable — the dashboard may redirect to a login page
        echo "  Got redirect ($GW22_CODE) — dashboard is served but requires auth flow"
        pass_test "GW-22"
        ;;
    401)
        # In auth mode without key param, web middleware may redirect or return 401
        # Try with Bearer header instead
        GW22_BEARER_CODE=$(curl -s -o /tmp/gw22-bearer.txt -w "%{http_code}" \
            "$GW_URL/" \
            -H "Authorization: Bearer $ADMIN_KEY" 2>/dev/null || echo "000")
        GW22_BEARER_BODY=$(cat /tmp/gw22-bearer.txt 2>/dev/null || echo "")
        rm -f /tmp/gw22-bearer.txt

        echo "  Retry with Bearer: HTTP $GW22_BEARER_CODE"

        if [ "$GW22_BEARER_CODE" = "200" ] && echo "$GW22_BEARER_BODY" | grep -qi '<html\|<!doctype'; then
            pass_test "GW-22"
        else
            fail_test "GW-22" "Dashboard not accessible via query param ($GW22_CODE) or Bearer ($GW22_BEARER_CODE)"
        fi
        ;;
    *)
        fail_test "GW-22" "Expected 200/302, got $GW22_CODE"
        ;;
esac

# =================================================================
# GW-23: Enrollment info
# =================================================================
begin_test "GW-23: Enrollment info"

GW23_CODE=$(curl -s -o /tmp/gw23-body.txt -w "%{http_code}" \
    "$GW_URL/api/v1/enroll/info" 2>/dev/null || echo "000")
GW23_BODY=$(cat /tmp/gw23-body.txt 2>/dev/null || echo "")
rm -f /tmp/gw23-body.txt

echo "  GET /api/v1/enroll/info: HTTP $GW23_CODE"
echo "  Body: $GW23_BODY"

if [ "$GW23_CODE" = "200" ]; then
    GW23_METHOD=$(echo "$GW23_BODY" | jq -r '.method // empty' 2>/dev/null)
    echo "  Enrollment method: $GW23_METHOD"

    if [ -n "$GW23_METHOD" ]; then
        pass_test "GW-23"
    else
        fail_test "GW-23" "Response missing method field"
    fi
else
    fail_test "GW-23" "Expected 200, got $GW23_CODE"
fi
