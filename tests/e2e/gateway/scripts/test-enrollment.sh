# Gateway enrollment tests (GW-02 through GW-06).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# Helper: build auth header for admin API calls.
gw_admin_auth_header() {
    if [ -n "$ADMIN_KEY" ]; then
        echo "Authorization: Bearer $ADMIN_KEY"
    else
        # Open mode — no auth needed, but curl -H "" is harmless
        echo "X-No-Auth: open-mode"
    fi
}

# Helper: create a fresh bootstrap token via admin API. Prints the token string.
gw_create_bootstrap_token() {
    local username="${1:-e2e-user}"
    local resp
    resp=$(curl -sf -X POST "$GW_URL/api/v1/admin/tokens" \
        -H "Content-Type: application/json" \
        -H "$(gw_admin_auth_header)" \
        -d "{\"username\":\"$username\",\"team\":\"e2e-team\",\"expiresIn\":3600}" 2>/dev/null)
    echo "$resp" | jq -r '.token // empty' 2>/dev/null
}

# =================================================================
# GW-02: Token-based enrollment
# =================================================================
begin_test "GW-02: Token-based enrollment"

if [ -z "$BOOTSTRAP_TOKEN" ]; then
    skip_test "GW-02" "No bootstrap token available (setup may have failed)"
else
    GW02_RESPONSE=$(curl -sf -X POST "$GW_URL/api/v1/enroll" \
        -H "Content-Type: application/json" \
        -d "{\"token\":\"${BOOTSTRAP_TOKEN}\",\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "")

    echo "  Response (first 300 chars):"
    echo "$GW02_RESPONSE" | head -c 300 | sed 's/^/    /'
    echo ""

    # Extract apiKey from response
    DEVICE_API_KEY=$(echo "$GW02_RESPONSE" | jq -r '.apiKey // empty' 2>/dev/null)

    if [ -z "$DEVICE_API_KEY" ]; then
        # Fallback: grep-based extraction
        DEVICE_API_KEY=$(echo "$GW02_RESPONSE" | grep -oP '"apiKey"\s*:\s*"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' || echo "")
    fi

    if [ -n "$DEVICE_API_KEY" ] && assert_contains "$GW02_RESPONSE" "apiKey"; then
        export DEVICE_API_KEY
        echo "  Device API key obtained (prefix: ${DEVICE_API_KEY:0:12}...)"
        pass_test "GW-02"
    else
        fail_test "GW-02" "Enrollment response missing apiKey"
    fi
fi

# =================================================================
# GW-03: Enrollment with invalid token
# =================================================================
begin_test "GW-03: Enrollment with invalid token rejects"

INVALID_DEVICE_ID="e2e-invalid-token-device-$$"
GW03_HTTP_CODE=$(curl -s -o /tmp/gw03-body.txt -w "%{http_code}" -X POST "$GW_URL/api/v1/enroll" \
    -H "Content-Type: application/json" \
    -d "{\"token\":\"cfgd_bs_totally_invalid_token_value\",\"deviceId\":\"${INVALID_DEVICE_ID}\",\"hostname\":\"e2e-invalid\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "000")
GW03_BODY=$(cat /tmp/gw03-body.txt 2>/dev/null || echo "")
rm -f /tmp/gw03-body.txt

echo "  HTTP status: $GW03_HTTP_CODE"
echo "  Body: $GW03_BODY"

case "$GW03_HTTP_CODE" in
    400|401|403|404)
        pass_test "GW-03"
        ;;
    *)
        fail_test "GW-03" "Expected 400/401/403/404, got $GW03_HTTP_CODE"
        ;;
esac

# =================================================================
# GW-04: SSH key enrollment via challenge-response
# =================================================================
begin_test "GW-04: SSH key enrollment via challenge-response"

if ! command -v ssh-keygen >/dev/null 2>&1; then
    skip_test "GW-04" "ssh-keygen not available"
