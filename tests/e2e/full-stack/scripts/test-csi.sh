# shellcheck shell=bash
# Full-stack E2E tests: CSI
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== CSI Tests ==="

# The node FS-CSI-01's pod ran on, whose driver has the module cached.
CSI01_NODE=""

# =================================================================
# FS-CSI-01: CSI driver: deploy DaemonSet, mount module content, verify
# =================================================================
begin_test "FS-CSI-01: CSI driver: module mount and content verification"

if ! wait_for_daemonset "$E2E_INSTALL_NS" "$E2E_CSI_DS" 60; then
    fail_test "FS-CSI-01" "CSI DaemonSet not ready"
else
    # Push a test module to the registry (from host)
    TEST_MODULE_DIR=$(mktemp -d)
    create_test_module_dir "$TEST_MODULE_DIR" "csi-test-mod-${E2E_RUN_ID}" "1.0.0"
    OCI_REF="${REGISTRY}/cfgd-e2e/csi-test:v1.0-${E2E_RUN_ID}"
    PUSH_OK=true
    "$CFGD_BIN" module push "$TEST_MODULE_DIR" --artifact "$OCI_REF" --no-color 2>&1 || PUSH_OK=false
    rm -rf "$TEST_MODULE_DIR"

    if [ "$PUSH_OK" = "false" ]; then
        fail_test "FS-CSI-01" "Failed to push test module to registry"
    else
    # Create Module CRD with OCI ref (keyless signature satisfies webhook policy)
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-test-mod-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    # Create an injection-enabled namespace
    ensure_namespace "e2e-csi-test-${E2E_RUN_ID}"
    ensure_label namespace "e2e-csi-test-${E2E_RUN_ID}" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "e2e-csi-test-${E2E_RUN_ID}" "csi-test-mod-${E2E_RUN_ID}:v1.0" || true

    # Create a pod with module annotation
    kubectl apply -n "e2e-csi-test-${E2E_RUN_ID}" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-mount-test
  annotations:
    cfgd.io/modules: "csi-test-mod-${E2E_RUN_ID}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    # Wait for pod to be running (CSI driver needs to pull and mount)
    echo "  Waiting for pod to be running..."
    POD_RUNNING=false
    wait_for_k8s_field pod csi-mount-test "e2e-csi-test-${E2E_RUN_ID}" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true
    CSI01_NODE=$(kubectl get pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" \
        -o jsonpath='{.spec.nodeName}' 2>/dev/null || echo "")

    if $POD_RUNNING; then
        # Verify module content is mounted
        MODULE_FILE=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
            cat "/cfgd-modules/csi-test-mod-${E2E_RUN_ID}/module.yaml" 2>/dev/null || echo "")
        HELLO_SH=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
            cat "/cfgd-modules/csi-test-mod-${E2E_RUN_ID}/bin/hello.sh" 2>/dev/null || echo "")

        echo "  module.yaml present: $([ -n "$MODULE_FILE" ] && echo 'yes' || echo 'no')"
        echo "  bin/hello.sh present: $([ -n "$HELLO_SH" ] && echo 'yes' || echo 'no')"

        # Verify read-only mount
        RO_TEST=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
            touch "/cfgd-modules/csi-test-mod-${E2E_RUN_ID}/test-write" 2>&1 || echo "read-only")

        if [ -n "$MODULE_FILE" ] && echo "$RO_TEST" | grep -qi "read-only"; then
            pass_test "FS-CSI-01"
        elif [ -n "$MODULE_FILE" ]; then
            pass_test "FS-CSI-01"
        else
            fail_test "FS-CSI-01" "Module content not found at mount path"
        fi
    else
        fail_test "FS-CSI-01" "Pod did not reach Running state (CSI mount may have failed)"
        kubectl describe pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null | tail -20
    fi
    fi  # PUSH_OK
fi

# =================================================================
# FS-CSI-02: CSI driver: unmount on pod delete
# =================================================================
begin_test "FS-CSI-02: CSI driver: unmount on pod delete"

# Delete the pod
kubectl delete pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" --grace-period=5 --ignore-not-found 2>/dev/null || true

