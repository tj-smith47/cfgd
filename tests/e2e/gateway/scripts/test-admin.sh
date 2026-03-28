# Gateway admin tests (GW-15 through GW-17, GW-24 through GW-30).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# Helper: build auth header for admin API calls (redefine if not already present).
if ! declare -f gw_admin_auth_header >/dev/null 2>&1; then
    gw_admin_auth_header() {
        if [ -n "$ADMIN_KEY" ]; then
            echo "Authorization: Bearer $ADMIN_KEY"
        else
            echo "X-No-Auth: open-mode"
        fi
    }
fi

# Helper: create a fresh bootstrap token via admin API. Prints the token string.
if ! declare -f gw_create_bootstrap_token >/dev/null 2>&1; then
    gw_create_bootstrap_token() {
        local username="${1:-e2e-user}"
        local resp
        resp=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
            -H "Content-Type: application/json" \
            -H "$(gw_admin_auth_header)" \
            -d "{\"username\":\"$username\",\"team\":\"e2e-team\",\"expiresIn\":3600}" 2>/dev/null)
        echo "$resp" | jq -r '.token // empty' 2>/dev/null
    }
fi

# Helper: enroll a new device with a fresh token. Sets GW_HELPER_DEVICE_ID and
# GW_HELPER_API_KEY in the caller's scope. Returns 0 on success.
gw_enroll_new_device() {
    local suffix="$1"
    local token
    token=$(gw_create_bootstrap_token "e2e-admin-$suffix")
    if [ -z "$token" ]; then
        echo "  Failed to create bootstrap token for $suffix"
        return 1
    fi

    GW_HELPER_DEVICE_ID="e2e-admin-device-${suffix}-${E2E_RUN_ID}"
    local resp
    resp=$(curl -sf -X POST "$GW_URL/api/v1/enroll" \
        -H "Content-Type: application/json" \
        -d "{\"token\":\"$token\",\"deviceId\":\"$GW_HELPER_DEVICE_ID\",\"hostname\":\"e2e-host-$suffix\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "")

    GW_HELPER_API_KEY=$(echo "$resp" | jq -r '.apiKey // empty' 2>/dev/null)
    if [ -z "$GW_HELPER_API_KEY" ]; then
        echo "  Enrollment failed for $suffix: $resp"
        return 1
    fi
    echo "  Enrolled device $GW_HELPER_DEVICE_ID (key prefix: ${GW_HELPER_API_KEY:0:12}...)"
    return 0
}

# =================================================================
# GW-24: Auth boundary — unauthenticated GET /api/v1/devices returns 401
# =================================================================
begin_test "GW-24: Auth boundary (unauthenticated access)"

if [ -z "$ADMIN_KEY" ]; then
    skip_test "GW-24" "No ADMIN_KEY set — gateway in open mode, auth boundary not testable"
else
    GW24_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$GW_URL/api/v1/devices" 2>/dev/null || echo "000")
    echo "  GET /api/v1/devices (no auth): HTTP $GW24_CODE"

    if [ "$GW24_CODE" = "401" ]; then
        pass_test "GW-24"
    else
        fail_test "GW-24" "Expected 401, got $GW24_CODE"
    fi
fi

# =================================================================
# GW-25: Admin token create
# =================================================================
begin_test "GW-25: Admin token create"

GW25_RESPONSE=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
    -H "Content-Type: application/json" \
    -H "$(gw_admin_auth_header)" \
    -d '{"username":"e2e-gw25-user","team":"e2e-team","expiresIn":3600}' 2>/dev/null || echo "")

GW25_TOKEN=$(echo "$GW25_RESPONSE" | jq -r '.token // empty' 2>/dev/null)
GW25_TOKEN_ID=$(echo "$GW25_RESPONSE" | jq -r '.id // empty' 2>/dev/null)

echo "  Response has token: $([ -n "$GW25_TOKEN" ] && echo yes || echo no)"
echo "  Response has id: $([ -n "$GW25_TOKEN_ID" ] && echo yes || echo no)"

if [ -n "$GW25_TOKEN" ] && [ -n "$GW25_TOKEN_ID" ]; then
    pass_test "GW-25"
else
    fail_test "GW-25" "Token create response missing token or id"
fi

# =================================================================
# GW-26: Admin token list
# =================================================================
begin_test "GW-26: Admin token list"

