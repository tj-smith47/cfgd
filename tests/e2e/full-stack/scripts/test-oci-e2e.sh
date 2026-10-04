# shellcheck shell=bash
# Full-stack E2E tests: OCI Supply Chain
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== OCI Supply Chain Tests ==="

# =================================================================
# OCI-E2E-01: Push -> Module CRD -> CSI mount
# =================================================================
begin_test "OCI-E2E-01: Push module, create Module CRD, deploy pod, verify content"

OCI01_NS="e2e-oci01-${E2E_RUN_ID}"
OCI01_MOD="oci01-mod-${E2E_RUN_ID}"
OCI01_REF="${REGISTRY}/cfgd-e2e/oci01:v1.0-${E2E_RUN_ID}"
OCI01_DIR=$(mktemp -d)
create_test_module_dir "$OCI01_DIR" "$OCI01_MOD" "1.0.0"
PUSH_OK=true
"$CFGD_BIN" module push "$OCI01_DIR" --artifact "$OCI01_REF" --no-color 2>&1 || PUSH_OK=false
rm -rf "$OCI01_DIR"

if [ "$PUSH_OK" = "false" ]; then
    fail_test "OCI-E2E-01" "Failed to push test module to registry"
else
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI01_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI01_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    ensure_namespace "$OCI01_NS"
    ensure_label namespace "$OCI01_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$OCI01_NS" "${OCI01_MOD}:v1.0" || true

    kubectl apply -n "$OCI01_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: oci01-pod
  annotations:
    cfgd.io/modules: "${OCI01_MOD}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for pod to be running..."
    POD_RUNNING=false
    wait_for_k8s_field pod oci01-pod "$OCI01_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        if module_mount_holds oci01-pod "$OCI01_NS" "$OCI01_MOD"; then
            pass_test "OCI-E2E-01"
        else
            fail_test "OCI-E2E-01" "Module content at the mount path is not what was pushed ($MOUNT_SEEN)"
        fi
    else
        fail_test "OCI-E2E-01" "Pod did not reach Running state (CSI mount may have failed)"
        kubectl describe pod oci01-pod -n "$OCI01_NS" 2>/dev/null | tail -20
    fi
fi

# =================================================================
# OCI-E2E-02: Signed artifact
# =================================================================
begin_test "OCI-E2E-02: Push with --sign, Module CRD with signature, verify mount"

OCI02_NS="e2e-oci02-${E2E_RUN_ID}"
OCI02_MOD="oci02-signed-${E2E_RUN_ID}"
OCI02_REF="${REGISTRY}/cfgd-e2e/oci02-signed:v1.0-${E2E_RUN_ID}"
OCI02_DIR=$(mktemp -d)
create_test_module_dir "$OCI02_DIR" "$OCI02_MOD" "1.0.0"

# Generate a cosign key pair for signing
OCI02_KEYDIR=$(mktemp -d)
COSIGN_PASSWORD="" cosign generate-key-pair --output-key-prefix "$OCI02_KEYDIR/e2e" 2>/dev/null || true

PUSH_OK=true
if [ -f "$OCI02_KEYDIR/e2e.key" ]; then
    COSIGN_PASSWORD="" "$CFGD_BIN" module push "$OCI02_DIR" \
        --artifact "$OCI02_REF" --sign --key "$OCI02_KEYDIR/e2e.key" --no-color 2>&1 || PUSH_OK=false
else
    "$CFGD_BIN" module push "$OCI02_DIR" \
        --artifact "$OCI02_REF" --sign --no-color 2>&1 || PUSH_OK=false
fi
rm -rf "$OCI02_DIR"

if [ "$PUSH_OK" = "false" ]; then
    fail_test "OCI-E2E-02" "Failed to push signed module to registry"
    rm -rf "$OCI02_KEYDIR"