# Wait for pod to be deleted
echo "  Waiting for pod deletion..."
wait_for_deleted 30 pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" || true

# Verify no mount leftovers via the test pod (which has host access)
CSI_MOUNTS=$(exec_in_pod mount 2>/dev/null | grep "cfgd" | grep "csi-mount-test" || echo "")
if [ -z "$CSI_MOUNTS" ]; then
    pass_test "FS-CSI-02"
else
    fail_test "FS-CSI-02" "CSI mount still present after pod deletion"
    echo "  Remaining mounts: $CSI_MOUNTS"
fi

# =================================================================
# FS-CSI-03: Multi-module volume mount
# =================================================================
begin_test "FS-CSI-03: CSI driver: multi-module volume mount"

# Push two distinct test modules
MOD_A_DIR=$(mktemp -d)
create_test_module_dir "$MOD_A_DIR" "csi-multi-a-${E2E_RUN_ID}" "1.0.0"
OCI_REF_A="${REGISTRY}/cfgd-e2e/csi-multi-a:v1.0-${E2E_RUN_ID}"
PUSH_A_OK=true
"$CFGD_BIN" module push "$MOD_A_DIR" --artifact "$OCI_REF_A" --no-color 2>&1 || PUSH_A_OK=false
rm -rf "$MOD_A_DIR"

MOD_B_DIR=$(mktemp -d)
create_test_module_dir "$MOD_B_DIR" "csi-multi-b-${E2E_RUN_ID}" "1.0.0"
OCI_REF_B="${REGISTRY}/cfgd-e2e/csi-multi-b:v1.0-${E2E_RUN_ID}"
PUSH_B_OK=true
"$CFGD_BIN" module push "$MOD_B_DIR" --artifact "$OCI_REF_B" --no-color 2>&1 || PUSH_B_OK=false
rm -rf "$MOD_B_DIR"

if [ "$PUSH_A_OK" = "false" ] || [ "$PUSH_B_OK" = "false" ]; then
    fail_test "FS-CSI-03" "Failed to push one or both test modules"
else
    # Create Module CRDs
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-multi-a-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI_REF_A}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-multi-b-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI_REF_B}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    # Create injection-enabled namespace
    CSI03_NS="e2e-csi-multi-${E2E_RUN_ID}"
    ensure_namespace "$CSI03_NS"
    ensure_label namespace "$CSI03_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$CSI03_NS" "csi-multi-a-${E2E_RUN_ID}:v1.0,csi-multi-b-${E2E_RUN_ID}:v1.0" || true

    # Create pod referencing both modules
    kubectl apply -n "$CSI03_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-multi-test
  annotations:
    cfgd.io/modules: "csi-multi-a-${E2E_RUN_ID}:v1.0,csi-multi-b-${E2E_RUN_ID}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for multi-module pod..."
    POD_RUNNING=false
    wait_for_k8s_field pod csi-multi-test "$CSI03_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        MOD_A_FILE=$(kubectl exec csi-multi-test -n "$CSI03_NS" -- \
            cat "/cfgd-modules/csi-multi-a-${E2E_RUN_ID}/module.yaml" 2>/dev/null || echo "")
        MOD_B_FILE=$(kubectl exec csi-multi-test -n "$CSI03_NS" -- \
            cat "/cfgd-modules/csi-multi-b-${E2E_RUN_ID}/module.yaml" 2>/dev/null || echo "")

        echo "  Module A mounted: $([ -n "$MOD_A_FILE" ] && echo 'yes' || echo 'no')"
        echo "  Module B mounted: $([ -n "$MOD_B_FILE" ] && echo 'yes' || echo 'no')"

        if [ -n "$MOD_A_FILE" ] && [ -n "$MOD_B_FILE" ]; then
            pass_test "FS-CSI-03"
        else
            fail_test "FS-CSI-03" "One or both module volumes not mounted"
        fi
    else
        fail_test "FS-CSI-03" "Pod did not reach Running state"
        kubectl describe pod csi-multi-test -n "$CSI03_NS" 2>/dev/null | tail -20
    fi

    # Cleanup
    kubectl delete namespace "$CSI03_NS" --ignore-not-found --wait=false 2>/dev/null || true
fi

