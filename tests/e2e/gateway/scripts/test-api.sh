# Gateway API & auth tests (GW-11 through GW-14, GW-19, GW-20).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# Helper: build auth header for device API calls.
gw_device_auth_header() {
    if [ -n "${DEVICE_API_KEY:-}" ]; then
        echo "Authorization: Bearer $DEVICE_API_KEY"
    else
        echo "X-No-Auth: open-mode"
    fi
}

# Re-use admin auth helper from test-enrollment.sh if available, otherwise define it.
if ! type gw_admin_auth_header >/dev/null 2>&1; then
    gw_admin_auth_header() {
        if [ -n "${ADMIN_KEY:-}" ]; then
            echo "Authorization: Bearer $ADMIN_KEY"
        else
            echo "X-No-Auth: open-mode"
        fi
    }
fi

# =================================================================
# GW-11: Device list API
# =================================================================
begin_test "GW-11: Device list API returns JSON array"

if [ -z "${DEVICE_API_KEY:-}" ] && [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-11" "No DEVICE_API_KEY or ADMIN_KEY available"
else
    # Prefer admin key for listing all devices; fall back to device key
    if [ -n "${ADMIN_KEY:-}" ]; then
        GW11_AUTH="$(gw_admin_auth_header)"
    else
        GW11_AUTH="$(gw_device_auth_header)"
    fi

    GW11_HTTP_CODE=$(curl -s -o /tmp/gw11-body.json -w "%{http_code}" \
        -H "$GW11_AUTH" \
        "$GW_URL/api/v1/devices" 2>/dev/null || echo "000")
    GW11_BODY=$(cat /tmp/gw11-body.json 2>/dev/null || echo "")
    rm -f /tmp/gw11-body.json

    echo "  HTTP status: $GW11_HTTP_CODE"
    echo "  Body (first 300 chars): $(echo "$GW11_BODY" | head -c 300)"

    if [ "$GW11_HTTP_CODE" != "200" ]; then
        fail_test "GW-11" "Expected HTTP 200, got $GW11_HTTP_CODE"
    else
        # Verify response is a JSON array
        GW11_TYPE=$(echo "$GW11_BODY" | jq -r 'type' 2>/dev/null || echo "")
        GW11_LENGTH=$(echo "$GW11_BODY" | jq 'length' 2>/dev/null || echo "0")

        if [ "$GW11_TYPE" != "array" ]; then
            fail_test "GW-11" "Expected JSON array, got type '$GW11_TYPE'"
        elif [ "$GW11_LENGTH" -lt 1 ] 2>/dev/null; then
            fail_test "GW-11" "Expected at least 1 device, got $GW11_LENGTH"
        else
            echo "  Devices in list: $GW11_LENGTH"
            pass_test "GW-11"
        fi
    fi
fi

# =================================================================
# GW-12: Device detail API
# =================================================================
begin_test "GW-12: Device detail API returns device with hostname"

if [ -z "${DEVICE_API_KEY:-}" ] && [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-12" "No DEVICE_API_KEY or ADMIN_KEY available"
elif [ -z "${GW_DEVICE_ID:-}" ]; then
    skip_test "GW-12" "No GW_DEVICE_ID available"
else
    if [ -n "${ADMIN_KEY:-}" ]; then
        GW12_AUTH="$(gw_admin_auth_header)"
    else
        GW12_AUTH="$(gw_device_auth_header)"
    fi

    GW12_HTTP_CODE=$(curl -s -o /tmp/gw12-body.json -w "%{http_code}" \
        -H "$GW12_AUTH" \
        "$GW_URL/api/v1/devices/${GW_DEVICE_ID}" 2>/dev/null || echo "000")
    GW12_BODY=$(cat /tmp/gw12-body.json 2>/dev/null || echo "")
    rm -f /tmp/gw12-body.json

    echo "  HTTP status: $GW12_HTTP_CODE"
    echo "  Body (first 300 chars): $(echo "$GW12_BODY" | head -c 300)"

    if [ "$GW12_HTTP_CODE" != "200" ]; then
        fail_test "GW-12" "Expected HTTP 200, got $GW12_HTTP_CODE"
    else
        # Verify response contains hostname field
        GW12_HOSTNAME=$(echo "$GW12_BODY" | jq -r '.hostname // empty' 2>/dev/null)

        if [ -z "$GW12_HOSTNAME" ]; then
            fail_test "GW-12" "Response missing hostname field"
        else
            echo "  Device hostname: $GW12_HOSTNAME"
            pass_test "GW-12"
        fi
    fi
fi

# =================================================================
# GW-13: Drift events API
# =================================================================
begin_test "GW-13: Drift events API returns array"

if [ -z "${DEVICE_API_KEY:-}" ] && [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-13" "No DEVICE_API_KEY or ADMIN_KEY available"
elif [ -z "${GW_DEVICE_ID:-}" ]; then
    skip_test "GW-13" "No GW_DEVICE_ID available"
else
    if [ -n "${ADMIN_KEY:-}" ]; then
        GW13_AUTH="$(gw_admin_auth_header)"
    else
        GW13_AUTH="$(gw_device_auth_header)"
    fi

    GW13_HTTP_CODE=$(curl -s -o /tmp/gw13-body.json -w "%{http_code}" \
        -H "$GW13_AUTH" \
        "$GW_URL/api/v1/devices/${GW_DEVICE_ID}/drift" 2>/dev/null || echo "000")
    GW13_BODY=$(cat /tmp/gw13-body.json 2>/dev/null || echo "")
    rm -f /tmp/gw13-body.json

    echo "  HTTP status: $GW13_HTTP_CODE"
    echo "  Body (first 300 chars): $(echo "$GW13_BODY" | head -c 300)"

    if [ "$GW13_HTTP_CODE" != "200" ]; then
        fail_test "GW-13" "Expected HTTP 200, got $GW13_HTTP_CODE"
    else
        GW13_TYPE=$(echo "$GW13_BODY" | jq -r 'type' 2>/dev/null || echo "")

        if [ "$GW13_TYPE" != "array" ]; then
            fail_test "GW-13" "Expected JSON array, got type '$GW13_TYPE'"
        else
            GW13_LENGTH=$(echo "$GW13_BODY" | jq 'length' 2>/dev/null || echo "0")
            echo "  Drift events: $GW13_LENGTH"
            pass_test "GW-13"
        fi
    fi
fi

# =================================================================
# GW-14: Fleet events API
# =================================================================
begin_test "GW-14: Fleet events API returns array"

if [ -z "${DEVICE_API_KEY:-}" ] && [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-14" "No DEVICE_API_KEY or ADMIN_KEY available"
else
    if [ -n "${ADMIN_KEY:-}" ]; then
        GW14_AUTH="$(gw_admin_auth_header)"
    else
        GW14_AUTH="$(gw_device_auth_header)"
    fi

    GW14_HTTP_CODE=$(curl -s -o /tmp/gw14-body.json -w "%{http_code}" \
        -H "$GW14_AUTH" \
        "$GW_URL/api/v1/events" 2>/dev/null || echo "000")
    GW14_BODY=$(cat /tmp/gw14-body.json 2>/dev/null || echo "")
    rm -f /tmp/gw14-body.json

    echo "  HTTP status: $GW14_HTTP_CODE"
    echo "  Body (first 300 chars): $(echo "$GW14_BODY" | head -c 300)"

    if [ "$GW14_HTTP_CODE" != "200" ]; then
        fail_test "GW-14" "Expected HTTP 200, got $GW14_HTTP_CODE"
    else
        GW14_TYPE=$(echo "$GW14_BODY" | jq -r 'type' 2>/dev/null || echo "")

        if [ "$GW14_TYPE" != "array" ]; then
            fail_test "GW-14" "Expected JSON array, got type '$GW14_TYPE'"
        else
            GW14_LENGTH=$(echo "$GW14_BODY" | jq 'length' 2>/dev/null || echo "0")
            echo "  Fleet events: $GW14_LENGTH"
            pass_test "GW-14"
        fi
    fi
fi

# =================================================================
# GW-19: Set device config
# =================================================================
begin_test "GW-19: Set device config via PUT"

if [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-19" "No ADMIN_KEY available (admin-only endpoint)"
elif [ -z "${GW_DEVICE_ID:-}" ]; then
    skip_test "GW-19" "No GW_DEVICE_ID available"
else
    GW19_HTTP_CODE=$(curl -s -o /tmp/gw19-body.txt -w "%{http_code}" \
        -X PUT "$GW_URL/api/v1/devices/${GW_DEVICE_ID}/config" \
        -H "Content-Type: application/json" \
        -H "$(gw_admin_auth_header)" \
        -d '{"config":{"packages":["curl","jq"],"env":{"E2E_TEST":"true"}}}' 2>/dev/null || echo "000")
    GW19_BODY=$(cat /tmp/gw19-body.txt 2>/dev/null || echo "")
    rm -f /tmp/gw19-body.txt

    echo "  HTTP status: $GW19_HTTP_CODE"
    if [ -n "$GW19_BODY" ]; then
        echo "  Body: $(echo "$GW19_BODY" | head -c 200)"
    fi

    case "$GW19_HTTP_CODE" in
        200|204)
            pass_test "GW-19"
            ;;
        *)
            fail_test "GW-19" "Expected HTTP 200 or 204, got $GW19_HTTP_CODE"
            ;;
    esac
fi

# =================================================================
# GW-20: Force reconcile
# =================================================================
begin_test "GW-20: Force reconcile via POST"

if [ -z "${ADMIN_KEY:-}" ]; then
    skip_test "GW-20" "No ADMIN_KEY available (admin-only endpoint)"
elif [ -z "${GW_DEVICE_ID:-}" ]; then
    skip_test "GW-20" "No GW_DEVICE_ID available"
else
    GW20_HTTP_CODE=$(curl -s -o /tmp/gw20-body.txt -w "%{http_code}" \
        -X POST "$GW_URL/api/v1/devices/${GW_DEVICE_ID}/reconcile" \
        -H "Content-Type: application/json" \
        -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
    GW20_BODY=$(cat /tmp/gw20-body.txt 2>/dev/null || echo "")
    rm -f /tmp/gw20-body.txt

    echo "  HTTP status: $GW20_HTTP_CODE"
    if [ -n "$GW20_BODY" ]; then
        echo "  Body: $(echo "$GW20_BODY" | head -c 200)"
    fi

    case "$GW20_HTTP_CODE" in
        200|204)
            pass_test "GW-20"
            ;;
        *)
            fail_test "GW-20" "Expected HTTP 200 or 204, got $GW20_HTTP_CODE"
            ;;
    esac
fi