else
    # Read the public key for the Module CRD if available
    PUB_KEY=""
    if [ -f "$OCI02_KEYDIR/e2e.pub" ]; then
        PUB_KEY=$(cat "$OCI02_KEYDIR/e2e.pub")
    fi
    rm -rf "$OCI02_KEYDIR"

    if [ -n "$PUB_KEY" ]; then
        kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI02_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI02_REF}"
  mountPolicy: Always
  signature:
    cosign:
      publicKey: |
        $(sed '2,$s/^/        /' <<<"$PUB_KEY")
EOF
    else
        kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI02_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI02_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF
    fi

    ensure_namespace "$OCI02_NS"
    ensure_label namespace "$OCI02_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$OCI02_NS" "${OCI02_MOD}:v1.0" || true

    kubectl apply -n "$OCI02_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: oci02-pod
  annotations:
    cfgd.io/modules: "${OCI02_MOD}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for pod to be running..."
    POD_RUNNING=false
    wait_for_k8s_field pod oci02-pod "$OCI02_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        if module_mount_holds oci02-pod "$OCI02_NS" "$OCI02_MOD"; then
            pass_test "OCI-E2E-02"
        else
            fail_test "OCI-E2E-02" "Signed module content at the mount path is not what was pushed ($MOUNT_SEEN)"
        fi
    else
        fail_test "OCI-E2E-02" "Pod did not reach Running state"
        kubectl describe pod oci02-pod -n "$OCI02_NS" 2>/dev/null | tail -20
    fi
fi

# =================================================================
# OCI-E2E-03: Unsigned artifact rejected
# =================================================================
begin_test "OCI-E2E-03: Module with disallow unsigned policy rejects unsigned module"

OCI03_MOD="oci03-unsigned-${E2E_RUN_ID}"
OCI03_REF="${REGISTRY}/cfgd-e2e/oci03-unsigned:v1.0-${E2E_RUN_ID}"
OCI03_DIR=$(mktemp -d)
create_test_module_dir "$OCI03_DIR" "$OCI03_MOD" "1.0.0"
PUSH_OK=true
"$CFGD_BIN" module push "$OCI03_DIR" --artifact "$OCI03_REF" --no-color 2>&1 || PUSH_OK=false
rm -rf "$OCI03_DIR"

if [ "$PUSH_OK" = "false" ]; then
    fail_test "OCI-E2E-03" "Failed to push unsigned module to registry"
else
    # Create a ClusterConfigPolicy that disallows unsigned modules
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: oci03-no-unsigned-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector: {}
  security:
    allowUnsigned: false
    trustedRegistries:
      - "${REGISTRY}/*"
EOF

    # Try to create a Module without signature; the webhook should reject it
    REJECT_OUTPUT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI03_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI03_REF}"
  mountPolicy: Always
EOF
)
    echo "  Webhook response: $(echo "$REJECT_OUTPUT" | head -3)"

    # Any failed apply prints "error", so the verdict needs the webhook's own
    # refusal, and a refused module was never stored.
    if k8s_exists module "$OCI03_MOD"; then
        fail_test "OCI-E2E-03" "Unsigned module was accepted despite disallow-unsigned policy"
        kubectl delete module "$OCI03_MOD" --ignore-not-found 2>/dev/null || true
    elif grep -q "unsigned modules are not allowed" <<<"$REJECT_OUTPUT"; then
        pass_test "OCI-E2E-03"
    else
        fail_test "OCI-E2E-03" "The apply failed without the webhook's unsigned-module refusal: $(head -3 <<<"$REJECT_OUTPUT")"
    fi

    # Clean up the ClusterConfigPolicy
    kubectl delete clusterconfigpolicy "oci03-no-unsigned-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
fi

# =================================================================
# OCI-E2E-04: Multi-platform
# =================================================================
begin_test "OCI-E2E-04: Push --platform linux/amd64,linux/arm64, verify Module status"

OCI04_MOD="oci04-multi-${E2E_RUN_ID}"
OCI04_REF="${REGISTRY}/cfgd-e2e/oci04-multi:v1.0-${E2E_RUN_ID}"