# =================================================================
# FS-CSI-04: Module cache hit
# =================================================================
begin_test "FS-CSI-04: CSI driver: module cache hit"

# Print the cache-hit count for label set $CSI04_LABELS scraped off CSI
# driver pod $1, keeping the response in $2 for a fail reason; returns 1
# without a count when the port-forward did not open or the scrape did not
# answer 2xx. No sample for the label set yet counts as 0.
csi04_hits() {
    local pod="$1" body="$2" pid code content_type
    printf '000 \n' > "$body.meta"
    : > "$body"
    if ! pid=$(port_forward "$E2E_INSTALL_NS" "pod/$pod" 19094 9090); then
        echo "no-tunnel" > "$body.meta"
        return 1
    fi
    read -r code content_type <<<"$(http_get_to_file "http://localhost:19094/metrics" "$body")"
    stop_port_forward "$pid"
    printf '%s %s\n' "$code" "${content_type:-}" > "$body.meta"
    [[ "$code" == 2* ]] || return 1
    metric_sample_value cfgd_csi_cache_hits "$CSI04_LABELS" "$body"
}

# Describe what csi04_hits kept in $1 for scraping pod $2.
csi04_evidence() {
    local code content_type
    read -r code content_type < "$1.meta"
    if [ "$code" = no-tunnel ]; then
        echo "port_forward to pod/$2 did not open (kubectl output above)"
    else
        http_evidence "$code" "${content_type:-}" "$1"
    fi
}

if [ -z "$CSI01_NODE" ]; then
    fail_test "FS-CSI-04" "FS-CSI-01 recorded no node for its pod, so there is no warm cache to mount from"
else
    # A second mount of FS-CSI-01's module on the node that already pulled it
    # must be served from that node's cache, so its driver counts one more hit.
    CSI04_NS="e2e-csi-cache-${E2E_RUN_ID}"
    CSI04_LABELS="module=\"csi-test-mod-${E2E_RUN_ID}\""
    CSI04_BEFORE_BODY="$CLI_SCRATCH/fs-csi-04-before.txt"
    CSI04_AFTER_BODY="$CLI_SCRATCH/fs-csi-04-after.txt"
    ensure_namespace "$CSI04_NS"
    ensure_label namespace "$CSI04_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$CSI04_NS" "csi-test-mod-${E2E_RUN_ID}:v1.0" || true

    CSI04_DRIVER=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" \
        --field-selector "spec.nodeName=$CSI01_NODE" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
    echo "  Node: $CSI01_NODE, CSI driver pod: ${CSI04_DRIVER:-<none>}"

    if [ -z "$CSI04_DRIVER" ]; then
        fail_test "FS-CSI-04" "No CSI driver pod on node $CSI01_NODE"
    elif ! CSI04_HITS_BEFORE=$(csi04_hits "$CSI04_DRIVER" "$CSI04_BEFORE_BODY"); then
        fail_test "FS-CSI-04" "Scrape of pod/$CSI04_DRIVER before the mount failed: $(csi04_evidence "$CSI04_BEFORE_BODY" "$CSI04_DRIVER")"
    else
        kubectl apply -n "$CSI04_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-cache-test
  annotations:
    cfgd.io/modules: "csi-test-mod-${E2E_RUN_ID}:v1.0"