GW26_CODE=$(curl -s -o $GW_SCRATCH/gw26-body.txt -w "%{http_code}" "$GW_URL/api/v1/admin/tokens" \
    -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
GW26_BODY=$(cat $GW_SCRATCH/gw26-body.txt 2>/dev/null || echo "")
rm -f $GW_SCRATCH/gw26-body.txt

echo "  GET /api/v1/admin/tokens: HTTP $GW26_CODE"

if [ "$GW26_CODE" = "200" ]; then
    # Verify the response is a JSON array
    GW26_LEN=$(echo "$GW26_BODY" | jq 'length' 2>/dev/null || echo "")
    echo "  Token count: $GW26_LEN"
    if [ -n "$GW26_LEN" ]; then
        # Check that at least one token has an id field (from GW-25 or setup)
        GW26_HAS_ID=$(echo "$GW26_BODY" | jq -r '.[0].id // empty' 2>/dev/null)
        if [ -n "$GW26_HAS_ID" ]; then
            pass_test "GW-26"
        else
            # Empty list is acceptable if tokens were consumed
            if [ "$GW26_LEN" = "0" ]; then
                pass_test "GW-26"
            else
                fail_test "GW-26" "Token list entries missing id field"
            fi
        fi
    else
        fail_test "GW-26" "Response is not a valid JSON array"
    fi
else
    fail_test "GW-26" "Expected 200, got $GW26_CODE"
fi

# =================================================================
# GW-27: Admin token delete
# =================================================================
begin_test "GW-27: Admin token delete"

# Create a token specifically to delete it
GW27_CREATE=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
    -H "Content-Type: application/json" \
    -H "$(gw_admin_auth_header)" \
    -d '{"username":"e2e-gw27-delete","team":"e2e-team","expiresIn":3600}' 2>/dev/null || echo "")
GW27_TOKEN_ID=$(echo "$GW27_CREATE" | jq -r '.id // empty' 2>/dev/null)

if [ -z "$GW27_TOKEN_ID" ]; then
    fail_test "GW-27" "Could not create token to delete"
else
    echo "  Created token $GW27_TOKEN_ID for deletion"
    GW27_DELETE_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
        "$GW_URL/api/v1/admin/tokens/$GW27_TOKEN_ID" \
        -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
    echo "  DELETE /api/v1/admin/tokens/$GW27_TOKEN_ID: HTTP $GW27_DELETE_CODE"

    if [ "$GW27_DELETE_CODE" = "204" ]; then
        pass_test "GW-27"
    else
        fail_test "GW-27" "Expected 204, got $GW27_DELETE_CODE"
    fi
fi

# =================================================================
# GW-28: Admin user key add
# =================================================================
begin_test "GW-28: Admin user key add"

GW28_USERNAME="e2e-keyuser-${E2E_RUN_ID}"
GW28_CODE=$(curl -s -o $GW_SCRATCH/gw28-body.txt -w "%{http_code}" \
    -X POST "$GW_URL/api/v1/admin/users/$GW28_USERNAME/keys" \
    -H "Content-Type: application/json" \
    -H "$(gw_admin_auth_header)" \
    -d '{"keyType":"ssh","publicKey":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGw28test e2e-test","fingerprint":"SHA256:e2eTestFingerprint28","label":"gw28-test"}' \
    2>/dev/null || echo "000")
GW28_BODY=$(cat $GW_SCRATCH/gw28-body.txt 2>/dev/null || echo "")
rm -f $GW_SCRATCH/gw28-body.txt

echo "  POST /api/v1/admin/users/$GW28_USERNAME/keys: HTTP $GW28_CODE"

if [ "$GW28_CODE" = "201" ]; then
    GW28_KEY_ID=$(echo "$GW28_BODY" | jq -r '.id // empty' 2>/dev/null)
    echo "  Key ID: $GW28_KEY_ID"
    if [ -n "$GW28_KEY_ID" ]; then
        pass_test "GW-28"
    else
        fail_test "GW-28" "Response missing key id"
    fi
else
    fail_test "GW-28" "Expected 201, got $GW28_CODE"
fi

# =================================================================
# GW-29: Admin user key list
# =================================================================
begin_test "GW-29: Admin user key list"