# Push two platform-specific artifacts, then verify the Module CRD status
OCI04_DIR_AMD=$(mktemp -d)
OCI04_DIR_ARM=$(mktemp -d)
create_test_module_dir "$OCI04_DIR_AMD" "$OCI04_MOD" "1.0.0"
create_test_module_dir "$OCI04_DIR_ARM" "$OCI04_MOD" "1.0.0"

PUSH_AMD_OK=true
PUSH_ARM_OK=true
"$CFGD_BIN" module push "$OCI04_DIR_AMD" \
    --artifact "$OCI04_REF" --platform linux/amd64 --no-color 2>&1 || PUSH_AMD_OK=false
"$CFGD_BIN" module push "$OCI04_DIR_ARM" \
    --artifact "$OCI04_REF" --platform linux/arm64 --no-color 2>&1 || PUSH_ARM_OK=false
rm -rf "$OCI04_DIR_AMD" "$OCI04_DIR_ARM"

if [ "$PUSH_AMD_OK" = "false" ] || [ "$PUSH_ARM_OK" = "false" ]; then
    fail_test "OCI-E2E-04" "Failed to push multi-platform module"
else
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI04_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI04_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    # The two pushes above name one tag, so the tag has to resolve to an index
    # listing both platforms for the controller to report both.
    oci04_both_platforms() {
        PLATFORMS=$(kubectl get module "$OCI04_MOD" \
            -o jsonpath='{.status.availablePlatforms[*]}' 2>/dev/null || echo "")
        case " $PLATFORMS " in
            *" linux/amd64 "*) ;;
            *) return 1 ;;
        esac
        case " $PLATFORMS " in
            *" linux/arm64 "*) return 0 ;;
            *) return 1 ;;
        esac
    }
    PLATFORMS=""
    echo "  Waiting for Module availablePlatforms..."
    if wait_until 60 2 "$OCI04_MOD to list linux/amd64 and linux/arm64" oci04_both_platforms; then
        echo "  Available platforms: $PLATFORMS"
        pass_test "OCI-E2E-04"
    else
        fail_test "OCI-E2E-04" "Expected availablePlatforms to list linux/amd64 and linux/arm64, got: ${PLATFORMS:-none}"
    fi
fi

# =================================================================
# OCI-E2E-05: Digest pinning
# =================================================================
begin_test "OCI-E2E-05: Module references @sha256:..., verify mount"

OCI05_NS="e2e-oci05-${E2E_RUN_ID}"
OCI05_MOD="oci05-digest-${E2E_RUN_ID}"
OCI05_TAG_REF="${REGISTRY}/cfgd-e2e/oci05-digest:v1.0-${E2E_RUN_ID}"
OCI05_DIR=$(mktemp -d)
create_test_module_dir "$OCI05_DIR" "$OCI05_MOD" "1.0.0"

# Push and capture the digest from output
PUSH_OUTPUT=""
PUSH_OK=true
PUSH_OUTPUT=$("$CFGD_BIN" module push "$OCI05_DIR" --artifact "$OCI05_TAG_REF" --no-color 2>&1) || PUSH_OK=false
rm -rf "$OCI05_DIR"

echo "  Push output: $(echo "$PUSH_OUTPUT" | head -5)"

# Extract digest (sha256:...) from push output
DIGEST=$(echo "$PUSH_OUTPUT" | grep -oE 'sha256:[a-f0-9]{64}' | head -1 || echo "")

if [ "$PUSH_OK" = "false" ]; then
    fail_test "OCI-E2E-05" "Failed to push module to registry"
elif [ -z "$DIGEST" ]; then
    fail_test "OCI-E2E-05" "Could not extract digest from push output"