spec:
  nodeName: "${CSI01_NODE}"
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

        echo "  Waiting for cache-test pod..."
        POD_RUNNING=false
        wait_for_k8s_field pod csi-cache-test "$CSI04_NS" \
            '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

        if ! $POD_RUNNING; then
            fail_test "FS-CSI-04" "Pod did not reach Running state"
            kubectl describe pod csi-cache-test -n "$CSI04_NS" 2>/dev/null | tail -20
        elif ! CSI04_HITS_AFTER=$(csi04_hits "$CSI04_DRIVER" "$CSI04_AFTER_BODY"); then
            fail_test "FS-CSI-04" "Scrape of pod/$CSI04_DRIVER after the mount failed: $(csi04_evidence "$CSI04_AFTER_BODY" "$CSI04_DRIVER")"
        else
            echo "  cache hits for csi-test-mod-${E2E_RUN_ID}: before $CSI04_HITS_BEFORE, after $CSI04_HITS_AFTER"
            # prometheus-client renders no line at all for a family with no
            # sample, so a body without this module's sample says only that
            # the driver counted no hit for it.
            if metric_sample_lines cfgd_csi_cache_hits "$CSI04_AFTER_BODY" \
                | grep -qF "{$CSI04_LABELS}"; then
                if [ "$CSI04_HITS_AFTER" -gt "$CSI04_HITS_BEFORE" ]; then
                    pass_test "FS-CSI-04"
                else
                    fail_test "FS-CSI-04" "Cache hits did not rise (before $CSI04_HITS_BEFORE, after $CSI04_HITS_AFTER). Before: $(csi04_evidence "$CSI04_BEFORE_BODY" "$CSI04_DRIVER") After: $(csi04_evidence "$CSI04_AFTER_BODY" "$CSI04_DRIVER")"
                fi
            else
                fail_test "FS-CSI-04" "driver under test counted no cache hit for the second mount: $(csi04_evidence "$CSI04_AFTER_BODY" "$CSI04_DRIVER")"
            fi
        fi
    fi

    # Cleanup
    kubectl delete namespace "$CSI04_NS" --ignore-not-found --wait=false 2>/dev/null || true
fi

# =================================================================
# FS-CSI-05: Invalid module ref: the pod runs uninjected and the skip is logged
# =================================================================
begin_test "FS-CSI-05: CSI driver: invalid module ref runs uninjected"

CSI05_NS="e2e-csi-invalid-${E2E_RUN_ID}"
CSI05_SINCE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
ensure_namespace "$CSI05_NS"
CSI05_LABELLED=true
ensure_label namespace "$CSI05_NS" cfgd.io/inject-modules=true --overwrite || CSI05_LABELLED=false

# The injector admits a pod naming a module it cannot resolve unpatched and
# logs the skip, so the pod runs with no CSI volume. Running alone cannot tell
# that skip from a webhook that never saw the pod; the operator's log line can.
csi05_skip_logged() {
    kubectl logs -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" --since-time="$CSI05_SINCE" --tail=-1 2>/dev/null |
        grep 'module CRD not found, skipping injection' |
        grep -qE "module=\"?nonexistent-module-${E2E_RUN_ID}\"?([^A-Za-z0-9_-]|$)" # rc-ok: a failed read leaves grep empty, so the function answers no
}

if ! $CSI05_LABELLED; then
    fail_test "FS-CSI-05" "Namespace $CSI05_NS could not be labelled for injection"
elif ! wait_for_injection "$CSI05_NS" "csi-test-mod-${E2E_RUN_ID}:v1.0"; then
    # The probe uses the FS-CSI-01 module, which resolves: until it injects,
    # the API server's namespace cache lacks the label and the pod below would
    # never reach the webhook.
    fail_test "FS-CSI-05" "The injector never served namespace $CSI05_NS"
else
    CSI05_APPLY_RC=0
    kubectl apply -n "$CSI05_NS" -f - <<EOF || CSI05_APPLY_RC=$?
apiVersion: v1
kind: Pod
metadata:
  name: csi-invalid-test
  annotations:
    cfgd.io/modules: "nonexistent-module-${E2E_RUN_ID}:v9.9"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF
    if [ "$CSI05_APPLY_RC" -ne 0 ]; then
        fail_test "FS-CSI-05" "kubectl apply of pod/csi-invalid-test failed (rc=$CSI05_APPLY_RC)"
    else
        POD_PHASE=$(wait_for_k8s_field pod csi-invalid-test "$CSI05_NS" '{.status.phase}' Running 60) || true
        CSI05_VOLS=$(kubectl get pod csi-invalid-test -n "$CSI05_NS" \
            -o jsonpath='{.spec.volumes[?(@.csi.driver=="'"$CSI_DRIVER_NAME"'")].name}' 2>&1) || CSI05_VOLS="read failed: $CSI05_VOLS"
        CSI05_SKIPPED=false
        wait_until 30 2 "the operator to log it skipped nonexistent-module-${E2E_RUN_ID}" csi05_skip_logged && CSI05_SKIPPED=true
        echo "  Pod phase: ${POD_PHASE:-<none>}; $CSI_DRIVER_NAME volumes: ${CSI05_VOLS:-<none>}; skip logged: $CSI05_SKIPPED"
        if [ "$POD_PHASE" != "Running" ]; then
            fail_test "FS-CSI-05" "Expected the uninjected pod Running, phase is ${POD_PHASE:-<none>}"
        elif [ -n "$CSI05_VOLS" ]; then
            fail_test "FS-CSI-05" "The pod carries $CSI_DRIVER_NAME volumes for a module that does not exist: $CSI05_VOLS"
        elif ! $CSI05_SKIPPED; then
            fail_test "FS-CSI-05" "The operator logged no 'module CRD not found, skipping injection' for nonexistent-module-${E2E_RUN_ID} since $CSI05_SINCE"
        else
            pass_test "FS-CSI-05"
        fi
    fi
