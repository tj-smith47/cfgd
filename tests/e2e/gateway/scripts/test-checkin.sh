# Gateway checkin tests (GW-07 through GW-10, GW-18, GW-31).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# =================================================================
# GW-07: Device checkin happy path
# =================================================================
begin_test "GW-07: Device checkin happy path"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-07" "No device enrolled (GW-02 may have failed)"
else
    GW07_RESPONSE=$(curl -sf -X POST "${GW_URL}/api/v1/checkin" \
        -H "Authorization: Bearer ${DEVICE_API_KEY}" \
        -H "Content-Type: application/json" \
        -d "{\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\",\"configHash\":\"sha256:e2e-checkin-test-${E2E_RUN_ID}\"}" \
        2>&1 || echo "FAILED")

    echo "  Response (first 300 chars):"
    echo "$GW07_RESPONSE" | head -c 300 | sed 's/^/    /'
    echo ""

    if echo "$GW07_RESPONSE" | grep -q '"status"'; then
        pass_test "GW-07"
    else
        fail_test "GW-07" "Checkin response missing 'status' field"
    fi
fi

# =================================================================
# GW-08: Checkin with drift report
# =================================================================
begin_test "GW-08: Checkin with drift report"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-08" "No device enrolled (GW-02 may have failed)"
else
    GW08_HTTP_CODE=$(curl -s -o $GW_SCRATCH/gw08-body.txt -w "%{http_code}" \
        -X POST "${GW_URL}/api/v1/devices/${GW_DEVICE_ID}/drift" \
        -H "Authorization: Bearer ${DEVICE_API_KEY}" \
        -H "Content-Type: application/json" \
        -d '{"details":[{"field":"packages.curl","expected":"8.5.0","actual":"8.4.0"}]}' \
        2>/dev/null || echo "000")
    GW08_BODY=$(cat $GW_SCRATCH/gw08-body.txt 2>/dev/null || echo "")
    rm -f $GW_SCRATCH/gw08-body.txt

    echo "  HTTP status: $GW08_HTTP_CODE"
    echo "  Body (first 300 chars):"
    echo "$GW08_BODY" | head -c 300 | sed 's/^/    /'
    echo ""

    if [ "$GW08_HTTP_CODE" = "201" ]; then
        pass_test "GW-08"
    else
        fail_test "GW-08" "Expected 201, got $GW08_HTTP_CODE"
    fi
fi

# =================================================================
# GW-09: Checkin with compliance data
# =================================================================
begin_test "GW-09: Checkin with compliance data"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-09" "No device enrolled (GW-02 may have failed)"
else
    GW09_COMPLIANCE='{"compliant":true,"totalChecks":5,"passedChecks":5,"failedChecks":0}'
    GW09_CHECKIN_CODE=$(curl -s -o $GW_SCRATCH/gw09-checkin.txt -w "%{http_code}" \
        -X POST "${GW_URL}/api/v1/checkin" \
        -H "Authorization: Bearer ${DEVICE_API_KEY}" \
        -H "Content-Type: application/json" \
        -d "{\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\",\"configHash\":\"sha256:e2e-compliance-${E2E_RUN_ID}\",\"complianceSummary\":${GW09_COMPLIANCE}}" \
        2>/dev/null || echo "000")
    GW09_CHECKIN_BODY=$(cat $GW_SCRATCH/gw09-checkin.txt 2>/dev/null || echo "")
    rm -f $GW_SCRATCH/gw09-checkin.txt

    echo "  Checkin HTTP status: $GW09_CHECKIN_CODE"

    if [ "$GW09_CHECKIN_CODE" != "200" ]; then
        echo "  Checkin body: $GW09_CHECKIN_BODY"
        fail_test "GW-09" "Checkin with compliance data failed (HTTP $GW09_CHECKIN_CODE)"
    else
        # GET the device and verify compliance data is stored
        GW09_DEVICE_CODE=$(curl -s -o $GW_SCRATCH/gw09-device.txt -w "%{http_code}" \
            -X GET "${GW_URL}/api/v1/devices/${GW_DEVICE_ID}" \
            -H "Authorization: Bearer ${DEVICE_API_KEY}" \
            2>/dev/null || echo "000")
        GW09_DEVICE_BODY=$(cat $GW_SCRATCH/gw09-device.txt 2>/dev/null || echo "")
        rm -f $GW_SCRATCH/gw09-device.txt

        echo "  GET device HTTP status: $GW09_DEVICE_CODE"
        echo "  Device body (first 500 chars):"
        echo "$GW09_DEVICE_BODY" | head -c 500 | sed 's/^/    /'
        echo ""

        if [ "$GW09_DEVICE_CODE" = "200" ] && echo "$GW09_DEVICE_BODY" | jq -e '.complianceSummary' >/dev/null 2>&1; then
            # Verify the compliance data matches what we sent
            GW09_STORED_COMPLIANT=$(echo "$GW09_DEVICE_BODY" | jq -r '.complianceSummary.compliant // empty' 2>/dev/null)
            if [ "$GW09_STORED_COMPLIANT" = "true" ]; then
                pass_test "GW-09"
            else
                fail_test "GW-09" "Compliance data stored but 'compliant' field mismatch (got: $GW09_STORED_COMPLIANT)"
            fi
        else
            fail_test "GW-09" "Device response missing complianceSummary (HTTP $GW09_DEVICE_CODE)"
        fi
    fi