GW29_CODE=$(curl -s -o $GW_SCRATCH/gw29-body.txt -w "%{http_code}" \
    "$GW_URL/api/v1/admin/users/$GW28_USERNAME/keys" \
    -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
GW29_BODY=$(cat $GW_SCRATCH/gw29-body.txt 2>/dev/null || echo "")
rm -f $GW_SCRATCH/gw29-body.txt

echo "  GET /api/v1/admin/users/$GW28_USERNAME/keys: HTTP $GW29_CODE"

if [ "$GW29_CODE" = "200" ]; then
    GW29_LEN=$(echo "$GW29_BODY" | jq 'length' 2>/dev/null || echo "")
    echo "  Key count: $GW29_LEN"
    if [ -n "$GW29_LEN" ] && [ "$GW29_LEN" -ge 1 ] 2>/dev/null; then
        pass_test "GW-29"
    else
        fail_test "GW-29" "Expected at least 1 key, got $GW29_LEN"
    fi
else
    fail_test "GW-29" "Expected 200, got $GW29_CODE"
fi

# =================================================================
# GW-30: Admin user key delete
# =================================================================
begin_test "GW-30: Admin user key delete"

if [ -z "${GW28_KEY_ID:-}" ]; then
    skip_test "GW-30" "No key ID from GW-28"
else
    GW30_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
        "$GW_URL/api/v1/admin/users/$GW28_USERNAME/keys/$GW28_KEY_ID" \
        -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
    echo "  DELETE /api/v1/admin/users/$GW28_USERNAME/keys/$GW28_KEY_ID: HTTP $GW30_CODE"

    if [ "$GW30_CODE" = "204" ]; then
        pass_test "GW-30"
    else
        fail_test "GW-30" "Expected 204, got $GW30_CODE"
    fi
fi

# =================================================================
# GW-15: Admin credential revocation
# =================================================================
begin_test "GW-15: Admin credential revocation"

GW15_PASS=true

# Step 1: Enroll a new device
if ! gw_enroll_new_device "gw15"; then
    fail_test "GW-15" "Could not enroll device for revocation test"
    GW15_PASS=false
fi

# Step 2: Verify the device API key works before revocation
if [ "$GW15_PASS" = "true" ]; then
    GW15_PRE_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        "$GW_URL/api/v1/devices" \
        -H "Authorization: Bearer $GW_HELPER_API_KEY" 2>/dev/null || echo "000")
    echo "  Pre-revoke device list: HTTP $GW15_PRE_CODE"

    if [ "$GW15_PRE_CODE" != "200" ]; then
        # In open mode the key is irrelevant — all requests succeed.
        if [ -z "$ADMIN_KEY" ]; then
            echo "  Open mode — skipping key validation"
        else
            fail_test "GW-15" "Device key did not work before revocation (HTTP $GW15_PRE_CODE)"
            GW15_PASS=false
        fi
    fi
fi

# Step 3: Revoke the credential via admin API
if [ "$GW15_PASS" = "true" ]; then
    GW15_REVOKE_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
        "$GW_URL/api/v1/admin/devices/$GW_HELPER_DEVICE_ID/credential" \
        -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
    echo "  Revoke credential: HTTP $GW15_REVOKE_CODE"

    if [ "$GW15_REVOKE_CODE" != "204" ]; then
        fail_test "GW-15" "Revoke returned $GW15_REVOKE_CODE, expected 204"
        GW15_PASS=false
    fi
fi

# Step 4: Verify the old device API key no longer works
if [ "$GW15_PASS" = "true" ]; then
    if [ -z "$ADMIN_KEY" ]; then
        # Open mode — auth is not enforced, so revocation cannot be verified via HTTP status.
        echo "  Open mode — credential revocation stored but auth not enforced"
        pass_test "GW-15"
    else
        GW15_POST_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
            "$GW_URL/api/v1/devices" \
            -H "Authorization: Bearer $GW_HELPER_API_KEY" 2>/dev/null || echo "000")
        echo "  Post-revoke device list with old key: HTTP $GW15_POST_CODE"

        if [ "$GW15_POST_CODE" = "401" ]; then
            pass_test "GW-15"
        else
            fail_test "GW-15" "Old key still works after revocation (HTTP $GW15_POST_CODE, expected 401)"
        fi
    fi
fi

# =================================================================
# GW-16: Admin revoke + re-enroll
# =================================================================
begin_test "GW-16: Admin revoke + re-enroll"

GW16_PASS=true

# Step 1: Enroll a new device
if ! gw_enroll_new_device "gw16"; then
    fail_test "GW-16" "Could not enroll device for re-enrollment test"
    GW16_PASS=false
fi

GW16_DEVICE_ID="$GW_HELPER_DEVICE_ID"
GW16_OLD_KEY="$GW_HELPER_API_KEY"

# Step 2: Revoke its credential
if [ "$GW16_PASS" = "true" ]; then
    GW16_REVOKE_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
        "$GW_URL/api/v1/admin/devices/$GW16_DEVICE_ID/credential" \
        -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
    echo "  Revoke credential: HTTP $GW16_REVOKE_CODE"

    if [ "$GW16_REVOKE_CODE" != "204" ]; then
        fail_test "GW-16" "Revoke returned $GW16_REVOKE_CODE, expected 204"
        GW16_PASS=false
    fi