fi

# Cleanup
kubectl delete namespace "$CSI05_NS" --ignore-not-found --wait=false 2>/dev/null || true

# =================================================================
# FS-CSI-06: Module update propagation
# =================================================================
begin_test "FS-CSI-06: CSI driver: module update propagation"

# Push v2 of a module with different content
MOD_V2_DIR=$(mktemp -d)
create_test_module_dir "$MOD_V2_DIR" "csi-update-mod-${E2E_RUN_ID}" "2.0.0"
# Add a distinctive v2 marker file
echo "version-2-content" > "$MOD_V2_DIR/v2-marker.txt"
OCI_REF_V2="${REGISTRY}/cfgd-e2e/csi-update:v2.0-${E2E_RUN_ID}"
PUSH_V2_OK=true
"$CFGD_BIN" module push "$MOD_V2_DIR" --artifact "$OCI_REF_V2" --no-color 2>&1 || PUSH_V2_OK=false
rm -rf "$MOD_V2_DIR"

if [ "$PUSH_V2_OK" = "false" ]; then
    fail_test "FS-CSI-06" "Failed to push v2 module"
else
    # Create (or update) Module CRD pointing to v2
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-update-mod-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI_REF_V2}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    CSI06_NS="e2e-csi-update-${E2E_RUN_ID}"
    ensure_namespace "$CSI06_NS"
    ensure_label namespace "$CSI06_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$CSI06_NS" "csi-update-mod-${E2E_RUN_ID}:v2.0" || true

    # Create pod referencing the updated module
    kubectl apply -n "$CSI06_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-update-test
  annotations:
    cfgd.io/modules: "csi-update-mod-${E2E_RUN_ID}:v2.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for update-test pod..."
    POD_RUNNING=false
    wait_for_k8s_field pod csi-update-test "$CSI06_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        # Verify the v2 marker file is present
        V2_CONTENT=$(kubectl exec csi-update-test -n "$CSI06_NS" -- \
            cat "/cfgd-modules/csi-update-mod-${E2E_RUN_ID}/v2-marker.txt" 2>/dev/null || echo "")
        # Also verify module.yaml reflects v2
        MOD_YAML=$(kubectl exec csi-update-test -n "$CSI06_NS" -- \
            cat "/cfgd-modules/csi-update-mod-${E2E_RUN_ID}/module.yaml" 2>/dev/null || echo "")

        echo "  v2-marker.txt: ${V2_CONTENT:-<not found>}"
        echo "  module.yaml present: $([ -n "$MOD_YAML" ] && echo 'yes' || echo 'no')"

        if [ "$V2_CONTENT" = "version-2-content" ]; then
            pass_test "FS-CSI-06"
        elif [ -n "$MOD_YAML" ] && echo "$MOD_YAML" | grep -q "2.0.0"; then
            pass_test "FS-CSI-06"
        else
            fail_test "FS-CSI-06" "Updated module content not found in mount"
        fi
    else
        fail_test "FS-CSI-06" "Pod did not reach Running state"
        kubectl describe pod csi-update-test -n "$CSI06_NS" 2>/dev/null | tail -20
    fi

    # Cleanup
    kubectl delete namespace "$CSI06_NS" --ignore-not-found --wait=false 2>/dev/null || true
fi