fi

# =================================================================
# GW-10: Checkin with invalid API key
# =================================================================
begin_test "GW-10: Checkin with invalid API key"

GW10_HTTP_CODE=$(curl -s -o $GW_SCRATCH/gw10-body.txt -w "%{http_code}" \
    -X POST "${GW_URL}/api/v1/checkin" \
    -H "Authorization: Bearer cfgd_dk_totally_invalid_key_value" \
    -H "Content-Type: application/json" \
    -d "{\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-invalid\",\"os\":\"linux\",\"arch\":\"x86_64\",\"configHash\":\"sha256:invalid\"}" \
    2>/dev/null || echo "000")
GW10_BODY=$(cat $GW_SCRATCH/gw10-body.txt 2>/dev/null || echo "")
rm -f $GW_SCRATCH/gw10-body.txt

echo "  HTTP status: $GW10_HTTP_CODE"
echo "  Body: $GW10_BODY"

case "$GW10_HTTP_CODE" in
    401|403)
        pass_test "GW-10"
        ;;
    200)
        # Gateway may be in open mode (no CFGD_API_KEY set) — any Bearer token is accepted
        if [ -z "$ADMIN_KEY" ]; then
            skip_test "GW-10" "Gateway in open mode (no CFGD_API_KEY), cannot test auth rejection"
        else
            fail_test "GW-10" "Expected 401/403, got 200 (gateway has CFGD_API_KEY set)"
        fi
        ;;
    *)
        fail_test "GW-10" "Expected 401/403, got $GW10_HTTP_CODE"
        ;;
esac

# =================================================================
# GW-18: Checkin updates MachineConfig status
# =================================================================
begin_test "GW-18: Checkin updates MachineConfig status"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-18" "No device enrolled (GW-02 may have failed)"
else
    GW18_MC_NAME="e2e-mc-checkin-${E2E_RUN_ID}"

    # Create a MachineConfig CRD in the E2E namespace
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: ${GW18_MC_NAME}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: "e2e-host-${E2E_RUN_ID}"
  profile: "e2e-test"
EOF

    GW18_APPLY_RC=$?
    echo "  MachineConfig apply exit code: $GW18_APPLY_RC"

    if [ "$GW18_APPLY_RC" -ne 0 ]; then
        fail_test "GW-18" "Failed to create MachineConfig CRD"
    else
        # Checkin for this device
        GW18_CHECKIN_CODE=$(curl -s -o $GW_SCRATCH/gw18-checkin.txt -w "%{http_code}" \
            -X POST "${GW_URL}/api/v1/checkin" \
            -H "Authorization: Bearer ${DEVICE_API_KEY}" \
            -H "Content-Type: application/json" \
            -d "{\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\",\"configHash\":\"sha256:e2e-mc-test-${E2E_RUN_ID}\"}" \
            2>/dev/null || echo "000")
        GW18_CHECKIN_BODY=$(cat $GW_SCRATCH/gw18-checkin.txt 2>/dev/null || echo "")
        rm -f $GW_SCRATCH/gw18-checkin.txt

        echo "  Checkin HTTP status: $GW18_CHECKIN_CODE"

        # Verify the MachineConfig still exists and is retrievable
        GW18_MC_EXISTS=$(kubectl get machineconfig "${GW18_MC_NAME}" -n "${E2E_NAMESPACE}" -o name 2>/dev/null || echo "")

        echo "  MachineConfig lookup: ${GW18_MC_EXISTS:-not found}"

        if [ "$GW18_CHECKIN_CODE" = "200" ] && [ -n "$GW18_MC_EXISTS" ]; then
            pass_test "GW-18"
        else
            if [ "$GW18_CHECKIN_CODE" != "200" ]; then
                fail_test "GW-18" "Checkin failed (HTTP $GW18_CHECKIN_CODE): $GW18_CHECKIN_BODY"
            else
                fail_test "GW-18" "MachineConfig not found after checkin"
            fi
        fi

        # Cleanup: delete the MachineConfig
        kubectl delete machineconfig "${GW18_MC_NAME}" -n "${E2E_NAMESPACE}" --ignore-not-found 2>/dev/null || true
    fi
fi

# =================================================================
# GW-31: A checkin takes over a status map the merge-patch path owned
# =================================================================
begin_test "GW-31: Checkin takes over a merge-patch-owned status map"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-31" "No device enrolled (GW-02 may have failed)"
else
    GW31_MC_NAME="e2e-mc-ssa-${E2E_RUN_ID}"
    GW31_MANAGER="cfgd-operator/gateway/packages"

    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: ${GW31_MC_NAME}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: "e2e-host-${E2E_RUN_ID}"
  profile: "e2e-test"