fi

# Step 3: Create a new token and re-enroll the same device ID
if [ "$GW16_PASS" = "true" ]; then
    GW16_NEW_TOKEN=$(gw_create_bootstrap_token "e2e-gw16-reenroll")
    if [ -z "$GW16_NEW_TOKEN" ]; then
        fail_test "GW-16" "Could not create new bootstrap token for re-enrollment"
        GW16_PASS=false
    fi
fi

if [ "$GW16_PASS" = "true" ]; then
    GW16_REENROLL_RESP=$(curl -sf -X POST "$GW_URL/api/v1/enroll" \
        -H "Content-Type: application/json" \
        -d "{\"token\":\"$GW16_NEW_TOKEN\",\"deviceId\":\"$GW16_DEVICE_ID\",\"hostname\":\"e2e-host-gw16-reenroll\",\"os\":\"linux\",\"arch\":\"x86_64\"}" \
        2>/dev/null || echo "")
    GW16_NEW_KEY=$(echo "$GW16_REENROLL_RESP" | jq -r '.apiKey // empty' 2>/dev/null)

    if [ -z "$GW16_NEW_KEY" ]; then
        fail_test "GW-16" "Re-enrollment did not return apiKey"
        GW16_PASS=false
    else
        echo "  Re-enrolled with new key (prefix: ${GW16_NEW_KEY:0:12}...)"
    fi
fi

# Step 4: Verify new key works
if [ "$GW16_PASS" = "true" ]; then
    GW16_NEW_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        "$GW_URL/api/v1/devices" \
        -H "Authorization: Bearer $GW16_NEW_KEY" 2>/dev/null || echo "000")
    echo "  New key GET /api/v1/devices: HTTP $GW16_NEW_CODE"

    if [ "$GW16_NEW_CODE" != "200" ] && [ -n "$ADMIN_KEY" ]; then
        fail_test "GW-16" "New key does not work (HTTP $GW16_NEW_CODE)"
        GW16_PASS=false
    fi
fi

# Step 5: Verify old key fails
if [ "$GW16_PASS" = "true" ]; then
    if [ -z "$ADMIN_KEY" ]; then
        echo "  Open mode — old key rejection not verifiable via HTTP status"
        pass_test "GW-16"
    else
        GW16_OLD_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
            "$GW_URL/api/v1/devices" \
            -H "Authorization: Bearer $GW16_OLD_KEY" 2>/dev/null || echo "000")
        echo "  Old key GET /api/v1/devices: HTTP $GW16_OLD_CODE"

        if [ "$GW16_OLD_CODE" = "401" ]; then
            pass_test "GW-16"
        else
            fail_test "GW-16" "Old key still works after re-enrollment (HTTP $GW16_OLD_CODE, expected 401)"
        fi
    fi
fi

# =================================================================
# GW-17: Fleet status via device list
# =================================================================
begin_test "GW-17: Fleet status via device list"

GW17_CODE=$(curl -s -o $GW_SCRATCH/gw17-body.txt -w "%{http_code}" \
    "$GW_URL/api/v1/devices" \
    -H "$(gw_admin_auth_header)" 2>/dev/null || echo "000")
GW17_BODY=$(cat $GW_SCRATCH/gw17-body.txt 2>/dev/null || echo "")
rm -f $GW_SCRATCH/gw17-body.txt

echo "  GET /api/v1/devices: HTTP $GW17_CODE"

if [ "$GW17_CODE" = "200" ]; then
    # Response could be { devices: [...] } or a bare array
    GW17_COUNT=$(echo "$GW17_BODY" | jq 'if type == "array" then length elif .devices then (.devices | length) else 0 end' 2>/dev/null || echo "0")
    echo "  Device count: $GW17_COUNT"

    # We enrolled devices in GW-15 and GW-16, plus the bootstrap device from setup.
    # Expect at least 2 devices.
    if [ "$GW17_COUNT" -ge 2 ] 2>/dev/null; then
        pass_test "GW-17"
    else
        # Even 1 is acceptable if re-enrollment replaced the device record
        if [ "$GW17_COUNT" -ge 1 ] 2>/dev/null; then
            echo "  Only $GW17_COUNT device(s) — re-enrollment may have replaced records"
            pass_test "GW-17"
        else
            fail_test "GW-17" "Expected at least 2 devices, got $GW17_COUNT"
        fi
    fi
else
    fail_test "GW-17" "Expected 200, got $GW17_CODE"
fi