# =================================================================
# FS-CSI-07: CSI driver metrics
# =================================================================
begin_test "FS-CSI-07: CSI driver: /metrics returns cfgd_csi_volume_publish_total"

CSI07_POD=""
[ -z "$CSI01_NODE" ] || CSI07_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" \
    --field-selector "spec.nodeName=$CSI01_NODE" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
CSI07_LABELS="module=\"csi-test-mod-${E2E_RUN_ID}\",result=\"success\""

if [ -z "$CSI01_NODE" ]; then
    fail_test "FS-CSI-07" "FS-CSI-01 recorded no node for its pod, so no driver is known to have published"
elif [ -z "$CSI07_POD" ]; then
    fail_test "FS-CSI-07" "No CSI driver pod on node $CSI01_NODE"
else
    # CSI container is distroless (no wget/curl). Port-forward to scrape metrics.
    CSI07_PORT=19090
    CSI07_BODY="$CLI_SCRATCH/fs-csi-07-metrics.txt"
    echo "  Node: $CSI01_NODE, CSI driver pod: $CSI07_POD"
    if CSI07_PF_PID=$(port_forward "$E2E_INSTALL_NS" "pod/$CSI07_POD" "$CSI07_PORT" 9090); then
        read -r CSI07_CODE CSI07_CONTENT_TYPE \
            <<<"$(http_get_to_file "http://localhost:$CSI07_PORT/metrics" "$CSI07_BODY")"
        stop_port_forward "$CSI07_PF_PID"
        CSI07_EVIDENCE="$(http_evidence "$CSI07_CODE" "${CSI07_CONTENT_TYPE:-}" "$CSI07_BODY")"
        CSI07_PUBLISHES="$(metric_sample_value cfgd_csi_volume_publish "$CSI07_LABELS" "$CSI07_BODY")"
        echo "  Successful publishes of csi-test-mod-${E2E_RUN_ID}: $CSI07_PUBLISHES"

        if [[ "$CSI07_CODE" != 2* ]]; then
            fail_test "FS-CSI-07" "/metrics did not answer 2xx: $CSI07_EVIDENCE"
        elif [ "$CSI07_PUBLISHES" -ge 1 ]; then
            pass_test "FS-CSI-07"
        else
            fail_test "FS-CSI-07" "pod/$CSI07_POD, the driver on FS-CSI-01's node, reports no cfgd_csi_volume_publish sample {$CSI07_LABELS} of 1 or more: $CSI07_EVIDENCE"
        fi
    else
        fail_test "FS-CSI-07" "Port-forward to pod/$CSI07_POD never opened localhost:$CSI07_PORT (kubectl output above)"
    fi
fi

# =================================================================
# FS-CSI-08: CSI pod readiness
# =================================================================
begin_test "FS-CSI-08: CSI driver: DaemonSet pod Ready"

CSI_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")

if [ -z "$CSI_POD" ]; then
    fail_test "FS-CSI-08" "No CSI driver pod found"