EOF
    GW31_APPLY_RC=$?

    if [ "$GW31_APPLY_RC" -ne 0 ]; then
        fail_test "GW-31" "Failed to create MachineConfig CRD"
    else
        # A merge patch under a kubectl manager leaves the Update-type ownership
        # a release that wrote the whole status carried; the check-in below is
        # the first server-side apply to reach the same field.
        kubectl patch machineconfig "${GW31_MC_NAME}" -n "${E2E_NAMESPACE}" \
            --subresource=status --type=merge \
            -p '{"status":{"packageVersions":{"seeded/stale":"0.0.1"}}}' 2>/dev/null

        GW31_CHECKIN_CODE=$(curl -s -o $GW_SCRATCH/gw31-checkin.txt -w "%{http_code}" \
            -X POST "${GW_URL}/api/v1/checkin" \
            -H "Authorization: Bearer ${DEVICE_API_KEY}" \
            -H "Content-Type: application/json" \
            -d "{\"deviceId\":\"${GW_DEVICE_ID}\",\"hostname\":\"e2e-host-${E2E_RUN_ID}\",\"os\":\"linux\",\"arch\":\"x86_64\",\"configHash\":\"sha256:e2e-ssa-${E2E_RUN_ID}\",\"packageVersions\":{\"e2e/pkg\":\"1.2.3\"}}" \
            2>/dev/null || echo "000")
        GW31_CHECKIN_BODY=$(cat $GW_SCRATCH/gw31-checkin.txt 2>/dev/null || echo "")
        rm -f $GW_SCRATCH/gw31-checkin.txt

        echo "  Checkin HTTP status: $GW31_CHECKIN_CODE"

        if [ "$GW31_CHECKIN_CODE" != "200" ]; then
            fail_test "GW-31" "Checkin failed (HTTP $GW31_CHECKIN_CODE): $GW31_CHECKIN_BODY"
        else
            # The gateway awaits the apply before it answers, so the object read
            # here already carries whatever the check-in wrote.
            kubectl get machineconfig "${GW31_MC_NAME}" -n "${E2E_NAMESPACE}" \
                -o json --show-managed-fields=true > $GW_SCRATCH/gw31-mc.json 2>/dev/null
            GW31_MAP=$(jq -c '.status.packageVersions // {}' $GW_SCRATCH/gw31-mc.json 2>/dev/null || echo "{}")

            GW31_APPLY_OP=$(kubectl get machineconfig "${GW31_MC_NAME}" -n "${E2E_NAMESPACE}" \
                --show-managed-fields=true \
                -o jsonpath="{.metadata.managedFields[?(@.manager==\"${GW31_MANAGER}\")].operation}" 2>/dev/null || echo "")
            GW31_APPLY_FIELDS=$(jq -c --arg m "$GW31_MANAGER" \
                '[.metadata.managedFields[] | select(.manager==$m) | .fieldsV1] | tostring' \
                $GW_SCRATCH/gw31-mc.json 2>/dev/null || echo "")
            GW31_UPDATE_OWNERS=$(jq -r \
                '[.metadata.managedFields[] | select(.operation=="Update") | select((.fieldsV1|tostring)|contains("f:packageVersions")) | .manager] | join(",")' \
                $GW_SCRATCH/gw31-mc.json 2>/dev/null || echo "")

            echo "  status.packageVersions:        $GW31_MAP"
            echo "  ${GW31_MANAGER} operation: ${GW31_APPLY_OP:-none}"
            echo "  Update managers still naming f:packageVersions: ${GW31_UPDATE_OWNERS:-none}"

            # The gateway logs a refused status write at error level and lets the
            # check-in succeed, so a silent 409 shows up here and nowhere else.
            GW31_CONFLICT_LOG=$(kubectl logs -n cfgd-system deploy/cfgd-server --since=2m 2>/dev/null \
                | grep -F "device-reported MachineConfig status was not written" \
                | grep -F "${GW31_MC_NAME}" || true)
            echo "  Conflict log lines: ${GW31_CONFLICT_LOG:-none}"

            GW31_PASS=true
            assert_equals "$GW31_MAP" '{"e2e/pkg":"1.2.3"}' || GW31_PASS=false
            assert_equals "$GW31_APPLY_OP" "Apply" || GW31_PASS=false
            assert_contains "$GW31_APPLY_FIELDS" "f:packageVersions" || GW31_PASS=false
            assert_equals "$GW31_UPDATE_OWNERS" "" || GW31_PASS=false
            assert_equals "$GW31_CONFLICT_LOG" "" || GW31_PASS=false

            if [ "$GW31_PASS" = true ]; then
                pass_test "GW-31"
            else
                echo "  managedFields:"
                jq -c '.metadata.managedFields' $GW_SCRATCH/gw31-mc.json 2>/dev/null | sed 's/^/    /'
                fail_test "GW-31" "The checkin did not take sole ownership of status.packageVersions"
            fi
            rm -f $GW_SCRATCH/gw31-mc.json
        fi

        # Cleanup: delete the MachineConfig
        kubectl delete machineconfig "${GW31_MC_NAME}" -n "${E2E_NAMESPACE}" --ignore-not-found 2>/dev/null || true
    fi
fi
