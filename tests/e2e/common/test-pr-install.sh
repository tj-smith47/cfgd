#!/usr/bin/env bash
# Checks without a cluster that helpers.sh names the PR-owned install (its
# release, namespace, workloads, pod selectors and CSI driver) from one run id,
# that a local run id is the same in every process of one checkout, and that
# ensure_namespace and running_image address the namespaces they are given.
# kubectl is a stub on PATH that logs its arguments, so nothing reaches a cluster.
#
# Usage: tests/e2e/common/test-pr-install.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
e2e_root="$(dirname "$here")"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

pass() { echo "PASS  $1"; }
fail() {
    echo "FAIL  $1"
    failures=$((failures + 1))
}

mkdir -p "$scratch/bin"
cat > "$scratch/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$KUBECTL_LOG"
STUB
chmod +x "$scratch/bin/kubectl"
log="$scratch/kubectl.log"

# Each check sources helpers.sh in a fresh shell, the way a suite does, and
# prints what the inner script echoes.
# shellcheck disable=SC2016 # the inner script expands its own positional args
in_helpers() {
    local script="$1"; shift
    env -u GITHUB_RUN_ID -u CFGD_NAMESPACE PATH="$scratch/bin:$PATH" KUBECTL_LOG="$log" \
        REGISTRY=r.example CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"; eval "$2"' _ "$e2e_root" "$script"
}

expect_var() {
    local var="$1" want="$2" got
    got="$(in_helpers "printf '%s' \"\${$var}\"" GITHUB_RUN_ID=42 2>&1)" || got="(failed: $got)"
    if [ "$got" = "$want" ]; then pass "$var=$got"; else fail "$var=$got (want $want)"; fi
}

expect_var E2E_INSTALL_RELEASE cfgd-e2e-42
expect_var E2E_INSTALL_NS cfgd-e2e-42-sys
expect_var E2E_OPERATOR_DEPLOY cfgd-e2e-42-operator
expect_var E2E_CSI_DS cfgd-e2e-42-csi
expect_var E2E_WEBHOOK_SVC cfgd-e2e-42-webhook
expect_var E2E_OPERATOR_PODS "app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=operator"
expect_var E2E_CSI_PODS "app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=csi-driver"
expect_var CSI_DRIVER_NAME e2e.csi.cfgd.io

# A local setup and a local suite are separate processes; both must name the
# same install.
first="$(in_helpers "printf %s \"\$E2E_RUN_ID\"" 2>&1)" || first="(failed: $first)"
second="$(in_helpers "printf %s \"\$E2E_RUN_ID\"" 2>&1)" || second="(failed: $second)"
sha="$(git -C "$e2e_root" rev-parse --short HEAD)"
if [ "$first" = "local-$sha" ] && [ "$second" = "$first" ]; then
    pass "local E2E_RUN_ID agrees across processes: $first"
else
    fail "local E2E_RUN_ID: first=$first second=$second (want local-$sha twice)"
fi

if [ -s "$log" ]; then
    fail "sourcing helpers.sh called kubectl: $(tr '\n' ';' < "$log")"
else
    pass "sourcing helpers.sh calls no kubectl"
fi

# expect_kubectl <label> <script> <line...>: each line must appear in the log.
expect_kubectl() {
    local label="$1" script="$2" line ok=true; shift 2
    : > "$log"
    in_helpers "$script" GITHUB_RUN_ID=42 >/dev/null 2>&1 || ok=false
    for line in "$@"; do
        grep -qxF -- "$line" "$log" || ok=false
    done
    if $ok; then pass "$label"; else fail "$label: kubectl saw $(tr '\n' ';' < "$log")"; fi
}

expect_kubectl "ensure_namespace labels a namespace it creates with the run label" \
    'ensure_namespace e2e-x-42' \
    "create namespace e2e-x-42" "label namespace e2e-x-42 cfgd.io/e2e-run=42 --overwrite"

# The stub's create succeeds, so ensure_namespace reaches the label branch and
# only its guard keeps the live namespace unlabelled.
expect_unlabelled() {
    local label="$1" ns="$2" rc=0; shift 2
    : > "$log"
    in_helpers "ensure_namespace $ns" GITHUB_RUN_ID=42 "$@" >/dev/null 2>&1 || rc=$?
    if [ "$rc" -ne 0 ] || ! grep -qxF "create namespace $ns" "$log" || grep -q '^label' "$log"; then
        fail "$label: rc=$rc, kubectl saw $(tr '\n' ';' < "$log")"
    else
        pass "$label"
    fi
}
expect_unlabelled "ensure_namespace leaves the shared cfgd-system namespace unlabelled" cfgd-system
expect_unlabelled "ensure_namespace leaves cfgd-system unlabelled when CFGD_NAMESPACE names another" \
    cfgd-system CFGD_NAMESPACE=other
expect_unlabelled "ensure_namespace leaves \$CFGD_NAMESPACE unlabelled" live-ns CFGD_NAMESPACE=live-ns

expect_kubectl "running_image reads cfgd-system by default" \
    'running_image daemonset cfgd-csi-csi cfgd-csi' \
    'get daemonset cfgd-csi-csi -n cfgd-system -o jsonpath={.spec.template.spec.containers[?(@.name=="cfgd-csi")].image}'
expect_kubectl "running_image reads the namespace it is given" \
    "running_image daemonset \"\$E2E_CSI_DS\" cfgd-csi \"\$E2E_INSTALL_NS\"" \
    'get daemonset cfgd-e2e-42-csi -n cfgd-e2e-42-sys -o jsonpath={.spec.template.spec.containers[?(@.name=="cfgd-csi")].image}'

# CSI_DRIVER_NAME is the one spelling of the PR install's driver; a csiDriver.name
# key in the values file, in block or flow form, could drift from it. A file yq
# cannot read is a failure too, or a broken file would pass as "no key".
values="$e2e_root/manifests/pr-install-values.yaml"
has_name="$(yq '.csiDriver | has("name")' "$values" 2>&1 | tr '\n' ' ' | sed 's/ *$//' || true)"
case "$has_name" in
    false) pass "pr-install-values.yaml leaves csiDriver.name to --set-string csiDriver.name=\$CSI_DRIVER_NAME" ;;
    true) fail "$values sets csiDriver.name ($(yq '.csiDriver.name' "$values")); the install passes --set-string csiDriver.name=\$CSI_DRIVER_NAME" ;;
    *) fail "yq could not read $values: $has_name" ;;
esac

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all PR-install naming checks passed"