else
    READY_STATUS=$(kubectl get pod "$CSI_POD" -n "$E2E_INSTALL_NS" \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
    echo "  CSI pod: $CSI_POD"
    echo "  Ready: $READY_STATUS"

    if [ "$READY_STATUS" = "True" ]; then
        pass_test "FS-CSI-08"
    else
        fail_test "FS-CSI-08" "CSI DaemonSet pod Ready condition is not True"
        kubectl describe pod "$CSI_POD" -n "$E2E_INSTALL_NS" 2>/dev/null | tail -15
    fi
fi

# =================================================================
# FS-CSI-09: Volume unmount cleanup
# =================================================================
begin_test "FS-CSI-09: CSI driver: volume unmount cleanup on pod delete"

CSI09_NS="e2e-csi-unmount-${E2E_RUN_ID}"
ensure_namespace "$CSI09_NS"
CSI09_LABELLED=true
ensure_label namespace "$CSI09_NS" cfgd.io/inject-modules=true --overwrite || CSI09_LABELLED=false

! $CSI09_LABELLED || wait_for_injection "$CSI09_NS" "csi-test-mod-${E2E_RUN_ID}:v1.0" || true

# Reuse module from FS-CSI-01
kubectl apply -n "$CSI09_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-unmount-test
  annotations:
    cfgd.io/modules: "csi-test-mod-${E2E_RUN_ID}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

echo "  Waiting for unmount-test pod..."
POD_RUNNING=false
wait_for_k8s_field pod csi-unmount-test "$CSI09_NS" \
    '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

if $POD_RUNNING; then
    # Verify mount exists before delete
    PRE_MOUNT=$(kubectl exec csi-unmount-test -n "$CSI09_NS" -- \
        cat "/cfgd-modules/csi-test-mod-${E2E_RUN_ID}/module.yaml" 2>/dev/null || echo "")
    echo "  Mount before delete: $([ -n "$PRE_MOUNT" ] && echo 'present' || echo 'absent')"

    # Delete the pod
    kubectl delete pod csi-unmount-test -n "$CSI09_NS" --grace-period=5 --ignore-not-found 2>/dev/null || true

    # Wait for pod to be gone
    echo "  Waiting for pod deletion..."
    wait_for_deleted 30 pod csi-unmount-test -n "$CSI09_NS" || true

    # Verify no mount leftovers
    CSI_MOUNTS=$(exec_in_pod mount 2>/dev/null | grep "cfgd" | grep "csi-unmount-test" || echo "")
    if ! $CSI09_LABELLED; then
        # No label, no injected volume, and "no mount left behind" is then
        # a fact about a pod that never had one.
        fail_test "FS-CSI-09" "Namespace $CSI09_NS could not be labelled for injection"
    elif [ -z "$CSI_MOUNTS" ]; then
        pass_test "FS-CSI-09"
    else
        fail_test "FS-CSI-09" "CSI mount still present after pod deletion"
        echo "  Remaining mounts: $CSI_MOUNTS"
    fi
else
    fail_test "FS-CSI-09" "Pod did not reach Running state"
    kubectl describe pod csi-unmount-test -n "$CSI09_NS" 2>/dev/null | tail -20
fi

# Cleanup
kubectl delete namespace "$CSI09_NS" --ignore-not-found --wait=false 2>/dev/null || true

# =================================================================
# FS-CSI-10: ReadOnly enforcement
# =================================================================
begin_test "FS-CSI-10: CSI driver: readOnly enforcement"

CSI10_NS="e2e-csi-ro-${E2E_RUN_ID}"
ensure_namespace "$CSI10_NS"
ensure_label namespace "$CSI10_NS" cfgd.io/inject-modules=true --overwrite

wait_for_injection "$CSI10_NS" "csi-test-mod-${E2E_RUN_ID}:v1.0" || true

# Reuse module from FS-CSI-01
kubectl apply -n "$CSI10_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-ro-test
  annotations:
    cfgd.io/modules: "csi-test-mod-${E2E_RUN_ID}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

echo "  Waiting for ro-test pod..."
POD_RUNNING=false
wait_for_k8s_field pod csi-ro-test "$CSI10_NS" \
    '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

if $POD_RUNNING; then
    # Attempt to write a file inside the mounted module directory
    WRITE_RESULT=$(kubectl exec csi-ro-test -n "$CSI10_NS" -- \
        sh -c "touch /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/write-test 2>&1" || echo "read-only")
    echo "  Write attempt result: $WRITE_RESULT"

    if echo "$WRITE_RESULT" | grep -qi "read.only\|permission denied\|not permitted"; then
        pass_test "FS-CSI-10"
    elif [ -n "$WRITE_RESULT" ] && ! kubectl exec csi-ro-test -n "$CSI10_NS" -- \
        test -f "/cfgd-modules/csi-test-mod-${E2E_RUN_ID}/write-test" 2>/dev/null; then
        # Write failed (file doesn't exist) even if error message differs
        pass_test "FS-CSI-10"
    else
        fail_test "FS-CSI-10" "Write to read-only mount did not fail as expected"
    fi
else
    fail_test "FS-CSI-10" "Pod did not reach Running state"
    kubectl describe pod csi-ro-test -n "$CSI10_NS" 2>/dev/null | tail -20
fi

# Cleanup
kubectl delete namespace "$CSI10_NS" --ignore-not-found --wait=false 2>/dev/null || true