else
    # Build the digest-pinned reference (repo@sha256:...)
    OCI05_REPO=$(echo "$OCI05_TAG_REF" | cut -d: -f1)
    OCI05_DIGEST_REF="${OCI05_REPO}@${DIGEST}"
    echo "  Digest-pinned ref: $OCI05_DIGEST_REF"

    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI05_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI05_DIGEST_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    ensure_namespace "$OCI05_NS"
    ensure_label namespace "$OCI05_NS" cfgd.io/inject-modules=true --overwrite

    wait_for_injection "$OCI05_NS" "${OCI05_MOD}:v1.0" || true

    kubectl apply -n "$OCI05_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: oci05-pod
  annotations:
    cfgd.io/modules: "${OCI05_MOD}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for pod to be running..."
    POD_RUNNING=false
    wait_for_k8s_field pod oci05-pod "$OCI05_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        if module_mount_holds oci05-pod "$OCI05_NS" "$OCI05_MOD"; then
            pass_test "OCI-E2E-05"
        else
            fail_test "OCI-E2E-05" "Digest-pinned module content at the mount path is not what was pushed ($MOUNT_SEEN)"
        fi
    else
        fail_test "OCI-E2E-05" "Pod did not reach Running state (digest-pinned CSI mount failed)"
        kubectl describe pod oci05-pod -n "$OCI05_NS" 2>/dev/null | tail -20
    fi
fi

# =================================================================
# OCI-E2E-06: Registry auth
# =================================================================
begin_test "OCI-E2E-06: Push, Module CRD, CSI uses imagePullSecrets"

OCI06_NS="e2e-oci06-${E2E_RUN_ID}"
OCI06_MOD="oci06-auth-${E2E_RUN_ID}"
OCI06_REF="${REGISTRY}/cfgd-e2e/oci06-auth:v1.0-${E2E_RUN_ID}"
OCI06_DIR=$(mktemp -d)
create_test_module_dir "$OCI06_DIR" "$OCI06_MOD" "1.0.0"
PUSH_OK=true
"$CFGD_BIN" module push "$OCI06_DIR" --artifact "$OCI06_REF" --no-color 2>&1 || PUSH_OK=false
rm -rf "$OCI06_DIR"

if [ "$PUSH_OK" = "false" ]; then
    fail_test "OCI-E2E-06" "Failed to push module to registry"
else
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${OCI06_MOD}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI06_REF}"
  mountPolicy: Always
  signature:
    cosign:
      keyless: true
EOF

    ensure_namespace "$OCI06_NS"
    ensure_label namespace "$OCI06_NS" cfgd.io/inject-modules=true --overwrite

    # Wait for registry-credentials to be replicated by Reflector
    echo "  Waiting for registry-credentials in namespace..."
    if ! wait_for_registry_credentials "$OCI06_NS" 30; then
        echo "  WARN: registry-credentials not replicated, creating pod anyway"
    fi

    wait_for_injection "$OCI06_NS" "${OCI06_MOD}:v1.0" || true

    # Create pod with imagePullSecrets referencing the registry-credentials secret
    kubectl apply -n "$OCI06_NS" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: oci06-pod
  annotations:
    cfgd.io/modules: "${OCI06_MOD}:v1.0"
spec:
  imagePullSecrets:
    - name: registry-credentials
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    echo "  Waiting for pod to be running..."
    POD_RUNNING=false
    wait_for_k8s_field pod oci06-pod "$OCI06_NS" \
        '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

    if $POD_RUNNING; then
        PULL_SECRETS=$(kubectl get pod oci06-pod -n "$OCI06_NS" \
            -o jsonpath='{.spec.imagePullSecrets[*].name}' 2>/dev/null || echo "")
        echo "  imagePullSecrets: ${PULL_SECRETS:-none}"

        if ! module_mount_holds oci06-pod "$OCI06_NS" "$OCI06_MOD"; then
            fail_test "OCI-E2E-06" "Module content at the mount path is not what was pushed ($MOUNT_SEEN)"
        elif ! echo "$PULL_SECRETS" | grep -qwF "registry-credentials"; then
            fail_test "OCI-E2E-06" "Pod carries no registry-credentials pull secret: ${PULL_SECRETS:-none}"
        else
            pass_test "OCI-E2E-06"
        fi
    else
        fail_test "OCI-E2E-06" "Pod did not reach Running state (registry auth may have failed)"
        kubectl describe pod oci06-pod -n "$OCI06_NS" 2>/dev/null | tail -20
    fi
fi
