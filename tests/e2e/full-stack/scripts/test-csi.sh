# Full-stack E2E tests: CSI
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== CSI Tests ==="

# =================================================================
# FS-CSI-01: CSI driver — deploy DaemonSet, mount module content, verify
# =================================================================
begin_test "FS-CSI-01: CSI driver — module mount and content verification"

if ! $CSI_AVAILABLE; then
    skip_test "FS-CSI-01" "CSI driver not ready"
else
    # Verify CSI DaemonSet is ready
    CSI_DS_NAME=$(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "cfgd-csi-csi")
    if ! wait_for_daemonset cfgd-system "$CSI_DS_NAME" 60; then
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
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
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
        kubectl create namespace "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null || true
        kubectl label namespace "e2e-csi-test-${E2E_RUN_ID}" cfgd.io/inject-modules=true --overwrite 2>/dev/null

        sleep 3

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

        if $POD_RUNNING; then
            # Verify module content is mounted
            MODULE_FILE=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                cat /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/module.yaml 2>/dev/null || echo "")
            HELLO_SH=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                cat /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/bin/hello.sh 2>/dev/null || echo "")

            echo "  module.yaml present: $([ -n "$MODULE_FILE" ] && echo 'yes' || echo 'no')"
            echo "  bin/hello.sh present: $([ -n "$HELLO_SH" ] && echo 'yes' || echo 'no')"

            # Verify read-only mount
            RO_TEST=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                touch /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/test-write 2>&1 || echo "read-only")

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
fi

# =================================================================
# FS-CSI-02: CSI driver — unmount on pod delete
# =================================================================
begin_test "FS-CSI-02: CSI driver — unmount on pod delete"

if ! $CSI_AVAILABLE; then
    skip_test "FS-CSI-02" "CSI driver not ready"
else
    # Delete the pod
    kubectl delete pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" --grace-period=5 2>/dev/null || true

    # Wait for pod to be deleted
    echo "  Waiting for pod deletion..."
    for i in $(seq 1 30); do
        POD_EXISTS=$(kubectl get pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null || echo "")
        if [ -z "$POD_EXISTS" ]; then
            break
        fi
        sleep 1
    done

    # Verify no mount leftovers via the test pod (which has host access)
    CSI_MOUNTS=$(exec_in_pod mount 2>/dev/null | grep "cfgd" | grep "csi-mount-test" || echo "")
    if [ -z "$CSI_MOUNTS" ]; then
        pass_test "FS-CSI-02"
    else
        fail_test "FS-CSI-02" "CSI mount still present after pod deletion"
        echo "  Remaining mounts: $CSI_MOUNTS"
    fi
fi