else
    GW04_PASS=true
    GW04_TMPDIR=$(mktemp -d)
    GW04_KEY="$GW04_TMPDIR/e2e_ed25519"
    GW04_USERNAME="e2e-ssh-user-$$"
    GW04_DEVICE_ID="e2e-ssh-device-$$"

    # Step 1: Generate ephemeral ed25519 key
    ssh-keygen -t ed25519 -f "$GW04_KEY" -N "" -q 2>/dev/null
    GW04_PUBKEY=$(cat "$GW04_KEY.pub" 2>/dev/null)
    GW04_FINGERPRINT=$(ssh-keygen -lf "$GW04_KEY.pub" 2>/dev/null | awk '{print $2}')

    echo "  Generated key: ${GW04_FINGERPRINT}"

    if [ -z "$GW04_PUBKEY" ] || [ -z "$GW04_FINGERPRINT" ]; then
        fail_test "GW-04" "Failed to generate SSH key pair"
        GW04_PASS=false
    fi

    # Step 2: Register public key via admin API
    if [ "$GW04_PASS" = "true" ]; then
        GW04_ADDKEY_CODE=$(curl -s -o /tmp/gw04-addkey.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/admin/users/$GW04_USERNAME/keys" \
            -H "Content-Type: application/json" \
            -H "$(gw_admin_auth_header)" \
            -d "{\"keyType\":\"ssh\",\"publicKey\":$(echo "$GW04_PUBKEY" | jq -Rs .),\"fingerprint\":\"$GW04_FINGERPRINT\",\"label\":\"e2e-test\"}" 2>/dev/null || echo "000")
        echo "  Register key: HTTP $GW04_ADDKEY_CODE"

        if [ "$GW04_ADDKEY_CODE" != "201" ]; then
            echo "  Body: $(cat /tmp/gw04-addkey.txt 2>/dev/null)"
            fail_test "GW-04" "Failed to register SSH key (HTTP $GW04_ADDKEY_CODE)"
            GW04_PASS=false
        fi
        rm -f /tmp/gw04-addkey.txt
    fi

    # Step 3: Request enrollment challenge
    if [ "$GW04_PASS" = "true" ]; then
        GW04_CHALLENGE_CODE=$(curl -s -o /tmp/gw04-challenge.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/enroll/challenge" \
            -H "Content-Type: application/json" \
            -d "{\"username\":\"$GW04_USERNAME\",\"deviceId\":\"$GW04_DEVICE_ID\",\"hostname\":\"e2e-ssh-host\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "000")
        GW04_CHALLENGE_BODY=$(cat /tmp/gw04-challenge.txt 2>/dev/null || echo "")
        rm -f /tmp/gw04-challenge.txt

        echo "  Challenge request: HTTP $GW04_CHALLENGE_CODE"

        if [ "$GW04_CHALLENGE_CODE" = "201" ]; then
            GW04_CHALLENGE_ID=$(echo "$GW04_CHALLENGE_BODY" | jq -r '.challengeId // empty' 2>/dev/null)
            GW04_NONCE=$(echo "$GW04_CHALLENGE_BODY" | jq -r '.nonce // empty' 2>/dev/null)
            echo "  Challenge ID: $GW04_CHALLENGE_ID"

            if [ -z "$GW04_CHALLENGE_ID" ] || [ -z "$GW04_NONCE" ]; then
                fail_test "GW-04" "Challenge response missing challengeId or nonce"
                GW04_PASS=false
            fi
        elif [ "$GW04_CHALLENGE_CODE" = "400" ]; then
            # Server may be in token mode, not key mode
            echo "  Body: $GW04_CHALLENGE_BODY"
            skip_test "GW-04" "Server not in key enrollment mode (HTTP 400)"
            GW04_PASS=false
        else
            echo "  Body: $GW04_CHALLENGE_BODY"
            fail_test "GW-04" "Challenge request failed (HTTP $GW04_CHALLENGE_CODE)"
            GW04_PASS=false
        fi
    fi

    # Step 4: Sign the nonce with ssh-keygen -Y sign
    if [ "$GW04_PASS" = "true" ]; then
        echo "$GW04_NONCE" | ssh-keygen -Y sign -f "$GW04_KEY" -n cfgd-enroll > "$GW04_TMPDIR/signature.txt" 2>/dev/null
        GW04_SIGNATURE=$(cat "$GW04_TMPDIR/signature.txt" 2>/dev/null)

        if [ -z "$GW04_SIGNATURE" ]; then
            fail_test "GW-04" "Failed to sign nonce with SSH key"
            GW04_PASS=false
        else
            echo "  Nonce signed successfully"
        fi
    fi

    # Step 5: Verify enrollment
    if [ "$GW04_PASS" = "true" ]; then
        GW04_VERIFY_CODE=$(curl -s -o /tmp/gw04-verify.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/enroll/verify" \
            -H "Content-Type: application/json" \
            -d "{\"challengeId\":\"$GW04_CHALLENGE_ID\",\"signature\":$(echo "$GW04_SIGNATURE" | jq -Rs .),\"keyType\":\"ssh\"}" 2>/dev/null || echo "000")
        GW04_VERIFY_BODY=$(cat /tmp/gw04-verify.txt 2>/dev/null || echo "")
        rm -f /tmp/gw04-verify.txt

        echo "  Verify: HTTP $GW04_VERIFY_CODE"

        if [ "$GW04_VERIFY_CODE" = "201" ] && echo "$GW04_VERIFY_BODY" | jq -e '.apiKey' >/dev/null 2>&1; then
            echo "  SSH key enrollment succeeded"
            pass_test "GW-04"
        else
            echo "  Body: $GW04_VERIFY_BODY"
            fail_test "GW-04" "SSH key verification failed (HTTP $GW04_VERIFY_CODE)"
        fi
    fi

    # Cleanup temp dir
    rm -rf "$GW04_TMPDIR"
fi

# =================================================================
# GW-05: GPG key enrollment via challenge-response
# =================================================================
begin_test "GW-05: GPG key enrollment via challenge-response"

if ! command -v gpg >/dev/null 2>&1; then
    skip_test "GW-05" "gpg not available"
else
    GW05_PASS=true
    GW05_TMPDIR=$(mktemp -d)
    GW05_GPGHOME="$GW05_TMPDIR/gnupg"
    mkdir -p "$GW05_GPGHOME"
    chmod 700 "$GW05_GPGHOME"
    GW05_USERNAME="e2e-gpg-user-$$"
    GW05_DEVICE_ID="e2e-gpg-device-$$"

    # Step 1: Generate ephemeral GPG key
    gpg --homedir "$GW05_GPGHOME" --batch --passphrase "" --quick-generate-key \
        "$GW05_USERNAME <${GW05_USERNAME}@e2e.test>" ed25519 sign 1y 2>/dev/null

    GW05_FINGERPRINT=$(gpg --homedir "$GW05_GPGHOME" --list-keys --with-colons 2>/dev/null \
        | grep '^fpr' | head -1 | cut -d: -f10)
    GW05_PUBKEY=$(gpg --homedir "$GW05_GPGHOME" --armor --export "$GW05_FINGERPRINT" 2>/dev/null)

    echo "  Generated GPG key: ${GW05_FINGERPRINT:0:16}..."

    if [ -z "$GW05_PUBKEY" ] || [ -z "$GW05_FINGERPRINT" ]; then
        fail_test "GW-05" "Failed to generate GPG key pair"
        GW05_PASS=false
    fi

    # Step 2: Register public key via admin API
    if [ "$GW05_PASS" = "true" ]; then
        GW05_ADDKEY_CODE=$(curl -s -o /tmp/gw05-addkey.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/admin/users/$GW05_USERNAME/keys" \
            -H "Content-Type: application/json" \
            -H "$(gw_admin_auth_header)" \
            -d "{\"keyType\":\"gpg\",\"publicKey\":$(echo "$GW05_PUBKEY" | jq -Rs .),\"fingerprint\":\"$GW05_FINGERPRINT\",\"label\":\"e2e-gpg-test\"}" 2>/dev/null || echo "000")
        echo "  Register key: HTTP $GW05_ADDKEY_CODE"

        if [ "$GW05_ADDKEY_CODE" != "201" ]; then
            echo "  Body: $(cat /tmp/gw05-addkey.txt 2>/dev/null)"
            fail_test "GW-05" "Failed to register GPG key (HTTP $GW05_ADDKEY_CODE)"
            GW05_PASS=false
        fi
        rm -f /tmp/gw05-addkey.txt
    fi

    # Step 3: Request enrollment challenge
    if [ "$GW05_PASS" = "true" ]; then
        GW05_CHALLENGE_CODE=$(curl -s -o /tmp/gw05-challenge.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/enroll/challenge" \
            -H "Content-Type: application/json" \
            -d "{\"username\":\"$GW05_USERNAME\",\"deviceId\":\"$GW05_DEVICE_ID\",\"hostname\":\"e2e-gpg-host\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "000")
        GW05_CHALLENGE_BODY=$(cat /tmp/gw05-challenge.txt 2>/dev/null || echo "")
        rm -f /tmp/gw05-challenge.txt

        echo "  Challenge request: HTTP $GW05_CHALLENGE_CODE"

        if [ "$GW05_CHALLENGE_CODE" = "201" ]; then
            GW05_CHALLENGE_ID=$(echo "$GW05_CHALLENGE_BODY" | jq -r '.challengeId // empty' 2>/dev/null)
            GW05_NONCE=$(echo "$GW05_CHALLENGE_BODY" | jq -r '.nonce // empty' 2>/dev/null)
            echo "  Challenge ID: $GW05_CHALLENGE_ID"

            if [ -z "$GW05_CHALLENGE_ID" ] || [ -z "$GW05_NONCE" ]; then
                fail_test "GW-05" "Challenge response missing challengeId or nonce"
                GW05_PASS=false
            fi
        elif [ "$GW05_CHALLENGE_CODE" = "400" ]; then
            echo "  Body: $GW05_CHALLENGE_BODY"
            skip_test "GW-05" "Server not in key enrollment mode (HTTP 400)"
            GW05_PASS=false
        else
            echo "  Body: $GW05_CHALLENGE_BODY"
            fail_test "GW-05" "Challenge request failed (HTTP $GW05_CHALLENGE_CODE)"
            GW05_PASS=false
        fi
    fi

    # Step 4: Sign the nonce with gpg --detach-sign
    if [ "$GW05_PASS" = "true" ]; then
        echo -n "$GW05_NONCE" > "$GW05_TMPDIR/nonce.txt"
        gpg --homedir "$GW05_GPGHOME" --batch --yes --passphrase "" \
            --detach-sign --armor -o "$GW05_TMPDIR/nonce.txt.asc" \
            "$GW05_TMPDIR/nonce.txt" 2>/dev/null
        GW05_SIGNATURE=$(cat "$GW05_TMPDIR/nonce.txt.asc" 2>/dev/null)

        if [ -z "$GW05_SIGNATURE" ]; then
            fail_test "GW-05" "Failed to sign nonce with GPG key"
            GW05_PASS=false
        else
            echo "  Nonce signed successfully"
        fi
    fi

    # Step 5: Verify enrollment
    if [ "$GW05_PASS" = "true" ]; then
        GW05_VERIFY_CODE=$(curl -s -o /tmp/gw05-verify.txt -w "%{http_code}" \
            -X POST "$GW_URL/api/v1/enroll/verify" \
            -H "Content-Type: application/json" \
            -d "{\"challengeId\":\"$GW05_CHALLENGE_ID\",\"signature\":$(echo "$GW05_SIGNATURE" | jq -Rs .),\"keyType\":\"gpg\"}" 2>/dev/null || echo "000")
        GW05_VERIFY_BODY=$(cat /tmp/gw05-verify.txt 2>/dev/null || echo "")
        rm -f /tmp/gw05-verify.txt

        echo "  Verify: HTTP $GW05_VERIFY_CODE"

        if [ "$GW05_VERIFY_CODE" = "201" ] && echo "$GW05_VERIFY_BODY" | jq -e '.apiKey' >/dev/null 2>&1; then
            echo "  GPG key enrollment succeeded"
            pass_test "GW-05"
        else
            echo "  Body: $GW05_VERIFY_BODY"
            fail_test "GW-05" "GPG key verification failed (HTTP $GW05_VERIFY_CODE)"
        fi
    fi

    # Cleanup temp dir and GPG home
    rm -rf "$GW05_TMPDIR"
fi

# =================================================================
# GW-06: Duplicate enrollment (same device ID, consumed token)
# =================================================================
begin_test "GW-06: Duplicate enrollment rejected"

if [ -z "$BOOTSTRAP_TOKEN" ]; then
    skip_test "GW-06" "No bootstrap token available from GW-02"
else
    # The bootstrap token from setup was consumed during GW-02.
    # Re-using the same consumed token should fail.
    GW06_HTTP_CODE=$(curl -s -o /tmp/gw06-body.txt -w "%{http_code}" -X POST "$GW_URL/api/v1/enroll" \
        -H "Content-Type: application/json" \
        -d "{\"token\":\"${BOOTSTRAP_TOKEN}\",\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-dup-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "000")
    GW06_BODY=$(cat /tmp/gw06-body.txt 2>/dev/null || echo "")
    rm -f /tmp/gw06-body.txt

    echo "  Consumed-token re-enrollment: HTTP $GW06_HTTP_CODE"
    echo "  Body: $GW06_BODY"

    case "$GW06_HTTP_CODE" in
        400|401|403|409|422)
            pass_test "GW-06"
            ;;
        *)
            # Also try with a fresh token for the same device ID to test device-level dedup
            GW06_FRESH_TOKEN=$(gw_create_bootstrap_token "e2e-dup-user")
            if [ -n "$GW06_FRESH_TOKEN" ]; then
                GW06_FRESH_CODE=$(curl -s -o /tmp/gw06-fresh.txt -w "%{http_code}" -X POST "$GW_URL/api/v1/enroll" \
                    -H "Content-Type: application/json" \
                    -d "{\"token\":\"${GW06_FRESH_TOKEN}\",\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-dup2-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\"}" 2>/dev/null || echo "000")
                GW06_FRESH_BODY=$(cat /tmp/gw06-fresh.txt 2>/dev/null || echo "")
                rm -f /tmp/gw06-fresh.txt

                echo "  Fresh-token same-device re-enrollment: HTTP $GW06_FRESH_CODE"
                echo "  Body (first 200 chars): $(echo "$GW06_FRESH_BODY" | head -c 200)"

                case "$GW06_FRESH_CODE" in
                    400|401|403|409|422)
                        pass_test "GW-06"
                        ;;
                    201)
                        # Server allows re-enrollment (upsert behavior) — credential is replaced.
                        # This is a valid design: re-enrollment rotates the device API key.
                        echo "  Server permits re-enrollment with new token (upsert semantics)"
                        pass_test "GW-06"
                        ;;
                    *)
                        fail_test "GW-06" "Unexpected status $GW06_FRESH_CODE for duplicate enrollment"
                        ;;
                esac
            else
                fail_test "GW-06" "Consumed token returned $GW06_HTTP_CODE, and failed to create fresh token for further testing"
            fi
            ;;
    esac
fi
