#!/usr/bin/env bash
# Checks without a cluster that:
#   - helpers.sh names the PR-owned install (its release, namespace, workloads,
#     pod selectors, webhook configurations, webhook certificate and CSI driver)
#     from one run id
#   - a local run id is the same in every process of one checkout
#   - ensure_namespace and running_image address the namespaces they are given
#   - every cfgd.io object the operator and full-stack suites apply carries the
#     run label
#   - no e2e script runs a multi-command subshell or brace group as a condition
#   - setup's CRD check passes on CRDs whose spec matches the cluster's,
#     description text and API server defaults aside, and stops on a changed
#     spec, a CRD that is missing, unreadable or not Established, or a manifest
#     with nothing to compare
#   - argocd_managed tells a tracked, untracked and unreadable object apart in
#     the namespace it is given, and the Crossplane suite installs Crossplane
#     only where ArgoCD does not track it and stops when it cannot tell
#   - the CRD check names the source and rerun advice its caller passes
#   - no e2e script writes a CRD outside the exempt list, and no tracked
#     manifest under tests/e2e holds one
#   - require_release_webhooks_scoped passes on release webhooks scoped away
#     from run-labelled objects and stops on an unscoped entry, a configuration
#     ArgoCD tracks, and one that is missing or unreadable; the operator,
#     full-stack and gateway suites call it
#   - every ERROR line an e2e script prints goes to stderr
#   - no full-stack case calls skip_test
#   - every full-stack helm install and upgrade scopes its operator, validating
#     webhook and pod injector to its own namespace through HELM_SCOPE, an
#     array of exactly the three flags that no line appends to or rewrites,
#     and overrides none of its keys
#   - no operator or full-stack suite script names the release's operator,
#     namespace, webhooks or CSI driver by hand, outside the full-stack
#     suite's kept gateway lines
# kubectl is a stub on PATH, so nothing reaches a cluster; the CRD check's stub
# hands YAML reading to the real kubectl, which reads it offline.
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
# KUBECTL_FAIL holds glob patterns separated by `|`; a call whose arguments
# match one of them exits 1.
cat > "$scratch/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
echo "$*" >> "$KUBECTL_LOG"
IFS='|' read -ra fail_patterns <<<"${KUBECTL_FAIL:-}"
for pattern in "${fail_patterns[@]}"; do
    # shellcheck disable=SC2053 # the pattern is meant to glob
    [[ "$*" == $pattern ]] && exit 1
done
exit 0
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
expect_var E2E_WEBHOOK_CERT cfgd-e2e-42-webhook-tls
expect_var E2E_VALIDATING_WEBHOOK cfgd-e2e-42
expect_var E2E_MUTATING_WEBHOOK cfgd-e2e-42-pod-injector
expect_var E2E_OPERATOR_PODS "app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=operator"
expect_var E2E_CSI_PODS "app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=csi-driver"
expect_var CSI_DRIVER_NAME e2e.csi.cfgd.io
expect_var E2E_RELEASE_VALIDATING_WEBHOOK cfgd-validating-webhooks
expect_var E2E_RELEASE_MUTATING_WEBHOOK cfgd-mutating-webhooks

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

# create_e2e_namespace stops at the write that failed, names it, and starts no
# heartbeat loop, also when called as a condition, where set -e is off. The
# inner script stops any loop it did start, so a regression fails the check and
# leaves no process behind.
expect_namespace_write_stop() {
    local verb="$1" pattern="$2" out rc=0
    # shellcheck disable=SC2016 # the inner script expands its own variables
    out="$(in_helpers 'rc=0; create_e2e_namespace || rc=$?; hb="${HEARTBEAT_PID:-none}"; stop_heartbeat; echo "returned $rc, heartbeat $hb"; exit "$rc"' \
        GITHUB_RUN_ID=42 E2E_NAMESPACE=ns-x KUBECTL_FAIL="$pattern" 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ] && grep -qxF "returned 1, heartbeat none" <<<"$out" && grep -qxF "ERROR: could not $verb namespace ns-x. Check that the runner can create, label and annotate namespaces." <<<"$out"; then
        pass "create_e2e_namespace stops with a message when it cannot $verb the namespace"
    else
        fail "create_e2e_namespace with a failing $verb: rc=$rc, printed [$out]"
    fi
}
expect_namespace_write_stop create 'get namespace*|create namespace*'
expect_namespace_write_stop label 'get namespace*|label namespace*'
expect_namespace_write_stop annotate 'get namespace*|annotate namespace*'

expect_kubectl "running_image reads cfgd-system by default" \
    'running_image daemonset cfgd-csi-csi cfgd-csi' \
    'get daemonset cfgd-csi-csi -n cfgd-system -o jsonpath={.spec.template.spec.containers[?(@.name=="cfgd-csi")].image}'
expect_kubectl "running_image reads the namespace it is given" \
    "running_image daemonset \"\$E2E_CSI_DS\" cfgd-csi \"\$E2E_INSTALL_NS\"" \
    'get daemonset cfgd-e2e-42-csi -n cfgd-e2e-42-sys -o jsonpath={.spec.template.spec.containers[?(@.name=="cfgd-csi")].image}'

repo_root="$(dirname "$(dirname "$e2e_root")")"

# `set -e` is off inside an if/elif/while/until condition, so a subshell or a
# brace group there that runs several commands reports only the last one's
# status, and a failure before it goes unseen. scan_subshell_conditions
# <file...> reads the shell lines heredocs.awk finds outside heredoc bodies and
# prints `COND file:line` for each condition whose subshell or brace group holds
# more than one command (a `;`, `&&` or newline inside it). A paren after the
# keyword with no `then` or `do` after it is awk inside a quoted program, and is
# skipped.
# shellcheck disable=SC2016 # an awk program; the $ fields belong to awk
scan_subshell_conditions() {
    { awk -f "$here/heredocs.awk" "$@" || echo "UNREADABLE heredocs.awk exited $?"; } | awk -F '\t' '
        function reset() { state = 0; q = ""; depth = 0; inner = ""; after = ""; start = 0; open = ""; shut = "" }
        function feed(line,    i, c) {
            for (i = 1; i <= length(line); i++) {
                c = substr(line, i, 1)
                if (c == "\\" && q != "\047") { inner = inner substr(line, i, 2); i++; continue }
                if (q != "") { if (c == q) q = ""; inner = inner c; continue }
                if (c == "\"" || c == "\047") { q = c; inner = inner c; continue }
                if (c == open) depth++
                else if (c == shut && --depth == 0) return i
                inner = inner c
            }
            inner = inner "\n"
            return 0
        }
        function opens_body(text) { return text ~ /(^|[ \t;])(then|do)([ \t;]|$)/ }
        function judge() {
            sub(/^[[:space:];]+/, "", inner)
            sub(/[[:space:];]+$/, "", inner)
            if (inner ~ /;|&&|\n/) print "COND " file ":" start
        }
        /^UNREADABLE / { print; next }
        $1 == "FILE" { reset(); file = $2; next }
        $1 != "SH" { next }
        { raw = $0; sub(/^[^\t]*\t[^\t]*\t[^\t]*\t/, "", raw) }
        state == 2 {
            state = 0
            if ((after !~ /[^ \t]/ || after ~ /^[ \t]*[0-9]*>/) && raw ~ /^[ \t]*(then|do)([ \t]|$)/) judge()
        }
        state == 1 {
            i = feed(raw)
            if (i) { after = substr(raw, i + 1); if (opens_body(after)) { judge(); state = 0 } else state = 2 }
            next
        }
        match(raw, /^[ \t]*(if|elif|while|until)[ \t]+(![ \t]+)?[({]/) {
            start = $3; inner = ""; depth = 1; q = ""
            open = substr(raw, RSTART + RLENGTH - 1, 1); shut = (open == "(") ? ")" : "}"
            rest = substr(raw, RSTART + RLENGTH)
            i = feed(rest)
            if (!i) state = 1
            else { after = substr(rest, i + 1); if (opens_body(after)) judge(); else state = 2 }
        }
    '
}

# The fixtures are .bash files, so the live population below never lists them.
cond_fixtures="$here/fixtures/subshell-conditions"
cond_got="$(cd "$cond_fixtures" && scan_subshell_conditions conditions.bash reset-a.bash reset-b.bash 2>&1)"
cond_want="COND conditions.bash:3
COND conditions.bash:11
COND conditions.bash:16
COND conditions.bash:24
COND conditions.bash:25
COND conditions.bash:27
COND conditions.bash:29
COND reset-b.bash:1"
if [ "$cond_got" = "$cond_want" ]; then
    pass "the subshell-condition scan reports each planted multi-command condition once and stays quiet on single commands, awk, heredoc bodies and a file left mid-condition"
else
    fail "the subshell-condition scan printed [$cond_got], want [$cond_want]"
fi

cond_bad="$(scan_subshell_conditions "$cond_fixtures/absent.bash" 2>/dev/null)"
if [[ "$cond_bad" == UNREADABLE* ]]; then
    pass "the subshell-condition scan reports a file it cannot read"
else
    fail "the subshell-condition scan over a missing file printed [$cond_bad], want an UNREADABLE line"
fi

mapfile -t e2e_scripts < <(git -C "$repo_root" ls-files 'tests/e2e/*.sh')
if [ "${#e2e_scripts[@]}" -eq 0 ]; then
    fail "git ls-files 'tests/e2e/*.sh' matched no script, so the subshell-condition scan read nothing"
elif cond_found="$(cd "$repo_root" && scan_subshell_conditions "${e2e_scripts[@]}")" && [ -z "$cond_found" ]; then
    pass "no condition in tests/e2e runs a multi-command subshell or brace group (${#e2e_scripts[@]} scripts)"
else
    fail "set -e is off inside a condition, so only the last command's status is read: ${cond_found:-the scan failed}"
fi

# argocd_managed and the Crossplane install against stubs: kubectl answers a
# get as ARGO_STATE says (tracked prints a tracking id, untracked prints
# nothing, unreadable fails) and helm only records its call.
mkdir -p "$scratch/argo-bin"
cat > "$scratch/argo-bin/kubectl" <<'STUB'
#!/usr/bin/env bash
echo "kubectl $*" >> "$ARGO_LOG"
case "$ARGO_STATE" in
    tracked) printf '%s' 'crossplane:apps/Deployment:crossplane-system/crossplane' ;;
    untracked) ;;
    *) echo 'Error from server (Forbidden): deployments.apps "crossplane" is forbidden' >&2; exit 1 ;;
esac
STUB
cat > "$scratch/argo-bin/helm" <<'STUB'
#!/usr/bin/env bash
echo "helm $*" >> "$ARGO_LOG"
STUB
chmod +x "$scratch/argo-bin/kubectl" "$scratch/argo-bin/helm"

# argo_case <state> <script>: runs the script after sourcing helpers.sh and
# prints its output, then `rc=<status>`; the calls the stubs saw are in
# $scratch/argo.log.
# shellcheck disable=SC2016 # the inner script expands its own positional args
argo_case() {
    : > "$scratch/argo.log"
    env -u GITHUB_RUN_ID -u CFGD_NAMESPACE PATH="$scratch/argo-bin:$PATH" \
        ARGO_STATE="$1" ARGO_LOG="$scratch/argo.log" REGISTRY=r.example CLI_SCRATCH="$scratch" \
        bash -c 'source "$1/common/helpers.sh"; rc=0; eval "$2" 2>&1 || rc=$?; echo "rc=$rc"' _ "$e2e_root" "$2"
}

for argo_want in tracked:0 untracked:1 unreadable:2; do
    out="$(argo_case "${argo_want%%:*}" 'argocd_managed deployment crossplane crossplane-system')"
    if [ "$(sed -n 's/^rc=//p' <<<"$out")" = "${argo_want##*:}" ] &&
        grep -qx 'kubectl get deployment crossplane -n crossplane-system .*' "$scratch/argo.log"; then
        pass "argocd_managed returns ${argo_want##*:} on a ${argo_want%%:*} object in the namespace it is given"
    else
        fail "argocd_managed on a ${argo_want%%:*} object: got [$out] with kubectl calls [$(cat "$scratch/argo.log")], want rc=${argo_want##*:} from a get in crossplane-system"
    fi
done
argo_case untracked 'argocd_managed deployment cfgd-operator' >/dev/null
if grep -qx 'kubectl get deployment cfgd-operator -n cfgd-system .*' "$scratch/argo.log"; then
    pass "argocd_managed reads cfgd-system when given no namespace"
else
    fail "argocd_managed with no namespace called [$(cat "$scratch/argo.log")], want a get in cfgd-system"
fi

# argo_errors <output>: its ERROR lines, or nothing.
argo_errors() { grep '^ERROR' <<<"$1" || true; } # rc-ok: no ERROR line is a valid outcome, compared by the caller
for argo_want in tracked:0 untracked:1; do
    out="$(argo_case "${argo_want%%:*}" 'argocd_owner daemonset cfgd-csi-csi e2e-ns "rerun the full-stack suite"')"
    if [ "$(sed -n 's/^rc=//p' <<<"$out")" = "${argo_want##*:}" ] && [ -z "$(argo_errors "$out")" ]; then
        pass "argocd_owner returns ${argo_want##*:} on a ${argo_want%%:*} object and prints no ERROR"
    else
        fail "argocd_owner on a ${argo_want%%:*} object: got [$out], want rc=${argo_want##*:} and no ERROR"
    fi
done
out="$(argo_case unreadable 'argocd_owner daemonset cfgd-csi-csi e2e-ns "rerun the full-stack suite"')"
if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 2 ] &&
    [ "$(argo_errors "$out")" = "ERROR: could not read daemonset/cfgd-csi-csi in e2e-ns. Check that the runner can get daemonset objects there, then rerun the full-stack suite." ] &&
    grep -qx 'kubectl get daemonset cfgd-csi-csi -n e2e-ns .*' "$scratch/argo.log"; then
    pass "argocd_owner returns 2 with one ERROR naming the object, its namespace and the rerun advice it is given"
else
    fail "argocd_owner on an unreadable namespaced object: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=2 and the ERROR naming daemonset/cfgd-csi-csi in e2e-ns"
fi
out="$(argo_case unreadable 'argocd_owner validatingwebhookconfiguration cfgd-validating-webhooks - "rerun setup"')"
if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 2 ] &&
    [ "$(argo_errors "$out")" = "ERROR: could not read validatingwebhookconfiguration/cfgd-validating-webhooks. Check that the runner can get validatingwebhookconfiguration objects, then rerun setup." ] &&
    grep -qx 'kubectl get validatingwebhookconfiguration cfgd-validating-webhooks --ignore-not-found .*' "$scratch/argo.log"; then
    pass "argocd_owner names no namespace for a cluster-scoped kind, in the ERROR or the get"
else
    fail "argocd_owner on an unreadable cluster-scoped object: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=2, an ERROR with no namespace and a get without -n"
fi

for argo_empty in 'argocd_owner deployment cfgd-operator "" "rerun setup"' 'argocd_owner deployment cfgd-operator cfgd-system' 'argocd_managed deployment cfgd-operator ""'; do
    out="$(argo_case untracked "$argo_empty")"
    if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 2 ] && [ "$(argo_errors "$out" | wc -l)" -eq 1 ] && [ ! -s "$scratch/argo.log" ]; then
        pass "[$argo_empty] is refused with status 2 and one ERROR, and kubectl is not called"
    else
        fail "[$argo_empty]: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=2, one ERROR and no kubectl call"
    fi
done
out="$(argo_case tracked crossplane_install)"
if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 0 ] && ! grep -q '^helm' "$scratch/argo.log" &&
    grep -q "ArgoCD's; installing nothing" <<<"$out"; then
    pass "the Crossplane install leaves an ArgoCD-tracked Crossplane alone and calls no helm"
else
    fail "the Crossplane install on a tracked Crossplane: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=0, the ArgoCD line and no helm call"
fi
out="$(argo_case untracked crossplane_install)"
if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 0 ] &&
    grep -qx "helm upgrade --install crossplane crossplane-stable/crossplane .*" "$scratch/argo.log"; then
    pass "the Crossplane install runs helm upgrade --install where ArgoCD does not track Crossplane"
else
    fail "the Crossplane install on an untracked Crossplane: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=0 and a helm upgrade --install"
fi
out="$(argo_case unreadable crossplane_install)"
if [ "$(sed -n 's/^rc=//p' <<<"$out")" = 1 ] && ! grep -q '^helm' "$scratch/argo.log" &&
    grep -qx "ERROR: could not read deployment/crossplane in crossplane-system. Check that the runner can get deployment objects there, then rerun the Crossplane suite." <<<"$out"; then
    pass "the Crossplane install stops with an ERROR and calls no helm when it cannot read deployment/crossplane"
else
    fail "the Crossplane install on an unreadable Crossplane: got [$out] with calls [$(cat "$scratch/argo.log")], want rc=1, the ERROR line and no helm call"
fi

# The CRD check against fixture CRDs. A stub `kubectl get crd <name>` prints
# the fixture named <name>.yaml in CRD_LIVE_DIR as the cluster's copy, prints
# nothing when there is none and fails when CRD_GET_FAIL is set; every other
# kubectl call, and the YAML reading of each fixture, goes to the real kubectl,
# which needs no cluster for `annotate --local`.
crd_fixtures="$here/fixtures/crd-schema"
crd_fix="ArgoCD owns the cluster's CRDs; copy schemas/crds.yaml over /db/manifests/k3s/namespaces/crossplane-system/cfgd-crds.yaml, push it and let ArgoCD sync, then rerun setup."

# crd_case <live dir> <PR manifest> [VAR=value...]: prints check_pr_crds's
# output, then `rc=<status>`. CRD_SOURCE and CRD_RERUN, when set, are passed
# as the check's source label and rerun advice.
# shellcheck disable=SC2016 # the inner script expands its own positional args
crd_case() {
    local live="$1" pr="$2"; shift 2
    env -u GITHUB_RUN_ID -u CFGD_NAMESPACE PATH="$scratch/crd-bin:$PATH" \
        REAL_KUBECTL="$real_kubectl" CRD_LIVE_DIR="$live" \
        REGISTRY=r.example CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"
            rc=0
            if [ -n "${CRD_SOURCE:-}" ]; then
                check_pr_crds "$CRD_SOURCE" "$CRD_RERUN" < "$2" 2>&1 || rc=$?
            else
                check_pr_crds < "$2" 2>&1 || rc=$?
            fi
            echo "rc=$rc"' _ "$e2e_root" "$pr"
}

# crd_live <case> <widgets fixture or "">: a live dir holding gadgets and the
# named widgets variant, or no widgets at all when the variant is empty.
crd_live() {
    local dir="$scratch/crd-live/$1"
    mkdir -p "$dir"
    cp "$crd_fixtures/gadgets.yaml" "$dir/gadgets.example.io.yaml"
    [ -z "$2" ] || cp "$crd_fixtures/$2" "$dir/widgets.example.io.yaml"
    printf '%s\n' "$dir"
}

# expect_crd <label> <output> <want rc> <want ERROR lines, newline-separated>
expect_crd() {
    local label="$1" got="$2" want_rc="$3" want_errors="$4" got_rc got_errors
    got_rc="$(sed -n 's/^rc=//p' <<<"$got")"
    got_errors="$(grep '^ERROR' <<<"$got" || true)" # rc-ok: no ERROR line is a valid outcome, compared below
    if [ "$got_rc" = "$want_rc" ] && [ "$got_errors" = "$want_errors" ]; then
        pass "$label"
    else
        fail "$label: got rc=${got_rc:-none} with [$got_errors], want rc=$want_rc with [$want_errors]; full output: $got"
    fi
}

# expect_crd_change <live fixture> <what changed>: the check stops on the
# widgets CRD alone with the "changes the spec" ERROR.
expect_crd_change() {
    expect_crd "the CRD check stops when the cluster's widgets CRD differs in $2" \
        "$(crd_case "$(crd_live "${1%.yaml}" "$1")" "$crd_fixtures/pr.yaml")" 1 \
        "ERROR: this PR changes the spec of widgets.example.io (descriptions aside). $crd_fix"
}

crd_cases() {
    local out
    out="$(crd_case "$(crd_live equal widgets.yaml)" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check passes on a server-shaped copy (API defaults, last-applied annotation, server metadata, status) of the PR's CRDs" "$out" 0 ""
    out="$(crd_case "$(crd_live description widgets-description.yaml)" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check passes when only a description differs" "$out" 0 ""

    out="$(crd_case "$(crd_live changed widgets-changed.yaml)" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check stops on a changed schema field and names only that CRD" "$out" 1 \
        "ERROR: this PR changes the spec of widgets.example.io (descriptions aside). $crd_fix"
    if grep -q '"type": "string"' <<<"$out" && grep -q '"type": "integer"' <<<"$out"; then
        pass "the CRD check prints both sides of the spec difference"
    else
        fail "the CRD check printed no diff showing the size type change: $out"
    fi
    expect_crd_change widgets-property-dropped.yaml "a property named description"
    expect_crd_change widgets-columns.yaml "a printer column"
    expect_crd_change widgets-subresources.yaml "its subresources"
    expect_crd_change widgets-served.yaml "a version's served flag"
    expect_crd_change widgets-storage.yaml "a version's storage flag"
    expect_crd_change widgets-scope.yaml "its scope"
    expect_crd_change widgets-shortnames.yaml "its shortNames"
    expect_crd_change widgets-group.yaml "its group"

    out="$(crd_case "$(crd_live not-established widgets-not-established.yaml)" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check stops on a CRD whose Established condition is False" "$out" 1 \
        "ERROR: crd/widgets.example.io is not Established on the cluster (NotAccepted). Check ArgoCD's cfgd-crds sync and the CRD's status.conditions, then rerun setup."
    out="$(crd_case "$(crd_live no-conditions widgets-no-conditions.yaml)" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check stops on a CRD with no Established condition" "$out" 1 \
        "ERROR: crd/widgets.example.io is not Established on the cluster (no Established condition). Check ArgoCD's cfgd-crds sync and the CRD's status.conditions, then rerun setup."

    out="$(crd_case "$(crd_live missing "")" "$crd_fixtures/pr.yaml")"
    expect_crd "the CRD check stops on a CRD the cluster does not have and names it" "$out" 1 \
        "ERROR: this PR adds widgets.example.io, which the cluster does not have. $crd_fix"
    out="$(crd_case "$(crd_live unreadable widgets.yaml)" "$crd_fixtures/pr.yaml" CRD_GET_FAIL=1)"
    expect_crd "the CRD check stops when it cannot read the cluster's CRDs" "$out" 1 \
        "ERROR: could not read crd/widgets.example.io. Check that the runner can get customresourcedefinitions, then rerun setup.
ERROR: could not read crd/gadgets.example.io. Check that the runner can get customresourcedefinitions, then rerun setup."
    out="$(crd_case "$(crd_live empty "")" "$crd_fixtures/not-a-crd.yaml")"
    expect_crd "the CRD check fails on a manifest with no CRD in it" "$out" 1 \
        "ERROR: the cfgd-gen-crds output holds no CustomResourceDefinition to compare with the cluster. Check the cfgd-gen-crds output, then rerun setup."
    out="$(crd_case "$(crd_live empty-suite "")" "$crd_fixtures/not-a-crd.yaml" CRD_SOURCE=schemas/crds.yaml CRD_RERUN="rerun the Crossplane suite")"
    expect_crd "the CRD check names the source and rerun advice its caller passes" "$out" 1 \
        "ERROR: schemas/crds.yaml holds no CustomResourceDefinition to compare with the cluster. Check schemas/crds.yaml, then rerun the Crossplane suite."
    out="$(crd_case "$(crd_live noversions widgets.yaml)" "$crd_fixtures/no-versions.yaml")"
    expect_crd "the CRD check fails on a PR CRD with no versions" "$out" 1 \
        "ERROR: widgets.example.io in the cfgd-gen-crds output has no versions to compare. Check the cfgd-gen-crds output, then rerun setup."
}

if ! real_kubectl="$(command -v kubectl)" || ! command -v jq >/dev/null; then
    fail "the CRD check needs kubectl (it reads YAML offline with annotate --local) and jq on PATH; its cases did not run"
else
    mkdir -p "$scratch/crd-bin"
    cat > "$scratch/crd-bin/kubectl" <<'STUB'
#!/usr/bin/env bash
if [ "$1 $2" = "get crd" ]; then
    if [ -n "${CRD_GET_FAIL:-}" ]; then
        echo "Error from server (Forbidden): customresourcedefinitions is forbidden" >&2
        exit 1
    fi
    [ -f "$CRD_LIVE_DIR/$3.yaml" ] || exit 0
    exec "$REAL_KUBECTL" annotate --local -o json -f "$CRD_LIVE_DIR/$3.yaml" cfgd.io/e2e-unset-
fi
exec "$REAL_KUBECTL" "$@"
STUB
    chmod +x "$scratch/crd-bin/kubectl"
    crd_cases
fi

# ArgoCD owns the cluster's CRDs, so no e2e script writes one. crd-writes.awk
# reads heredocs.awk's records and reports each kubectl write that names a CRD,
# each helm install without --skip-crds and each heredoc holding a CRD.
# crd_kind matches a line setting kind to CustomResourceDefinition in YAML, its
# value bare, quoted or followed by a comment, as a list item's first key or
# inside a flow mapping, or as a JSON member. Both the scan and the manifest
# check read it.
crd_kind="^([^#]*[{,[:space:]-])?kind:[[:space:]]*[\"']?CustomResourceDefinition[\"']?[[:space:]]*([,}#].*)?\$|\"kind\"[[:space:]]*:[[:space:]]*\"CustomResourceDefinition\""
scan_crd_writes() {
    awk -f "$here/heredocs.awk" "$@" | awk -v crd_kind="$crd_kind" -f "$here/crd-writes.awk"
}

writes_fixtures="$here/fixtures/crd-writes"
writes_out="$(cd "$writes_fixtures" && scan_crd_writes writes.bash reads.bash 2>&1)" ||
    fail "the CRD-write scan failed over its fixtures: $writes_out"
writes_got="$(cut -f1,2 <<<"$writes_out")"
writes_want="$(printf '%s\t%s\n' \
    KUBECTL writes.bash:1 KUBECTL writes.bash:2 KUBECTL writes.bash:3 KUBECTL writes.bash:4 \
    KUBECTL writes.bash:6 HELM writes.bash:7 HELM writes.bash:8 HELM writes.bash:9 \
    HEREDOC writes.bash:11 HEREDOC writes.bash:17 HEREDOC writes.bash:24 HEREDOC writes.bash:27 \
    HEREDOC writes.bash:30 HEREDOC writes.bash:33 HEREDOC writes.bash:36 HEREDOC writes.bash:40 \
    HEREDOC writes.bash:43)"
if [ "$writes_got" = "$writes_want" ]; then
    pass "the CRD-write scan reports each planted kubectl write, helm install and CRD heredoc once, with bare, quoted, comment-tailed, list-item, flow-style and JSON kinds, and stays quiet on reads, --local, --dry-run, --skip-crds, messages, comments and quoted values"
else
    fail "the CRD-write scan printed [$writes_got], want [$writes_want]"
fi
heredoc_got="$(grep '^HEREDOC' <<<"$writes_out" | cut -f2,3 || true)" # rc-ok: no HEREDOC line is a failing outcome, compared below
heredoc_want="$(cat "$writes_fixtures/heredoc-commands.tsv")"
if [ "$heredoc_got" = "$heredoc_want" ]; then
    pass "the CRD-write scan prints the command that opened each CRD heredoc"
else
    fail "the CRD-write scan printed heredoc commands [$heredoc_got], want [$heredoc_want]"
fi

# Each entry is TAG, file and the command as crd-writes.awk prints it, so an
# entry follows its command when lines move and goes stale when it changes.
# Crossplane's Helm install runs only where ArgoCD does not run Crossplane,
# and Crossplane's chart holds none of the CRDs in cfgd-crds.yaml.
crd_write_exempt="HELM	tests/e2e/common/helpers.sh	helm upgrade --install crossplane crossplane-stable/crossplane --namespace crossplane-system --create-namespace --wait --timeout 120s || {"

if [ "${#e2e_scripts[@]}" -eq 0 ]; then
    fail "git ls-files 'tests/e2e/*.sh' matched no script, so the CRD-write scan read nothing"
elif ! crd_writes="$(cd "$repo_root" && scan_crd_writes "${e2e_scripts[@]}")"; then
    fail "the CRD-write scan failed over tests/e2e"
else
    crd_write_keys="$(awk 'BEGIN { FS = OFS = "\t" } { sub(/:[0-9]+$/, "", $2); print }' <<<"$crd_writes")"
    unexempt="$(awk -v exempt="$crd_write_exempt" 'BEGIN { FS = "\t"; n = split(exempt, e, "\n"); for (i = 1; i <= n; i++) ok[e[i]] = 1 }
        NF { file = $2; sub(/:[0-9]+$/, "", file); if (!ok[$1 "\t" file "\t" $3]) print }' <<<"$crd_writes")"
    stale="$(grep -vxF -f <(printf '%s\n' "$crd_write_keys") <<<"$crd_write_exempt" || true)" # rc-ok: no line left is the passing outcome, judged below
    if [ -z "$unexempt" ] && [ -z "$stale" ]; then
        pass "no script in tests/e2e writes a CRD outside the exempt list (${#e2e_scripts[@]} scripts)"
    else
        fail "ArgoCD owns the cluster's CRDs, so no e2e script writes one. Remove the write; if the line writes no CRD, add TAG, file and command to crd_write_exempt with the reason; if an exempt command changed, update its entry. Writes: [${unexempt}] Exempt entries that match no write: [${stale}]"
    fi
fi

# A CRD in a tracked manifest under tests/e2e can be applied by a path that
# names no CRD, so none may hold one; the fixtures under common/fixtures are
# read only by these checks.
# crd_manifests FILE...: the files with a line matching crd_kind.
crd_manifests() {
    local rc=0
    grep -lE "$crd_kind" "$@" || rc=$?
    [ "$rc" -le 1 ]
}

manifest_fixtures="$here/fixtures/crd-manifests"
manifests_want="$(cd "$manifest_fixtures" && printf '%s\n' hit*)"
if manifests_got="$(cd "$manifest_fixtures" && crd_manifests hit* miss*)" && [ -n "$manifests_want" ] && [ "$manifests_got" = "$manifests_want" ]; then
    pass "the CRD-manifest check reports bare, quoted, comment-tailed, list-item, flow-style and JSON CRD kinds and stays quiet on a quoted value and a commented-out kind line"
else
    fail "the CRD-manifest check printed [$manifests_got], want [$manifests_want]"
fi
if crd_manifests "$manifest_fixtures/hit.yaml" "$manifest_fixtures/absent.yaml" >/dev/null 2>&1; then
    fail "the CRD-manifest check passed over a file it could not read"
else
    pass "the CRD-manifest check fails over a file it cannot read"
fi

mapfile -t e2e_manifests < <(git -C "$repo_root" ls-files 'tests/e2e/*.yaml' 'tests/e2e/*.yml' 'tests/e2e/*.json' | grep -v '^tests/e2e/common/fixtures/')
if [ "${#e2e_manifests[@]}" -eq 0 ]; then
    fail "git ls-files 'tests/e2e/*.yaml' 'tests/e2e/*.yml' 'tests/e2e/*.json' matched no manifest outside common/fixtures, so the CRD-manifest check read nothing"
elif ! crd_files="$(cd "$repo_root" && crd_manifests "${e2e_manifests[@]}")"; then
    fail "the CRD-manifest check could not read the manifests under tests/e2e"
elif [ -z "$crd_files" ]; then
    pass "no tracked manifest in tests/e2e holds a CRD (${#e2e_manifests[@]} manifests)"
else
    fail "ArgoCD owns the cluster's CRDs, so no tracked manifest under tests/e2e holds one; remove the CRD from: [$crd_files]"
fi

# yq_v4_problem: prints nothing when the yq on PATH is mikefarah yq v4, and
# otherwise what it found there.
yq_v4_problem() {
    local version
    if ! command -v yq >/dev/null; then
        echo "no yq"
        return 0
    fi
    version="$(yq --version 2>&1)" || { echo "$(command -v yq), whose --version failed: $version"; return 0; }
    [[ "$version" =~ mikefarah/yq.*\ version\ v?4\. ]] || echo "$(command -v yq): $version"
}
mkdir -p "$scratch/python-yq" "$scratch/no-yq"
printf '#!/bin/sh\necho "yq 3.4.3"\n' > "$scratch/python-yq/yq"
chmod +x "$scratch/python-yq/yq"
got="$(PATH="$scratch/python-yq:$PATH" yq_v4_problem)"
if [ "$got" = "$scratch/python-yq/yq: yq 3.4.3" ]; then pass "the yq check names a python yq it finds first on PATH"; else fail "the yq check with python yq first on PATH printed [$got]"; fi
got="$(PATH="$scratch/no-yq" yq_v4_problem)"
if [ "$got" = "no yq" ]; then pass "the yq check reports a PATH with no yq"; else fail "the yq check with no yq on PATH printed [$got]"; fi

# Every check below reads YAML with mikefarah yq v4. Another yq, or none, would
# fail each of them for a reason unrelated to what it checks, so the run stops
# here with one line naming the yq it found.
yq_problem="$(yq_v4_problem)"
if [ -n "$yq_problem" ]; then
    fail "needs mikefarah yq v4 on PATH (found: $yq_problem)"
    echo "$failures check(s) failed"
    exit 1
fi

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

# Every operator object a suite applies must carry the run label in its own
# metadata.labels: the PR operator reconciles only objects with that label, so an
# unlabelled object makes its case fail for a reason unrelated to the case. The
# operator's kinds come from the CRDs it serves, so a new CRD joins the rule.

# operator_kinds <crds.yaml>: one kind per line, or a FAIL line.
operator_kinds() {
    local kinds
    if ! kinds="$(yq -N '.spec.names.kind' "$1" 2>&1)"; then
        echo "FAIL yq could not read $1: $kinds"
    elif ! grep -qE '^[A-Z][A-Za-z0-9]*$' <<<"$kinds" || grep -qvE '^[A-Z][A-Za-z0-9]*$' <<<"$kinds"; then
        echo "FAIL $1 names no CRD kinds: ${kinds:-empty}"
    else
        printf '%s\n' "$kinds"
    fi
}

# The words a heredoc body's expansions render to, random per run so no text
# written by hand in a script can equal either: the run label's value renders as
# label_sentinel, and every other expansion as expansion_placeholder.
label_sentinel="run-label-$$-$RANDOM$RANDOM"
expansion_placeholder="e2e-expansion-$$-$RANDOM$RANDOM"

# yaml_entries_of <helpers.sh>: one NAME=key word per `export NAME_YAML="key: ..."`
# line, the YAML entries helpers.sh hands heredoc bodies to expand.
yaml_entries_of() {
    sed -n 's/^export \([A-Z0-9_]*_YAML\)="\([^: "]*\): .*/\1=\2/p' "$1" | tr '\n' ' '
}
helpers="$e2e_root/common/helpers.sh"
yaml_entries="$(yaml_entries_of "$helpers")"

# render_awk defines render(s): one line of an unquoted heredoc body as bash
# expands it, so a YAML parser reads what the cluster would be sent. Each
# ${NAME} that yaml_entries names becomes its entry, `key: "<value>"`, the
# value being label_sentinel for E2E_RUN_LABEL_YAML and expansion_placeholder
# for the rest. Every other $VAR, ${...}, $(...) and `...` becomes
# expansion_placeholder alone, so each line stays one line and a parser's line
# numbers stay those of the source. A $(...) or ${...} still open at the end of
# the line takes the rest of it.
# shellcheck disable=SC2016 # an awk program; the $ signs belong to awk
render_awk='
function close_of(s, i, open, shut,   d, c) {
    for (d = 0; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "\\") i++
        else if (c == open) d++
        else if (c == shut && --d == 0) return i
    }
    return length(s)
}
function expand(name) {
    if (!(name in entry_key)) return placeholder
    return entry_key[name] ": \"" (name == "E2E_RUN_LABEL_YAML" ? sentinel : placeholder) "\""
}
function render(s,   out, i, c, j, n) {
    out = ""
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        n = substr(s, i + 1, 1)
        if (c == "\\" && n ~ /[$`\\]/) { out = out n; i++ }
        else if (c == "`") { j = index(substr(s, i + 1), "`"); i = j ? i + j : length(s); out = out placeholder }
        else if (c != "$") out = out c
        else if (n == "(") { i = close_of(s, i + 1, "(", ")"); out = out placeholder }
        else if (n == "{") { j = close_of(s, i + 1, "{", "}"); out = out expand(substr(s, i + 2, j - i - 2)); i = j }
        else if (match(substr(s, i + 1), /^[A-Za-z_][A-Za-z0-9_]*/)) { out = out expand(substr(s, i + 1, RLENGTH)); i += RLENGTH }
        else if (n ~ /[0-9*@#?$!-]/) { out = out placeholder; i++ }
        else out = out c
    }
    return out
}
BEGIN {
    entry_count = split(entries, entry_word, " ")
    for (entry_i = 1; entry_i <= entry_count; entry_i++) {
        entry_eq = index(entry_word[entry_i], "=")
        entry_key[substr(entry_word[entry_i], 1, entry_eq - 1)] = substr(entry_word[entry_i], entry_eq + 1)
    }
}'

# render [dash] [quoted]: stdin, a heredoc body, as bash sends it. With dash 1
# the leading tabs a <<- heredoc drops go first; with quoted 1 the lines stay
# as written, since bash expands nothing in a quoted-delimiter heredoc.
render() {
    awk -v dash="${1:-0}" -v quoted="${2:-0}" -v sentinel="$label_sentinel" -v placeholder="$expansion_placeholder" \
        -v entries="$yaml_entries" "$render_awk"'{ if (dash) sub(/^\t+/, ""); print (quoted ? $0 : render($0)) }'
}

# object_query is the yq program that reads one rendered body. It prints one
# tab-separated line per finding, body and type first:
#   object  a mapping whose apiVersion is a string starting cfgd.io/ or holding
#           expansion_placeholder, at any depth of any document after aliases
#           are resolved: the line of its apiVersion key, its depth (0 at the
#           document root), its apiVersion, kind, the document root's kind, its
#           metadata.name, whether metadata.labels has the run label's key and
#           whether that value is label_sentinel
#   whole   a document root, or an item of a *List, that is a scalar holding
#           expansion_placeholder: its line
# kind and the names are strings or empty; tabs and newlines in them become a
# space. An ownerReferences entry is a reference to an object and is skipped.
# yq collects an empty match into one empty list per document, which the
# length check drops.
# shellcheck disable=SC2016 # a yq program; $doc belongs to yq
object_query='explode(.) as $doc | $doc
| ( ( ..
      | select(tag == "!!map" and (.apiVersion | tag) == "!!str"
          and ((.apiVersion | test("^cfgd\.io/")) or (.apiVersion | contains(strenv(PLACEHOLDER)))))
      | select(path | join("/") | test("(^|/)ownerReferences/[0-9]+$") | not)
      | [filename, "object", (.apiVersion | key | line), (path | length), .apiVersion,
         ((.kind | select(tag == "!!str")) // ""),
         (($doc | select(tag == "!!map") | .kind | select(tag == "!!str")) // ""),
         ((.metadata | select(tag == "!!map") | .name | select(tag == "!!str")) // ""),
         ((.metadata | select(tag == "!!map") | .labels | select(tag == "!!map") | has(strenv(LABEL_KEY))) // false),
         ((.metadata | select(tag == "!!map") | .labels | select(tag == "!!map") | .[strenv(LABEL_KEY)]
           | (tag == "!!str" and . == strenv(SENTINEL))) // false)] ),
    ( (., (select(tag == "!!map" and (.kind | tag) == "!!str" and (.kind | test("List$"))) | .items | select(tag == "!!seq") | .[]))
      | select(tag != "!!map" and tag != "!!seq" and (tostring | contains(strenv(PLACEHOLDER))))
      | [filename, "whole", line] ) )
| select(length > 0) | map(tostring | sub("[\t\n]+"; " ")) | join("\t")'

# yq_error <dir> <body>: the first line of yq's last error, without the
# scratch file name it gives the body.
yq_error() {
    local err
    err="$(head -n1 "$1/yq.err")"
    printf '%s' "${err#"Error: bad file '$2': "}"
}

# run_query <dir> <label key> <body...>: object_query over the bodies, from <dir>.
run_query() {
    local dir="$1" key="$2"
    shift 2
    (cd "$dir" && SENTINEL="$label_sentinel" PLACEHOLDER="$expansion_placeholder" LABEL_KEY="$key" yq -N "$object_query" "$@" 2>"$dir/yq.err")
}

# parse_bodies <dir> <label key>: runs object_query over every body <dir>/index
# names, writing its lines to <dir>/objects and a `body<TAB>error` line for each
# body yq cannot read to <dir>/unparsed. One yq call reads every body; when it
# stops at a body it names, that body is set aside and the call goes on from
# the next, and an error that names no body has each remaining body read on its
# own.
parse_bodies() {
    local dir="$1" key="$2" bodies out bad i b
    : > "$dir/objects"
    : > "$dir/unparsed"
    mapfile -t bodies < <(cut -f1 "$dir/index")
    while [ "${#bodies[@]}" -gt 0 ]; do
        if out="$(run_query "$dir" "$key" "${bodies[@]}")"; then
            [ -z "$out" ] || printf '%s\n' "$out" >> "$dir/objects"
            return 0
        fi
        bad="$(sed -n "1s/^Error: bad file '\([^']*\)'.*/\1/p" "$dir/yq.err")"
        for i in "${!bodies[@]}"; do [ "${bodies[$i]}" != "$bad" ] || break; done
        if [ -z "$bad" ] || [ "${bodies[$i]}" != "$bad" ]; then
            for b in "${bodies[@]}"; do
                if out="$(run_query "$dir" "$key" "$b")"; then
                    [ -z "$out" ] || printf '%s\n' "$out" >> "$dir/objects"
                else
                    printf '%s\t%s\n' "$b" "$(yq_error "$dir" "$b")" >> "$dir/unparsed"
                fi
            done
            return 0
        fi
        # Lines yq printed before it stopped belong to the bodies ahead of the
        # bad one, apart from any of the bad body's own first documents.
        [ -z "$out" ] || awk -F '\t' -v bad="$bad" '$1 != bad' <<<"$out" >> "$dir/objects"
        printf '%s\t%s\n' "$bad" "$(yq_error "$dir" "$bad")" >> "$dir/unparsed"
        bodies=("${bodies[@]:i+1}")
    done
}

# squash_awk defines squash(s, n), shared by the scans that read shell lines.
# shellcheck disable=SC2016 # an awk program; the $ signs belong to awk
squash_awk='
# squash(s, n): shell line n with each quoted span made the word Q, each
# escaped character the letter X and a comment dropped, so a | or < in
# a string is not read as shell. In an ANSI-C span (a single quote
# after an odd run of $), as in a double-quoted one, a backslash
# escapes the next character. The string a bash -c or sh -c runs is
# shell, so it is read as such. A quote still open at the end of the
# line stays open into the next one; a line ending in one backslash
# keeps it. A quoted client stays the word client, so that
# --dry-run="client" reads as --dry-run=client. kept is the line as
# written with only its comment dropped, and mask is kept with each
# quoted character a Q and each escape XX, so an offset in one is the
# same offset in the other.
function squash(s, n,   out, i, c) {
    out = ""; kept = s; mask = ""
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if ((inq == "\"" || inq == "\047" && ansi) && c == "\\") {
            if (i < length(s)) { i++; mask = mask "QQ" } else mask = mask "Q"
            continue
        }
        if (inq != "" && c == inq) {
            if (qbuf == "client") out = substr(out, 1, length(out) - 1) "client"
            inq = ""; mask = mask "Q"; continue
        }
        if (inq == "\047") { qbuf = qbuf c; mask = mask "Q"; continue }
        if (inq == "\"") { qbuf = qbuf c; mask = mask "Q"; continue }
        if (c == "\\") { if (i == length(s)) { out = out c; mask = mask c } else { out = out "X"; mask = mask "XX"; i++ } }
        else if (cq != "" && c == cq) { cq = ""; out = out " ; "; mask = mask " " }
        else if ((c == "\047" || c == "\"") && cq == "" && out ~ /(^|[^A-Za-z0-9_])(ba)?sh[ \t]+-c[ \t]*$/) { cq = c; qat = n; out = out " ; "; mask = mask " " }
        else if (c == "\047" || c == "\"") {
            ansi = match(out, /\$+$/) && RLENGTH % 2
            inq = c; qat = n; qbuf = ""; out = out "Q"; mask = mask "Q"
        }
        else if (c == "#" && (i == 1 || substr(s, i - 1, 1) ~ /[ \t]/)) { kept = substr(s, 1, i - 1); break }
        else { out = out c; mask = mask c }
    }
    return out
}
'

# scan_run_labels <kinds> <dir or file...> reads every file in each dir, and
# each file named, through heredocs.awk. Each heredoc body is read with yq as
# bash would send it: an unquoted one rendered, a quoted one as written. Every
# fact about a cfgd.io object (where it sits, its kind, name and labels, the
# line of its apiVersion) comes from that parse. The YAML entries a body can
# expand come from yaml_entries. It prints one line per finding, tag first:
#   SITE         a heredoc fed to an apply (kubectl apply, create or replace
#                reading stdin, or a wrapper function around one) that holds an
#                operator object
#   FILEDOC      a heredoc fed to a command that does not apply it (cat > a file,
#                there or inside a pod), or captured into a variable when its
#                cfgd.io documents are of kinds the operator does not serve
#   CAPTURED     an operator object in a heredoc captured into a variable, where
#                the scan cannot see whether it reaches the cluster
#   OTHERKIND    a cfgd.io document of a kind the operator does not serve, applied
#                to the cluster
#   UNLABELLED   an operator object whose metadata.labels has no cfgd.io/e2e-run
#   HANDSPELLED  an operator object whose cfgd.io/e2e-run label is not
#                ${E2E_RUN_LABEL_YAML}
#   NESTED       a cfgd.io object below the root of a document, such as a List
#                item, applied or captured; an ownerReferences entry is a
#                reference and does not count
#   QUOTED       an operator object in a quoted-delimiter heredoc, where
#                ${E2E_RUN_LABEL_YAML} cannot expand
#   NOKIND       a cfgd.io object whose kind is missing or not a string
#   EXPANDED     in a heredoc applied or captured, a kind or apiVersion that holds
#                a shell expansion, or a document or List item that is one
#   CONTINUED    a line of an unquoted heredoc applied or captured that ends in
#                an odd number of backslashes, which bash joins to the next line
#   UNPARSED     a heredoc fed to the cluster or captured that yq cannot read
#                as YAML once rendered
#   OUTSIDE      a cfgd.io apiVersion on a shell line outside any heredoc
#   REDEFINED    a script other than helpers.sh that sets E2E_RUN_LABEL_YAML
#   BYPATH       an apply the scan cannot read: one given a manifest by path
#                (-f other than -, or -k), or one reading stdin fed by anything
#                but a heredoc on the apply command itself
#   DUPLICATE    one function name defined with two different bodies; every
#                function a scanned file defines is visible to all of them, and
#                the tree check scans each tests/e2e/*/scripts directory and
#                helpers.sh
#   UNTERMINATED, UNREADABLE, EMPTY   the scan could not read what it was given
# A heredoc written to a file is read only when it mentions cfgd.io/, and one
# yq cannot read (a script, say) is not YAML and is skipped quietly.
scan_run_labels() {
    local kinds="$1" dir f files=() found work label_key=""
    shift
    for dir in "$@"; do
        if [ -f "$dir" ]; then files+=("$dir"); continue; fi
        found=("$dir"/*)
        if [ ! -e "${found[0]}" ] && [ ! -L "${found[0]}" ]; then
            echo "EMPTY $dir: no files to scan"
            continue
        fi
        for f in "${found[@]}"; do
            if [ -f "$f" ] && [ -r "$f" ]; then files+=("$f"); else echo "UNREADABLE $f"; fi
        done
    done
    [ "${#files[@]}" -gt 0 ] || return 0
    [[ " $yaml_entries" =~ \ E2E_RUN_LABEL_YAML=([^ ]+) ]] && label_key="${BASH_REMATCH[1]}"
    if [ -z "$label_key" ]; then
        echo "UNREADABLE helpers.sh has no export E2E_RUN_LABEL_YAML=\"key: ...\" line (its YAML entries: ${yaml_entries:-none})"
        return 0
    fi
    work="$(mktemp -d "$scratch/scan.XXXXXX")" || { echo "UNREADABLE no scratch directory for the scan"; return 0; }
    : > "$work/index"
    awk -f "$here/heredocs.awk" "${files[@]}" > "$work/records" || echo "UNREADABLE heredocs.awk exited $? reading ${files[*]}"
    awk -F '\t' -v work="$work" -v sentinel="$label_sentinel" -v placeholder="$expansion_placeholder" \
        -v entries="$yaml_entries" -v helpers="$helpers" "$render_awk$squash_awk"'
        function rest(n,   i, p) { p = 0; for (i = 1; i <= n; i++) p += length($i) + 1; return substr($0, p + 1) }
        # runs_apply(w, v, cw): the verb w[v] belongs to a command that sends
        # manifests to a cluster. The word that runs it is the nearest one
        # before the verb that is not an option or an option argument: kubectl,
        # a variable or array expansion ($KUBECTL, "${kc[@]}") or a function
        # the scanned scripts define, standing as the command word w[cw], is a
        # kubectl; any other word names another
        # tool (cfgd, helm, git) whose apply is not one. kubectl anywhere before
        # the verb settles it, so sudo -E kubectl is a kubectl. The command
        # word is never an option argument, so k in time -p k is the runner.
        function runs_apply(w, v, cw,   i) {
            for (i = v - 1; i >= 1; i--) {
                if (w[i] == "") continue
                if (w[i] ~ /(^|\/)kubectl$/) return 1
                if (w[i] ~ /^-/) continue
                if (i == cw) return w[i] ~ /^(Q|\$)/ || (w[i] in defined)
                if (i > 1 && w[i - 1] ~ /^-[^=]*$/) { i--; continue }
                return w[i] ~ /^(Q|\$)/
            }
            return 0
        }
        # judge(c): the apply commands on the squashed command line c. A command
        # holding apply, create or replace and a -f, --filename, -k or
        # --kustomize argument is an apply when runs_apply says so, and a call
        # of a wrapper (a function whose body is an apply reading stdin) is an
        # apply reading stdin. A call is a function name standing as the
        # command word, which command_word finds past assignments, redirects
        # and keywords such as time; the same name as an argument is no call.
        # One with --dry-run=client sends nothing and is left alone. The scan
        # can read one shape: an apply reading stdin from a heredoc on the
        # same command. A manifest by path (-f other than -,
        # or -k) sets verdict to "path". An apply reading stdin with no heredoc
        # on its own command (fed by a pipe, or by nothing on the line), or with
        # a < beside it (a redirect, a here-string, a process substitution),
        # sets it to "stdin", apart from one with no feeder at all inside a
        # function body, which makes the function a wrapper and sets bare.
        # fed counts the applies fed a heredoc, whose body reaches the cluster.
        # A here-string (<<<) keeps a < once its heredoc token is made, so it
        # counts as a redirect. A heredoc on a descriptor other than 0 (3<<EOF)
        # is no stdin. Inside a function body, a call of another function a
        # scanned script defines with no feeder is recorded, as the caller is
        # a wrapper when the callee is one.
        function judge(c,   lists, nl, l, segs, ns, j, w, n, i, v, val, stdin, path, d, cw) {
            verdict = ""; fed = 0; bare = 0
            gsub(/[0-9]*[<>]&[0-9-]*/, " ", c); gsub(/&>/, " >", c)
            while (match(c, /(^|[ \t])[0-9]+<<-?[ \t]*X?(Q|[A-Za-z_][A-Za-z0-9_]*)/)) {
                d = substr(c, RSTART, RLENGTH)
                if (d ~ /^[ \t]*0+<</) c = substr(c, 1, RSTART - 1) " " substr(c, RSTART + index(d, "<") - 1)
                else c = substr(c, 1, RSTART - 1) " FDDOC " substr(c, RSTART + RLENGTH)
            }
            gsub(/<<-?[ \t]*X?(Q|[A-Za-z_][A-Za-z0-9_]*)/, " HEREDOC ", c)
            gsub(/\|\||&&/, ";", c)
            nl = split(c, lists, /[;&(){}`]/)
            for (l = 1; l <= nl; l++) {
                ns = split(lists[l], segs, "|")
                for (j = 1; j <= ns; j++) {
                    if (segs[j] ~ /--dry-run=client/) continue
                    n = split(segs[j], w, /[ \t]+/)
                    stdin = 0; path = 0
                    cw = command_word(w, n)
                    if (pass == 2 && fns && j == 1 && segs[j] !~ /HEREDOC/ && !index(segs[j], "<") && cw && (w[cw] in defined))
                        { ++calls; caller[calls] = fn[fns]; callee[calls] = w[cw] }
                    if (cw && (w[cw] in wrapper)) stdin = 1
                    for (v = 1; v <= n; v++) if (w[v] ~ /^(apply|create|replace)$/ && runs_apply(w, v, cw)) break
                    for (i = v + 1; i <= n; i++) {
                        if (w[i] ~ /^(-k|--kustomize)/) { path = 1; continue }
                        if (w[i] == "-f" || w[i] == "--filename") val = w[++i]
                        else if (w[i] ~ /^--filename=/) val = substr(w[i], 12)
                        else if (w[i] ~ /^-f/ && w[i] !~ /^--/) { val = substr(w[i], 3); sub(/^=/, "", val) }
                        else continue
                        if (val == "-") stdin = 1; else path = 1
                    }
                    if (path) verdict = "path"
                    if (!stdin) continue
                    if (segs[j] ~ /HEREDOC/) { fed++; if (!index(segs[j], "<")) continue }
                    if (fns && j == 1 && !index(segs[j], "<")) { bare = 1; continue }
                    if (verdict == "") verdict = "stdin"
                }
            }
        }
        # position(tok, st): the state after the word tok, where st is 1 while
        # the next word stands where bash reads a command word, 3 after time,
        # whose options (-p, --) keep that position, 2 when the next word is
        # the target of a redirect standing there, and 0 inside arguments. A
        # separator, a keyword that starts a command, a reserved { or }, an
        # assignment and a redirect all leave the next word at that position.
        function position(tok, st) {
            if (tok ~ /^[;()]$/) return 1
            if (st == 3 && tok ~ /^-/) return 3
            if (st == 3) st = 1
            if (st != 1) return st == 2
            if (tok == "time") return 3
            if (tok ~ /^(!|if|then|else|elif|do|while|until|coproc|\{|\}|HEREDOC|FDDOC)$/) return 1
            if (tok ~ /^[A-Za-z_][A-Za-z0-9_]*(\[[^]]*\])?\+?=/) return 1
            if (tok ~ /^[0-9]*[<>]/) return (tok ~ /^[0-9]*[<>]+&?$/) ? 2 : 1
            return 0
        }
        # regex_escape(s): s with each character a regular expression gives a
        # meaning to taken literally, as in a function named a.b.
        function regex_escape(s) { gsub(/[][\\.^$*+?(){}|\/]/, "\\\\&", s); return s }
        # at_command(st): the next word stands at a command-word position.
        function at_command(st) { return st == 1 || st == 3 }
        # command_word(w, n): the index of the command word among the words
        # w[1..n] of one simple command, or 0 when it has none.
        function command_word(w, n,   v, st) {
            st = 1
            for (v = 1; v <= n; v++) {
                if (w[v] == "") continue
                if (at_command(st) && !position(w[v], st)) return v
                st = position(w[v], st)
            }
            return 0
        }
        # words(s): reads the command line s at its command words. Bash takes
        # NAME() as a definition only at a command word, and function NAME at
        # one is checked here. A NAME is any word bash takes for one: no $
        # (a command substitution), = (an assignment) or < > (a process
        # substitution); one holding { or } is put in badname[1..bads], as
        # judge splits a command there and could not see its calls. It sets ob
        # and cb to the { and } that are reserved words (echo { opens
        # nothing), op and cp to its ( and ), and defs to the functions it
        # defines, each defname[d] with the { or ( that opens its body in
        # defkind[d], empty when the body opens on a later line, and the brace
        # and paren depth the line has reached at it in defat[d], defpat[d].
        function words(s,   t, n, i, st, w, nw) {
            gsub(/\([ \t]*\)/, " FNDEF ", s)
            # A } before a redirect (}>f) is a word of its own, as < and > end
            # a word.
            while (match(s, /[{}][<>]/)) s = substr(s, 1, RSTART) " " substr(s, RSTART + 1)
            gsub(/[;&|`]/, " ; ", s); gsub(/\(/, " ( ", s); gsub(/\)/, " ) ", s)
            n = split(s, t, /[ \t]+/); nw = 0
            for (i = 1; i <= n; i++) if (t[i] != "") w[++nw] = t[i]
            st = 1; ob = 0; cb = 0; op = 0; cp = 0; defs = 0; bads = 0
            for (i = 1; i <= nw; i++) {
                if (w[i] == "(") op++
                if (w[i] == ")") cp++
                if (at_command(st) && w[i] == "{") ob++
                if (at_command(st) && w[i] == "}") cb++
                if (at_command(st) && w[i] == "function" && w[i + 1] !~ /^(FNDEF|[;(){}])?$/) i++
                else if (!(w[i + 1] == "FNDEF" && w[i] !~ /[$=<>]/)) { st = position(w[i], st); continue }
                if (w[i] ~ /[{}]/) { badname[++bads] = w[i]; if (w[i + 1] == "FNDEF") i++; continue }
                defname[++defs] = w[i]; defkind[defs] = ""; defat[defs] = ob - cb; defpat[defs] = op - cp
                if (w[i + 1] == "FNDEF") i++
                if (w[i + 1] ~ /^[({]$/) defkind[defs] = w[i + 1]
            }
            first = w[1]
        }
        # body_done(k): the function on stack slot k has closed. A name another
        # scanned script defines with another body fails, as every definition
        # is visible to every script. A body is compared as bash runs it, word
        # by word: its comments are gone, a newline is a ;, and a ; that ends
        # nothing (one after another, after an opening { or (, or before a
        # closing one) is dropped.
        function body_done(k,   t, w, n, i, tk, m) {
            t = fnbody[k]
            gsub(/\n/, ";", t); gsub(/[;{}()]/, " & ", t)
            n = split(t, w, /[ \t]+/); m = 0
            for (i = 1; i <= n; i++) if (w[i] != "") tk[++m] = w[i]
            t = " ;"
            for (i = 1; i <= m; i++) {
                if (tk[i] == ";" && t ~ / [;{(]$/) continue
                if (tk[i] ~ /^[})]$/) sub(/ ;$/, "", t)
                t = t " " tk[i]
            }
            if (!(fn[k] in body_of)) { body_of[fn[k]] = t; body_at[fn[k]] = file ":" fnline[k]; return }
            if (body_of[fn[k]] != t) print "DUPLICATE " file ":" fnline[k] ": function " fn[k] " is also defined at " body_at[fn[k]] " with another body; every script sees every definition, so rename one"
        }
        # check_command: judges the command line gathered so far, keeping track
        # of the function bodies it opens and closes. A body is a { } or a
        # ( ) group, counted on the line with its quoted text dropped, and a
        # definition is one words() finds at a command word.
        function check_command(   c, raw, masked, l, k, d, top, e) {
            if (pending == "") return
            c = pending; pending = ""; top = fns; raw = pending_kept; pending_kept = ""
            masked = pending_mask; pending_mask = ""
            for (k = 1; k <= fns; k++) fnbody[k] = fnbody[k] "\n" raw
            gsub(/\$\{[^}]*\}/, "$V", c)
            # A clobber redirect (>|) is no pipe.
            gsub(/>\|/, ">", c)
            words(c)
            if (pass == 3) for (d = 1; d <= bads; d++) print "UNREADABLE " file ":" pending_at ": a function name the scan does not read: " badname[d]
            for (d = 1; d <= defs; d++) {
                if (pass == 1) defined[defname[d]] = 1
                fn[++fns] = defname[d]; fnline[fns] = pending_at; fnat[fns] = depth + defat[d]; fnpat[fns] = pdepth + defpat[d]
                fnkind[fns] = defkind[d]; fnopen[fns] = 0
                # A body is compared from the end of its opener, so function f {
                # and f() { open the same body. The opener is matched on the
                # line with its quoted text masked, so an "f()" in a string
                # before it is no opener, and after a character that ends a
                # word for words(); the text starts with a newline.
                e = regex_escape(defname[d]); fnbody[fns] = raw
                if (match(masked, "[ \t\n;&|()`](function[ \t]+" e "([ \t]*\\(\\))?|" e "[ \t]*\\(\\))"))
                    fnbody[fns] = substr(raw, RSTART + RLENGTH)
            }
            if (fns && fnkind[fns] == "" && first ~ /^[({]$/) fnkind[fns] = first
            judge(c)
            if (pass == 2 && bare) wrapper[fn[fns]] = 1
            if (pass == 3) {
                if (verdict == "path") print "BYPATH " file ":" pending_at ": the scan cannot read a manifest applied by path; apply it from a heredoc"
                if (verdict == "stdin") print "BYPATH " file ":" pending_at ": the scan cannot read what feeds this apply on stdin; feed it a heredoc on the apply command itself"
                if (fed) for (l = pending_at; l <= pending_last; l++) cluster_line[file, l] = 1
            }
            depth += ob - cb; pdepth += op - cp
            for (k = top + 1; k <= fns; k++) if (fnkind[k] == "{" ? ob : op) fnopen[k] = 1
            if (top && top == fns && (fnkind[fns] == "{" ? ob : op)) fnopen[fns] = 1
            while (fns && fnopen[fns] && (fnkind[fns] == "{" ? depth <= fnat[fns] : pdepth <= fnpat[fns])) {
                if (pass == 2) body_done(fns)
                fns--
            }
        }
        # close_wrappers: a function whose body calls a wrapper with no feeder
        # is a wrapper too, whatever order the two are defined in.
        function close_wrappers(   k, added) {
            do {
                added = 0
                for (k = 1; k <= calls; k++) {
                    if (caller[k] in wrapper || !(callee[k] in wrapper)) continue
                    wrapper[caller[k]] = 1; added = 1
                }
            } while (added)
        }
        function close_heredoc(id,   i, n, lead, yaml, path, out) {
            n = count[id]
            yaml = (cls[id] != "FILE")
            for (i = 1; i <= n && !yaml; i++) yaml = index(text[id, i], "cfgd.io/") > 0
            if (!yaml) return
            for (i = 1; i <= n; i++) out[i] = qtd[id] ? text[id, i] : render(text[id, i])
            # yq numbers lines from the first content line of a file, so the
            # blank, comment and --- lines before it are left out and counted.
            for (lead = 0; lead < n && out[lead + 1] ~ /^([ \t]*(#.*)?|---([ \t]+#.*)?[ \t]*)$/; lead++) continue
            if (lead == n) return
            path = work "/" (++bodies) ".yaml"
            for (i = lead + 1; i <= n; i++) print out[i] > path
            close(path)
            print bodies ".yaml", file, opened[id], cls[id], qtd[id], at[id, lead + 1] > (work "/index")
        }
        # The records are read four times: for the functions the scripts
        # define, for the wrappers among them, for the commands, and for the
        # heredocs, whose class rests on the command that opens them. A body
        # still open when a file ends leaves the scan unable to tell what is
        # inside a function, and a quote still open leaves it unable to tell
        # which lines are commands, so both fail.
        # apply_yaml stays a wrapper when the script defining it is not among
        # those scanned, so a heredoc fed to it is still judged.
        BEGIN { OFS = "\t"; wrapper["apply_yaml"] = 1 }
        $1 == "FILE" {
            check_command()
            if (pass == 2 && fns) print "UNREADABLE " file ":" fnline[1] ": a function body opened at line " fnline[1] " never closes for the scan"
            if (pass == 3 && (inq != "" || cq != "")) print "UNREADABLE " file ":" qat ": a quote opened at line " qat " never closes for the scan"
            if (FNR == 1 && ++pass == 3) close_wrappers()
            inq = ""; cq = ""; depth = 0; pdepth = 0; fns = 0
            next
        }
        { file = $2 }
        pass < 4 && $1 == "CLOSE" {
            # A quote open at the end of the line that opens a heredoc
            # closes on its terminator line, as in a bash -c string holding
            # the heredoc.
            if (inq != "" || cq != "") { inq = ""; cq = ""; check_command() }
            next
        }
        pass < 4 && $1 == "SH" {
            line = rest(3)
            if (pass == 3 && line !~ /^[ \t]*#/) {
                sub(/[ \t]+#.*$/, "", line)
                if (line ~ /(^|[^A-Za-z0-9_])["\047]?apiVersion["\047]?[ \t]*:[ \t]*["\047]?cfgd\.io\//) print "OUTSIDE " file ":" $3 ": a cfgd.io apiVersion outside any heredoc"
                if (file != helpers) {
                    # Reads of the variable go first; a name left over is set.
                    gsub(/\$E2E_RUN_LABEL_YAML([^A-Za-z0-9_]|$)|\$\{#?E2E_RUN_LABEL_YAML(\}|[-+?\/#%^,][^}]*\}|:[-+?0-9 ][^}]*\})/, " ", line)
                    if (line ~ /(^|[^A-Za-z0-9_])E2E_RUN_LABEL_YAML([^A-Za-z0-9_]|$)/) print "REDEFINED " file ":" $3 ": only helpers.sh sets E2E_RUN_LABEL_YAML; a script that sets it labels its objects with a value the scan cannot check"
                }
            }
            if (pending == "") pending_at = $3
            pending_last = $3
            pending = pending squash(rest(3), $3)
            if (pass == 3 && length(mask) != length(kept)) print "UNREADABLE " file ":" $3 ": the scan lost its place in the quotes on this line"
            pending_kept = pending_kept "\n" kept
            pending_mask = pending_mask "\n" mask
            # bash goes on reading a command past a line ending in one
            # backslash, a pipe or && or inside a quote.
            if (inq != "" || cq != "" || pending ~ /\\$/ || pending ~ /(\||&&)[ \t]*$/) { sub(/\\$/, "", pending); pending = pending " "; next }
            check_command()
            next
        }
        pass == 2 && $1 == "BODY" { for (k = 1; k <= fns; k++) fnbody[k] = fnbody[k] "\n" rest(4); next }
        pass < 4 { next }
        $1 == "UNCLOSED" { print "UNTERMINATED " file ":" $3 ": no " $4 " line closes this heredoc"; next }
        $1 == "OPEN" {
            id = $4; opened[id] = $3; qtd[id] = $6; dash[id] = $7; count[id] = 0
            c = rest(7)
            if (cluster_line[file, $3]) cls[id] = "CLUSTER"
            else {
                # A descriptor duplication such as 2>&1 writes no file.
                gsub(/[0-9]*>&[0-9-]+/, "", c)
                cls[id] = (c ~ /\$\(/ && c !~ />/) ? "CAPTURE" : "FILE"
            }
            next
        }
        $1 == "BODY" {
            id = $4; line = rest(4)
            if (dash[id]) sub(/^\t+/, "", line)
            if (!qtd[id] && cls[id] != "FILE" && match(line, /\\+$/) && RLENGTH % 2 == 1) print "CONTINUED " file ":" $3 ": a line continued with \\ inside a heredoc; bash joins it to the next line"
            text[id, ++count[id]] = line; at[id, count[id]] = $3
            next
        }
        $1 == "CLOSE" { close_heredoc($4); next }
        END { close(work "/index") }
    ' "$work/records" "$work/records" "$work/records" "$work/records" || echo "UNREADABLE the scan's heredoc collector exited $?"
    parse_bodies "$work" "$label_key"
    awk -F '\t' -v kinds="$(tr '\n' ' ' <<<"$kinds")" -v placeholder="$expansion_placeholder" '
        BEGIN { n = split(kinds, k, " "); for (i = 1; i <= n; i++) operator[k[i]] = 1 }
        FILENAME == ARGV[1] { src[$1] = $2; opened[$1] = $3; cls[$1] = $4; qtd[$1] = $5; first[$1] = $6; order[++bodies] = $1; next }
        FILENAME == ARGV[3] {
            if (cls[$1] == "FILE") next
            err = $2
            # yq counts lines from the body; the message names the script line.
            if (match(err, /at L[0-9]+[^:]*/)) err = substr(err, 1, RSTART - 1) "at line " (first[$1] + substr(err, RSTART + 4, index(substr(err, RSTART + 4), ".") - 1) - 1) substr(err, RSTART + RLENGTH)
            print "UNPARSED " src[$1] ":" opened[$1] ": yq cannot read this heredoc as YAML once its variables expand (" err "); make it valid YAML, with each $VAR, ${...} and $(...) inside a value, since the scan reads each as one plain word"
            next
        }
        {
            b = $1; at = src[b] ":" (first[b] + $3 - 1)
            if ($2 == "whole") {
                if (cls[b] != "FILE") { print "EXPANDED " at ": a heredoc document that is a whole expansion; the scan cannot read what it applies"; reported[b] = 1 }
                next
            }
            api = $5; kind = $6; root = $7; name = $8
            sub(/ +$/, "", kind); sub(/ +$/, "", root)
            if (cls[b] == "FILE") { if (api ~ /^cfgd\.io\//) cfgd[b] = 1; next }
            if (index(api, placeholder)) { print "EXPANDED " at ": apiVersion is a shell expansion; write it literally"; reported[b] = 1; next }
            cfgd[b] = 1
            if ($4 > 0) {
                print "NESTED " at ": a cfgd.io object nested inside " (root == "" ? "another document" : root) "; apply it as its own document"
                reported[b] = 1
                next
            }
            if (index(kind, placeholder)) { print "EXPANDED " at ": kind is a shell expansion; write it literally"; reported[b] = 1; next }
            if (cls[b] == "CAPTURE") {
                if (kind in operator) {
                    print "CAPTURED " at " " kind ": the scan cannot see where a captured heredoc goes; feed it to kubectl apply directly"
                    reported[b] = 1
                }
                next
            }
            if (kind == "") { print "NOKIND " at ": a cfgd.io object whose kind is missing or not a string"; next }
            if (!(kind in operator)) { print "OTHERKIND " at " " kind; next }
            site[b] = 1
            if (qtd[b]) print "QUOTED " at " " kind ": the heredoc delimiter is quoted, so ${E2E_RUN_LABEL_YAML} cannot expand"
            else if ($9 == "true" && $10 != "true") print "HANDSPELLED " at " " kind " " name ": spell the label as ${E2E_RUN_LABEL_YAML}"
            else if ($9 != "true") print "UNLABELLED " at " " kind " " name ": metadata.labels has no ${E2E_RUN_LABEL_YAML}"
        }
        END {
            for (i = 1; i <= bodies; i++) {
                b = order[i]
                if (site[b]) print "SITE " src[b] ":" opened[b]
                if (cfgd[b] && (cls[b] == "FILE" || (cls[b] == "CAPTURE" && !reported[b]))) print "FILEDOC " src[b] ":" opened[b]
            }
        }
    ' "$work/index" "$work/objects" "$work/unparsed" || echo "UNREADABLE the scan's awk exited $?"
    rm -rf "$work"
}

# Suites that apply operator objects today, each with a floor about two thirds
# of its current count, so one suite losing its sites fails on its own.
run_label_suites=(operator full-stack gateway)
run_label_floors=(36 15 3)

# label_dirs <root>: every <root>/*/scripts directory plus each floored suite's,
# so a floored suite that is missing fails as EMPTY.
label_dirs() {
    local root="$1" suite
    {
        printf '%s\n' "$root"/*/scripts
        for suite in "${run_label_suites[@]}"; do printf '%s\n' "$root/$suite/scripts"; done
    } | sort -u
}

# by_path_in_scope <root> <scan output>: the BYPATH lines from the floored
# suites and helpers.sh, the scripts that apply operator objects.
by_path_in_scope() {
    local tag where suite
    while read -r tag where; do
        [ "$tag" = BYPATH ] || continue
        for suite in "${run_label_suites[@]}"; do
            if [[ "$where" == "$1/$suite/scripts/"* ]]; then echo "$tag $where"; continue 2; fi
        done
        if [[ "$where" == "$helpers:"* ]]; then echo "$tag $where"; fi
    done <<<"$2"
}

# label_verdict <root> <scan output>: prints nothing when the scan is clean and
# each floored suite holds its floor, otherwise one line per problem.
label_verdict() {
    local root="$1" out="$2" i suite floor sites
    grep -Ev '^(SITE|FILEDOC|OTHERKIND|BYPATH) ' <<<"$out" || true
    by_path_in_scope "$root" "$out"
    for i in "${!run_label_suites[@]}"; do
        suite="${run_label_suites[$i]}" floor="${run_label_floors[$i]}"
        sites="$(grep -c "^SITE $root/$suite/scripts/" <<<"$out" || true)"
        if [ "$sites" -lt "$floor" ]; then
            echo "FLOOR $suite: only $sites heredocs apply an operator object (want at least $floor); the scan has lost its population"
        fi
    done
}

kinds="$(operator_kinds "$repo_root/schemas/crds.yaml")"
if [[ "$kinds" == FAIL* ]]; then
    fail "${kinds#FAIL }"
else
    pass "the operator serves $(tr '\n' ' ' <<<"$kinds" | sed 's/ $//') (schemas/crds.yaml)"
fi
mapfile -t label_scan_dirs < <(label_dirs "$e2e_root")
tree_scan="$(scan_run_labels "$kinds" "${label_scan_dirs[@]}" "$helpers")"
tree_verdict="$(label_verdict "$e2e_root" "$tree_scan")"
if [ -z "$tree_verdict" ]; then
    pass "every operator object the e2e suites apply carries the run label ($(grep -c '^SITE ' <<<"$tree_scan") heredocs: $(for s in "${run_label_suites[@]}"; do printf '%s %s, ' "$s" "$(grep -c "^SITE $e2e_root/$s/scripts/" <<<"$tree_scan")"; done | sed 's/, $//'))"
else
    fail "operator objects the PR operator would ignore:"
    printf '      %s\n' "${tree_verdict//$'\n'/$'\n'      }"
fi
others="$(grep '^OTHERKIND ' <<<"$tree_scan" | cut -d' ' -f2- | sed "s|$e2e_root/||" | paste -sd ';' - | sed 's/;/; /g' || true)"
[ -z "$others" ] || pass "cfgd.io objects outside the operator's watch, so no run label: $others"
by_path="$(grep '^BYPATH ' <<<"$tree_scan" | grep -vxFf <(by_path_in_scope "$e2e_root" "$tree_scan") | cut -d' ' -f2 | sed "s|$e2e_root/||; s|:\$||" | paste -sd ';' - | sed 's/;/; /g' || true)"
[ -z "$by_path" ] || pass "manifests applied by path outside the suites that apply operator objects: $by_path"
pass "$(grep -c '^FILEDOC ' <<<"$tree_scan" || true) heredocs hold a cfgd.io document that needs no run label: one fed to a command that does not apply it (cat > a file, there or inside a pod), or a captured document of a kind the operator does not serve"

# The scan against planted fixtures: one per way a suite writes a cfgd.io
# document, one per place a cfgd.io apiVersion can sit, one per placement of
# the label and one per YAML spelling of a key or value. Each fixture is
# checked as shell (bash -n) and its body as YAML, rendered by the scan's own
# render, before the scan reads it.
fixtures="$scratch/label-fixtures"
mkdir -p "$fixtures/scripts"
# plant <name> <opener> <body> [closer]: a fixture whose rendered body is YAML.
# plant_unparsed takes the same arguments for one whose rendered body yq must
# refuse, so the fixture stays invalid if render changes.
plant() { plant_as yaml "$@"; }
plant_unparsed() { plant_as not-yaml "$@"; }
plant_as() {
    local want="$1" name="$2" opener="$3" body="$4" closer="${5-EOF}" f checked dash=0 quoted=0
    f="$fixtures/scripts/$name.sh"
    printf '%s\n%s\n%s\n' "$opener" "$body" "$closer" > "$f"
    checked="$(bash -n "$f" 2>&1 | grep -v 'delimited by end-of-file' || true)"
    [ -z "$checked" ] || fail "fixture $name is not valid shell: $checked"
    [[ "$opener" != *'<<-'* ]] || dash=1
    [[ ! "$opener" =~ \<\<-?[[:space:]]*[\'\"\\] ]] || quoted=1
    if checked="$(printf '%s\n' "$body" | render "$dash" "$quoted" | yq '.' 2>&1 >/dev/null)"; then
        [ "$want" = yaml ] || fail "fixture $name was meant to be invalid YAML once rendered, and yq read it"
    else
        [ "$want" = not-yaml ] || fail "fixture $name is not valid YAML: $checked"
    fi
}
module_labelled="apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: labelled
  labels:
    app.kubernetes.io/part-of: e2e
    \${E2E_RUN_LABEL_YAML}
spec:
  packages: []"
module_unlabelled='apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: unlabelled
spec:
  packages: []'
# module_labels <line...>: a Module whose labels block holds exactly the lines.
module_labels() {
    printf 'apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\n  labels:\n'
    printf '    %s\n' "$@"
    printf 'spec:\n  packages: []'
}
apply="kubectl apply -n \"\$E2E_NAMESPACE\" -f - <<EOF"

plant labelled "$apply" "$module_labelled"
plant label-absent "$apply" "$(module_labels 'app.kubernetes.io/part-of: e2e')"
plant no-labels "$apply" "$module_unlabelled"
plant quoted-kind "$apply" "apiVersion: \"cfgd.io/v1alpha1\"
kind: 'Module' # the module
metadata:
  name: quoted-kind
spec:
  packages: []"
plant multi-doc "$apply" "$module_labelled
---
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: second
spec:
  hostname: second
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: third
data:
  k: v"
plant nested-metadata "$apply" "apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: nested
spec:
  template:
    metadata:
      labels:
        \${E2E_RUN_LABEL_YAML}"
plant comment "# apiVersion: cfgd.io/v1alpha1 in a shell comment"$'\n'"kubectl apply -f - <<EOF" '# apiVersion: cfgd.io/v1alpha1
apiVersion: v1
kind: ConfigMap
metadata:
  name: comment
data:
  k: v'
plant comment-heredoc "# Usage: apply_yaml \"T03\" <<'EOF' ... EOF"$'\n'"kubectl apply -f - <<EOF" "$module_unlabelled"
plant rc-ok-comment "kubectl apply -n \"\$E2E_NAMESPACE\" -f - 2>&1 <<EOF || true # rc-ok: read back with kubectl exec below" "$module_unlabelled"
plant quoted "kubectl apply -f - <<'EOF'" "$module_unlabelled"
plant in-pod "exec_in_pod bash -c 'cat > /etc/cfgd/in-pod.yaml << \"INNEREOF\"" 'apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: in-pod
spec:
  profile: base' "INNEREOF'"
plant exec-apply "exec_in_pod kubectl apply -f - <<EOF" "$module_unlabelled"
plant apply-yaml "apply_yaml \"T01\" <<EOF" "$module_unlabelled"
plant captured "RESULT=\$(kubectl apply -f - 2>&1 <<EOF || true" "$module_unlabelled" "EOF"$'\n'")"
plant continued "kubectl apply -n \"\$E2E_NAMESPACE\" \\"$'\n'"    -f - <<EOF" "$module_unlabelled"
plant dash "kubectl apply -f - <<-EOF" $'\t'"${module_unlabelled//$'\n'/$'\n\t'}" $'\tEOF'
plant captured-operator-kind "yaml=\$(cat <<EOF" 'apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: captured
spec:
  hostname: captured' "EOF"$'\n'")"$'\n'"echo \"\$yaml\" | kubectl apply -f -"
machine_config='apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: captured
spec:
  hostname: captured'
plant captured-dup "yaml=\$(cat 2>&1 <<EOF" "$machine_config" "EOF"$'\n'")"
plant captured-to-file "x=\$(cat <<EOF > f" "$machine_config" "EOF"$'\n'")"
plant captured-list "yaml=\$(cat <<EOF" 'apiVersion: v1
kind: List
items:
  - apiVersion: cfgd.io/v1alpha1
    kind: MachineConfig
    metadata:
      name: first
    spec:
      hostname: first
  - apiVersion: cfgd.io/v1alpha1
    kind: MachineConfig
    metadata:
      name: second
    spec:
      hostname: second' "EOF"$'\n'")"
plant captured-other-kind "yaml=\$(cat <<EOF" 'apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: captured
spec:
  team: a' "EOF"$'\n'")"
plant owner-reference "kubectl apply -f - <<EOF" 'apiVersion: v1
kind: ConfigMap
metadata:
  name: owned
  ownerReferences:
  - apiVersion: cfgd.io/v1alpha1
    kind: MachineConfig
    name: owner
    uid: 00000000-0000-0000-0000-000000000000
data:
  k: v'
plant owner-reference-indented "kubectl apply -f - <<EOF" 'apiVersion: v1
kind: ConfigMap
metadata:
  name: owned
  ownerReferences:
    - apiVersion: cfgd.io/v1alpha1
      kind: MachineConfig
      name: owner
      uid: 00000000-0000-0000-0000-000000000000
data:
  k: v'
plant file-operator-kind "cat > \"\$dir/mc.yaml\" <<EOF" 'apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: in-a-file
spec:
  hostname: in-a-file'
plant other-kind "kubectl apply -f - <<EOF" 'apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: team
spec:
  team: a'
plant list "kubectl apply -f - <<EOF" 'apiVersion: v1
kind: List
items:
  - apiVersion: cfgd.io/v1alpha1
    kind: Module
    metadata:
      name: listed
    spec:
      packages: []'
plant labels-comment "$apply" "apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: labels-comment
  labels: {app: x}  # want \${E2E_RUN_LABEL_YAML} here
spec:
  packages: []"
plant entry-comment "$apply" "$(module_labels "app: x  # want \${E2E_RUN_LABEL_YAML} here")"
plant key-prefix "$apply" "$(module_labels 'cfgd.io/e2e-run-foo: "42"')"
plant var-prefix "$apply" "$(module_labels "\${E2E_RUN_LABEL_YAML_OLD}")"
plant default-label "$apply" "$(module_labels "\${LABEL:-\${E2E_RUN_LABEL_YAML}}")"
plant job-label-only "$apply" "$(module_labels "\${E2E_JOB_LABEL_YAML}")"
plant hand-spelled "$apply" "$(module_labels "cfgd.io/e2e-run: \"\${E2E_RUN_ID}\"")"
plant flow-labels "$apply" "apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: flow-labels
  labels: {app: x, \${E2E_RUN_LABEL_YAML}}
spec:
  packages: []"
plant flow-metadata "$apply" "apiVersion: cfgd.io/v1alpha1
kind: Module
metadata: {name: flow, labels: {\${E2E_RUN_LABEL_YAML}}}
spec:
  packages: []"
plant flow-doc "$apply" "{apiVersion: cfgd.io/v1alpha1, kind: Module, metadata: {name: flow, labels: {\${E2E_RUN_LABEL_YAML}}}}"
plant json-doc "$apply" '{
  "apiVersion": "cfgd.io/v1alpha1",
  "kind": "Module",
  "metadata": {"name": "json"}
}'
plant json-not-cfgd "$apply" '{
  "apiVersion": "v1",
  "kind": "ConfigMap",
  "metadata": {"name": "json-not-cfgd"}
}'
plant flow-not-cfgd "$apply" "{apiVersion: v1, kind: ConfigMap, metadata: {name: flow-not-cfgd}}"
plant spaced-colon "$apply" "apiVersion : cfgd.io/v1alpha1
kind: Module
metadata:
  name: spaced-colon"
plant flow-multiline "$apply" "{
  apiVersion: cfgd.io/v1alpha1,
  kind: Module,
  metadata: {name: flow-multiline}
}"
plant spaced-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind : Module
metadata:
  name: spaced-kind
spec:
  packages: []"
plant no-kind "$apply" "apiVersion: cfgd.io/v1alpha1
metadata:
  name: no-kind"
plant suffix-key "$apply" "apiVersion: v1
kind: ConfigMap
metadata:
  name: suffix-key
data:
  myapiVersion: cfgd.io/v1alpha1"
plant tagged-value "$apply" "apiVersion: !!str cfgd.io/v1alpha1
kind: Module
metadata:
  name: tagged-value"
plant split-key "$apply" "apiVersion:
  cfgd.io/v1alpha1
kind: Module
metadata:
  name: split-key"
plant quoted-key "$apply" "'apiVersion': cfgd.io/v1alpha1
kind: Module
metadata:
  name: quoted-key"
# kind and apiVersion in every spelling the parser resolves to a string.
plant tag-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind: !!str Module
metadata:
  name: tag-kind
  labels:
    \${E2E_RUN_LABEL_YAML}
spec:
  packages: []"
plant anchor-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind: &k Module
metadata:
  name: anchor-kind
spec:
  packages: []"
plant folded-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind: >-
  Module
metadata:
  name: folded-kind"
plant literal-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind: |
  Module
metadata:
  name: literal-kind"
plant seq-kind "$apply" "apiVersion: cfgd.io/v1alpha1
kind: [Module]
metadata:
  name: seq-kind"
alias_api="metadata:
  name: alias-api
  annotations:
    group: &v cfgd.io/v1alpha1
apiVersion: *v
kind: Module"
plant alias-api "$apply" "$alias_api"
# Each expansion renders as one word, so text inside it that is not YAML
# (a ": " in a plain value) never reaches the parser; $VAR names the label too.
plant substitutions "$apply" "apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: \$(echo 'a: b')-\`echo 'c: d'\`-\${NAME:-e: f}-\$((1 + 1))
  labels:
    \$E2E_RUN_LABEL_YAML
spec:
  packages: []"
# cfgd.io text inside a string is no object.
cm_block="apiVersion: v1
kind: ConfigMap
metadata:
  name: cm-block
data:
  module.yaml: |
    apiVersion: cfgd.io/v1alpha1
    kind: Module
    metadata:
      name: in-a-string
      labels:
        \${E2E_RUN_LABEL_YAML}"
plant cm-block "$apply" "$cm_block"
cm_inline='apiVersion: v1
kind: ConfigMap
metadata:
  name: cm-inline
data: {note: "apiVersion: cfgd.io/v1alpha1"}'
plant cm-inline "$apply" "$cm_inline"
captured="yaml=\$(cat <<EOF"
captured_close="EOF"$'\n'")"
plant captured-tag-kind "$captured" "apiVersion: cfgd.io/v1alpha1
kind: !!str Module
metadata:
  name: captured-tag-kind" "$captured_close"
plant captured-anchor-kind "$captured" "apiVersion: cfgd.io/v1alpha1
kind: &k Module
metadata:
  name: captured-anchor-kind" "$captured_close"
plant captured-folded-kind "$captured" "apiVersion: cfgd.io/v1alpha1
kind: >-
  Module
metadata:
  name: captured-folded-kind" "$captured_close"
plant captured-alias-api "$captured" "$alias_api" "$captured_close"
plant captured-cm-block "$captured" "$cm_block" "$captured_close"
plant captured-cm-inline "$captured" "$cm_inline" "$captured_close"
plant nested-map "$apply" 'apiVersion: v1
kind: ConfigMap
metadata:
  name: nested-map
spec:
  template:
    apiVersion: cfgd.io/v1alpha1
    kind: Module'
plant flow-list "$apply" "{apiVersion: v1, kind: List, items: [{apiVersion: cfgd.io/v1alpha1, kind: Module, metadata: {name: flow-list}}]}"
plant json-hand "$apply" '{
  "apiVersion": "cfgd.io/v1alpha1",
  "kind": "Module",
  "metadata": {"name": "json-hand", "labels": {"cfgd.io/e2e-run": "42"}}
}'
# A substitution at the start of a line renders as one word in column 0, which
# ends the block scalar above it.
key_at_column_0="apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: unparsed
  labels:
    \${E2E_RUN_LABEL_YAML}
spec:
  signature:
    cosign:
      publicKey: |
\$(sed 's/^/        /' key.pub)"
plant_unparsed unparsed "$apply" "$key_at_column_0"
plant_unparsed captured-unparsed "$captured" "$key_at_column_0" "$captured_close"
plant_unparsed file-script "cat > \"\$dir/apply.sh\" <<EOF" 'if true; then
    echo "apiVersion: cfgd.io/v1alpha1" | kubectl apply -f -
fi'
plant heredoc-unterminated "$apply" "$module_labelled" ''
plant by-path-stdin "kubectl apply -n ns -f - <<EOF" "$module_labelled"
cat > "$fixtures/scripts/by-path.sh" <<'FIXTURE'
kubectl apply -f manifest.yaml
kubectl apply -f "$dir/mc.yaml"
kubectl apply --filename=x.yaml
kubectl apply --filename x.yaml
kubectl create -fpath.yaml
kubectl replace -f=x.yaml
kubectl apply -k overlays/e2e
kubectl replace -n ns \
    -f x.yaml
kubectl get cm x -o yaml | kubectl apply -f -
echo "$yaml" | kubectl apply -f-
kubectl create namespace ns
FIXTURE
bash -n "$fixtures/scripts/by-path.sh" || fail "fixture by-path is not valid shell"
outside_yaml='apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: outside\n'
printf '%s\n' "printf '$outside_yaml' | kubectl apply -f -" > "$fixtures/scripts/outside.sh"
bash -n "$fixtures/scripts/outside.sh" || fail "fixture outside is not valid shell"
printf '%s\n' "echo 'apiVersion: \"cfgd.io/v1alpha1\" # quoted' | kubectl apply -f -" \
    "echo 'apiVersion : cfgd.io/v1alpha1' | kubectl apply -f -" \
    "echo \"'apiVersion': cfgd.io/v1alpha1\" | kubectl apply -f -" > "$fixtures/scripts/outside-quoted.sh"
bash -n "$fixtures/scripts/outside-quoted.sh" || fail "fixture outside-quoted is not valid shell"
# shellcheck disable=SC2059 # the payload is the printf format the fixture runs
parsed="$(printf "$outside_yaml" | yq '.' 2>&1 >/dev/null)" || fail "fixture outside is not valid YAML: $parsed"

# Bash sends a quoted body as written, backticks included, and joins a line
# of an unquoted body ending in one backslash to the next.
# shellcheck disable=SC2016 # the backticks are the body's text, sent as written
plant quoted-backtick "kubectl apply -f - <<'EOF'" '{
  "kind": "Module", "a": "`", "apiVersion": "cfgd.io/v1alpha1", "b": "`",
  "metadata": {"name": "quoted-backtick"}
}'
plant quoted-continued "kubectl apply -f - <<'EOF'" 'apiVersion: cfgd.io/v1alpha1
kind: Module # \
metadata:
  name: quoted-continued'
plant continued-label "$apply" "apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: continued-label
  annotations:
    note: ends-in-an-escaped-backslash\\\\
  labels:
    # the run label \\
    \${E2E_RUN_LABEL_YAML}"
plant kind-var "$apply" "apiVersion: cfgd.io/v1alpha1
kind: \$KIND
metadata:
  name: kind-var"
plant kind-default "$apply" "apiVersion: cfgd.io/v1alpha1
kind: \${KIND:-Module}
metadata:
  name: kind-default"
plant api-var "$apply" "apiVersion: \$GROUP/v1alpha1
kind: Module
metadata:
  name: api-var"
plant captured-kind-var "$captured" "apiVersion: cfgd.io/v1alpha1
kind: \$KIND
metadata:
  name: captured-kind-var" "$captured_close"
plant whole-var "$apply" "---
\$MODULE_DOC"
plant whole-subst "$apply" "\$(cat \"\$dir/module.yaml\")"
plant whole-backtick "$apply" "\`cat module.yaml\`"
plant whole-item "$apply" "apiVersion: v1
kind: List
items:
  - \$MODULE_DOC"
plant captured-whole "$captured" "\$MODULE_DOC" "$captured_close"
plant block-scalar-expansion "$apply" "apiVersion: v1
kind: ConfigMap
metadata:
  name: block-scalar-expansion
data:
  module.yaml: |
    \$(cat module.yaml)"
# yq numbers lines from a file's first content line; the scan gives the
# script's.
plant lead-lines "$apply" "---
# a comment

$module_unlabelled"
plant lead-marker "$apply" "--- # a comment
$module_unlabelled"
cat > "$fixtures/scripts/by-stdin.sh" <<'FIXTURE'
kubectl apply -f - < "$f"
kubectl create -f- <"$f"
kubectl replace --filename=- < manifest.yaml
cat "$f" | kubectl apply -f -
sed "s/x/y/" "$f" | kubectl apply -f -
sed -e 's/x/y/' manifest.yaml | kubectl apply -f -
envsubst < "$f" | kubectl apply -f -
apply_yaml "T01" < "$f"
cat "$f" | apply_yaml "T01"
kubectl apply -n ns \
    -f - < "$f"
cat "$f" |
    kubectl apply -f -
out=$(cat "$f" | kubectl apply -f - 2>&1)
if ! kubectl apply -f - < "$f"; then echo failed; fi
kubectl apply -f x.yaml -f - < "$f"
exec_in_pod bash -c 'cat m.yaml | kubectl apply -f -'
bash -c "kubectl apply -f - < m.yaml"
exec_in_pod bash -c 'cat > m.yaml <<INNER
a: b
INNER'
kubectl apply -f - < m.yaml
FIXTURE
cat >> "$fixtures/scripts/by-stdin.sh" <<'FIXTURE'
cat <<EOF | kubectl apply -f -
apiVersion: v1
kind: ConfigMap
metadata:
  name: from-a-heredoc
EOF
sed 's/x/y/' <<'EOF' | kubectl apply -f -
apiVersion: v1
kind: ConfigMap
metadata:
  name: from-sed-on-a-heredoc
EOF
echo "$yaml" | kubectl apply -f -
printf '%s\n' "$yaml" | kubectl apply -f -
kubectl apply -f - <<<"$yaml"
echo "$yaml" | exec_in_pod bash -c 'kubectl apply -f -'
FIXTURE
# probe <name> <script>: a fixture holding one command line the scan must
# report, or for the ones named here, the script around it.
probe() {
    printf '%s\n' "$2" > "$fixtures/scripts/$1.sh"
    bash -n "$fixtures/scripts/$1.sh" || fail "fixture $1 is not valid shell"
}
# shellcheck disable=SC2016 # each script is a fixture's text, written as it is
{
    probe awk-pipe 'awk 1 "$dir/m.yaml" | kubectl apply -f -'
    probe jq-pipe 'jq . "$dir/m.json" | kubectl apply -f -'
    probe yq-pipe 'yq ".items[]" "$dir/m.yaml" | kubectl apply -f -'
    probe head-pipe 'head -n 50 "$dir/m.yaml" | kubectl apply -f -'
    probe kustomize-pipe 'kustomize build "$dir" | kubectl apply -f -'
    probe curl-pipe 'curl -fsSL "$url" | kubectl apply -f -'
    probe tee-pipe 'tee /dev/null < "$dir/m.yaml" | kubectl apply -f -'
    probe cat-in-pod 'exec_in_pod sh -c "cat m.yaml | kubectl apply -f -"'
    probe here-string-subst 'kubectl apply -f - <<<"$(cat "$dir/m.yaml")"'
    probe var-echo 'y=$(cat "$dir/m.yaml")'$'\n''echo "$y" | kubectl apply -f -'
    probe var-printf 'y=$(< "$dir/m.yaml")'$'\n''printf "%s" "$y" | kubectl apply -f -'
    probe var-here-string 'y=$(cat "$dir/m.yaml")'$'\n''kubectl apply -f - <<<"$y"'
    probe lt-procsub 'kubectl apply -f - < <(cat "$dir/m.yaml")'
    probe procsub 'kubectl apply -f <(cat "$dir/m.yaml")'
    probe dev-stdin 'kubectl apply -f /dev/stdin < "$dir/m.yaml"'
    probe no-feeder 'kubectl apply -f -'
    probe kubectl-var 'KUBECTL=kubectl'$'\n''$KUBECTL apply -f "$dir/m.yaml"'$'\n''${KUBECTL} apply -f "$dir/m.yaml"'$'\n''sudo "$KUBECTL" apply -f "$dir/m.yaml"'
    probe kubectl-option 'kubectl --context=e2e apply -f "$dir/m.yaml"'
    probe heredoc-then-here-string "kubectl apply -f - <<EOF <<<\"\$y\""$'\n''a: b'$'\n''EOF'
    probe heredoc-then-redirect "kubectl apply -f - <<EOF < \"\$f\""$'\n''a: b'$'\n''EOF'
    probe function-fed 'fed() {'$'\n''    kubectl apply -f - <<<"$y"'$'\n''    kubectl apply -f - < "$f"'$'\n''    cat "$f" | kubectl apply -f -'$'\n''}'
    probe wrapper-order 'wo_outer() {'$'\n''    wo_inner "$1"'$'\n''}'$'\n''wo_inner() {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''wo_outer T1 < "$f"'
    probe wrapper-chain 'wc_a() {'$'\n''    wc_b "$1"'$'\n''}'$'\n''wc_b() {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''wc_a T1 < "$f"'$'\n''wc_c() { echo; }'
    probe wrapper-chain-forward 'wf_c() {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''wf_b() {'$'\n''    wf_c "$1"'$'\n''}'$'\n''wf_a() {'$'\n''    wf_b "$1"'$'\n''}'$'\n''wf_a T1 < "$f"'
    probe wrapper-call-piped 'wp_inner() { kubectl apply -f -; }'$'\n''wp_outer() {'$'\n''    cat "$f" | wp_inner'$'\n''}'$'\n''wp_outer T1 < "$f"'
    probe function-keyword 'function fk_g() {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''fk_g < "$f"'
    # A { or } is a body brace only where bash reads it as a reserved word.
    probe lone-pair 'lb_f() {'$'\n''    echo {'$'\n''}'$'\n''kubectl apply -f -'$'\n''echo }'
    probe brace-word-pair 'bw_f() {'$'\n''    echo a{b'$'\n''}'$'\n''cat "$f" | kubectl apply -f -'$'\n''kubectl apply -f -'$'\n''echo c}d'
    probe default-json 'dj_f() {'$'\n''    local b=${body:-{}}'$'\n''    kubectl apply -f -'$'\n''}'$'\n''dj_f < "$f"'
    probe brace-groups 'grp() {'$'\n''    if { true; }; then { true; }; elif { true; }; then true; else { true; }; fi'$'\n''    while { false; }; do { true; }; done'$'\n''    until { true; }; do true; done'$'\n''    ! { false; }'$'\n''    true & { true; }'$'\n''    true | { cat; }'$'\n''    ( { true; } )'$'\n''    case x in x) { true; } ;; esac'$'\n''    { { true; } }'$'\n''    echo then {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''grp < "$f"'
    probe brace-time 'bt_f() {'$'\n''    time { true; }'$'\n''    time -p { true; }'$'\n''    coproc { cat; }'$'\n''    echo ` { true; } `'$'\n''    kubectl apply -f -'$'\n''}'$'\n''bt_f < "$f"'
    probe subshell-same-body-one 'ss() ( kubectl apply -f - )'$'\n''ss < "$f"'
    probe subshell-same-body-two 'ss() ('$'\n''    kubectl apply -f -'$'\n'')'$'\n''ss < "$f"'
    probe nested-one-line 'nl_o() { nl_i() { true; }'$'\n''    kubectl apply -f -'$'\n''}'$'\n''nl_o < "$f"'
    probe two-on-one-line 'md_a() { true; }; md_b() { true; }'$'\n''kubectl apply -f -'
    probe brace-word-in-body 'bz_f() {'$'\n''    echo }'$'\n''    kubectl apply -f -'$'\n''}'$'\n''bz_f < "$f"'
    probe nested-subshell-one-line 'po() ( pi() ( true; )'$'\n''    kubectl apply -f -'$'\n'')'$'\n''po < "$f"'
    # Two bodies that differ only inside a quoted word.
    probe quoted-body-one 'qb() { echo "a"; }'
    probe quoted-body-two 'qb() { echo "b"; }'
    # Two bodies cut at their first opener, before quoted text shaped like one.
    probe opener-quoted-one 'og() { kubectl apply -f -; echo "og()"; }'
    probe opener-quoted-two 'og() { echo nope; echo "og()"; }'
    probe suffix-name-one 'true; x_sf() { :; }; sf() { kubectl apply -f -; }'
    probe suffix-name-two 'sf() { kubectl apply -f -; }'
    probe dotted-prefix-one 'true; cxd() { :; }; c.d() { kubectl apply -f -; }'
    probe dotted-prefix-two 'c.d() { kubectl apply -f -; }'
    probe dotted-name-one 'a.b() { kubectl apply -f -; echo "axb()"; }'
    probe dotted-name-two 'a.b() { echo nope; echo "axb()"; }'
    # A function is defined, and called, at any command word.
    probe def-after-and 'true && ar_k() { kubectl "$@"; }'$'\n''ar_k apply -f m.yaml'
    probe def-after-semi 'set -e; sr_k() { kubectl "$@"; }'$'\n''sr_k apply -f m.yaml'$'\n''sp_k ( ) { kubectl "$@"; }'$'\n''sp_k apply -f m.yaml'
    probe def-keyword-after-semi 'true; function fr_k { kubectl "$@"; }'$'\n''fr_k apply -f m.yaml'
    probe call-prefixes 'cp_w() { kubectl apply -f -; }'$'\n''FOO=1 cp_w T1 < "$f"'$'\n''2>/dev/null cp_w T2 < "$f"'$'\n''time cp_w T3 < "$f"'$'\n''x=`cp_w T4 < "$f"`'$'\n''&>/dev/null cp_w T5 < "$f"'$'\n''> /dev/null cp_w T6 < "$f"'$'\n''A+=1 cp_w T7 < "$f"'$'\n''a[0]=1 cp_w T8 < "$f"'$'\n''0<&3 cp_w T9 < "$f"'$'\n''time -p cp_w T10 < "$f"'$'\n''time -- cp_w T11 < "$f"'$'\n''>| /dev/null cp_w T12 < "$f"'$'\n''cp_k() { kubectl "$@"; }'$'\n''time -p cp_k apply -f m.yaml'$'\n''cp_o() {'$'\n''    time -p cp_w'$'\n''}'$'\n''cp_o < "$f"'$'\n''time 2>/dev/null cp_w T13 < "$f"'
    # A wrapper name as an argument is no call.
    probe arg-named-runner 'ra_k() { kubectl "$@"; }'$'\n''echo ra_k apply -f m.yaml'$'\n''ra_k apply -f m.yaml'
    probe arg-named-wrapper 'an_w() { kubectl apply -f -; }'$'\n''an_o() {'$'\n''    echo an_w'$'\n''}'$'\n''an_o < "$f"'$'\n''echo an_w | kubectl apply -f -'$'\n''true && an_w < "$f"'$'\n''cat "$f" | an_w'
    # A function name is any word bash takes for one.
    probe odd-names 'k+x() { kubectl "$@"; }'$'\n''k+x apply -f m.yaml'$'\n''k@() { kubectl "$@"; }'$'\n''k@ apply -f m.yaml'$'\n''1k() { kubectl "$@"; }'$'\n''1k apply -f m.yaml'$'\n''function k%x { kubectl "$@"; }'$'\n''k%x apply -f m.yaml'$'\n''k[1]() { kubectl "$@"; }'$'\n''k[1] apply -f m.yaml'$'\n''w+x() { kubectl apply -f -; }'$'\n''w+x < "$f"'
    # judge splits a command at a brace, so a name holding one fails.
    probe brace-name 'bn_o() {'$'\n''    a{b() { :; }'$'\n''    kubectl apply -f -'$'\n''}'
    # An opener in a string, or after a character that ends no word, opens no body.
    probe opener-in-string-one $'true \\\n&& echo "x oq()" \'x oq()\' "\\" oq()" \\x && sh -c \'true\' && oq() { kubectl apply -f -; }'
    probe opener-in-string-two 'oq() { kubectl apply -f -; }'
    probe plus-prefix-one 'true; k+pq() { :; }; pq() { kubectl apply -f -; }'
    probe plus-prefix-two 'pq() { kubectl apply -f -; }'
    probe opener-after-each-one $'true;\tms_tb() { kubectl apply -f -; }\ntrue;ms_sc() { kubectl apply -f -; }\ntrue&&ms_am() { kubectl apply -f -; }\nfalse||ms_pi() { kubectl apply -f -; }\ntrue;(ms_op() { kubectl apply -f -; })\necho `ms_bt() { kubectl apply -f -; }`\ncase x in x)ms_cp() { kubectl apply -f -; };; esac'
    probe opener-after-each-two $'ms_tb() { kubectl apply -f -; }\nms_sc() { kubectl apply -f -; }\nms_am() { kubectl apply -f -; }\nms_pi() { kubectl apply -f -; }\n (ms_op() { kubectl apply -f -; })\n: `ms_bt() { kubectl apply -f -; }`\ncase y in y)ms_cp() { kubectl apply -f -; };; esac'
    # The opener is the first one on the line: function NAME later in a body opens nothing.
    probe later-keyword-one 'fk() { kubectl apply -f -; echo function fk; }'
    probe later-keyword-two 'fk() {'$'\n''    kubectl apply -f -; echo function fk; }'
    # An empty substitution, array or process substitution defines nothing.
    probe empty-groups $'echo $()\na=()\ncat <()\ncat >()'
    # A backslash escapes the next character in an ANSI-C string, and an odd run of $ makes one.
    probe ansi-swallow $'echo $\'it\\\'s\'\nkubectl apply -f -\necho \'x\''
    probe ansi-dollars $'echo $$\'a\\\'\nkubectl apply -f -'
    probe dollar-dq-one 'echo $"dq()"; dq() { kubectl apply -f -; }'
    probe dollar-dq-two 'dq() { kubectl apply -f -; }'
    # A double-quoted line ending in a backslash goes on into the next line.
    probe dq-backslash-one $'echo "a\\\nb"; mq(){ kubectl apply -f -; }'
    probe dq-backslash-two 'mq(){ kubectl apply -f -; }'
    # Bash refuses these, so they skip the probe's syntax check.
    printf '%s\n' 'true' 'true' 'true' "echo 'a" 'kubectl apply -f -' > "$fixtures/scripts/unclosed-single.sh"
    printf '%s\n' 'true' 'true' 'echo "a' 'kubectl apply -f -' > "$fixtures/scripts/unclosed-double.sh"
    printf '%s\n' 'echo "a"' "bash -c 'true" 'kubectl apply -f -' > "$fixtures/scripts/unclosed-bash-c.sh"
    printf '%s\n' 'ub_f() {' '    kubectl apply -f -' > "$fixtures/scripts/unclosed-body.sh"
    # One body defined in two files, spaced and opened differently.
    probe same-body-one 'sb() { true; true; kubectl apply -f -; true & } >/dev/null'$'\n''sb < "$f"'
    probe same-body-two '# The same body, spaced out.'$'\n''function sb {'$'\n''    true;'$'\n''    true'$'\n''    kubectl  apply -f - # reads stdin'$'\n''    true &'$'\n''}>/dev/null'$'\n''sb < "$f"'
    probe subshell-body 'sbody_f() ('$'\n''    echo x'$'\n'')'$'\n''kubectl apply -f -'
    probe subshell-body-group 'sg_f() ('$'\n''    { echo x; }'$'\n''    kubectl apply -f -'$'\n'')'$'\n''sg_f < "$f"'
    probe brace-after-subshell 'ba_f() ( echo x; )'$'\n''ba_g()'$'\n''{'$'\n''    kubectl apply -f -'$'\n''}'$'\n''ba_g < "$f"'
    probe subshell-body-one-line 'so_f() ( echo x; )'$'\n''kubectl apply -f -'
    probe subshell-then-brace 'st_f() ( echo x; )'$'\n''st_g() {'$'\n''    echo y'$'\n''}'$'\n''st_main() {'$'\n''    echo z'$'\n''}'$'\n''kubectl apply -f -'$'\n''cat "$f" | kubectl apply -f -'
    probe dry-run-spaced 'kubectl apply --dry-run client -f m.yaml'
    probe dry-run-bare 'kubectl apply --dry-run -f m.yaml'
    probe dry-run-server 'kubectl apply --dry-run=server -f m.yaml'
    probe dry-run-none 'kubectl apply --dry-run=none -f m.yaml'
    probe after-function 'af_f() { kubectl get ns; }'$'\n''kubectl apply -f -'$'\n''af_g() {'$'\n''    true'$'\n''}'$'\n''kubectl apply -f -'
    probe kubectl-array 'kc=(kubectl --context e2e)'$'\n''"${kc[@]}" apply -f "$dir/m.yaml"'
    probe kubectl-function 'k() { kubectl "$@"; }'$'\n''k apply -k "$dir"'
    probe sudo-kubectl 'sudo -E kubectl --context e2e apply -f - < "$dir/m.yaml"'
    probe wrapper-redirect 'wr_apply_it() {'$'\n''    kubectl apply -f -'$'\n''}'$'\n''wr_apply_it < "$dir/m.yaml"'
}
plant kubectl-var-heredoc "KUBECTL=kubectl"$'\n'"\$KUBECTL apply -f - <<EOF" "$module_unlabelled"
plant wrapper-order-heredoc "woh_outer() {"$'\n'"    woh_inner \"\$1\""$'\n'"}"$'\n'"woh_inner() {"$'\n'"    kubectl apply -f -"$'\n'"}"$'\n'"woh_outer T1 <<EOF" "$module_unlabelled"
plant wrapper-chain-heredoc "wch_a() {"$'\n'"    wch_b \"\$1\""$'\n'"}"$'\n'"wch_b() {"$'\n'"    wch_c \"\$1\""$'\n'"}"$'\n'"wch_c() {"$'\n'"    kubectl apply -f -"$'\n'"}"$'\n'"wch_a T1 <<EOF" "$module_unlabelled"
plant wrapper-call-fed "wcf_outer() {"$'\n'"    wcf_inner < \"\$f\""$'\n'"}"$'\n'"wcf_inner() {"$'\n'"    kubectl apply -f -"$'\n'"}"$'\n'"wcf_outer T1 <<EOF" "$module_unlabelled"
plant wrapper-call-heredoc "wchd_inner() { kubectl apply -f -; }"$'\n'"wchd_outer() {"$'\n'"    wchd_inner <<X" "a: b" "X"$'\n'"}"$'\n'"wchd_outer < \"\$f\""
# One function name defined in two files with two bodies, a wrapper in one.
plant same-name-wrapper "w() { kubectl apply -f -; }"$'\n'"w <<EOF" "$module_unlabelled"
plant same-name-other "w() { echo; }"$'\n'"w <<EOF" "$module_unlabelled"
# Two bodies that differ only inside their heredocs.
# A wrapper name as a redirect target is no call.
plant fd-heredoc-first "fh_w() { kubectl apply -f -; }"$'\n'"3<<EOF fh_w < \"\$f\"" "$module_labelled"
plant heredoc-first "hf_w() { kubectl apply -f -; }"$'\n'"<<EOF hf_w" "$module_unlabelled"
plant redirect-named-wrapper "rn_w() { kubectl apply -f -; }"$'\n'"x=\$(cat <<EOF > rn_w" "$machine_config" "EOF"$'\n'")"
plant heredoc-body-one "hb() {"$'\n'"    kubectl apply -f - <<EOF" "a: 1" "EOF"$'\n'"}"
plant heredoc-body-two "hb() {"$'\n'"    kubectl apply -f - <<EOF" "a: 2" "EOF"$'\n'"}"
plant fd3-heredoc "kubectl apply -f - 3<<EOF" "$module_labelled"
plant fd0-heredoc "kubectl apply -f - 0<<EOF" "$module_labelled"
plant pipe-then-heredoc "cat \"\$f\" | kubectl apply -f - <<EOF" "$module_labelled"
plant wrapper-heredoc "wh_apply_it() { kubectl apply -f -; }"$'\n'"wh_apply_it <<EOF" "$module_unlabelled"
# Commands the scan must leave alone: the body of a wrapper, an apply that
# sends nothing, the other tools the suites run with an apply verb or a -f,
# and text that only mentions an apply.
cat > "$fixtures/scripts/apply-negative.sh" <<'FIXTURE'
apply_yaml() {
    local test_id="$1" output
    if ! output=$(kubectl apply -f - 2>&1); then
        echo "  kubectl apply failed: $output"
        return 1
    fi
}
echo "$yaml" | kubectl apply --dry-run=client -f -
kubectl apply --dry-run=client -f manifest.yaml
run "${C[@]}" apply --yes
exec_in_pod cfgd --config "$c" apply --yes
cfgd --config "$c" apply -f x.yaml
helm upgrade --install r chart -f values.yaml
crossplane xpkg push "$img" -f "$xpkg"
kubectl create namespace ns
echo "  kubectl apply -f - < $f failed" | tee -a log
yaml=$(cat)
kubectl get cm x -o yaml > "$f"
bash -c 'echo "kubectl apply -f - < x"'
kubectl apply --dry-run='client' -f m.yaml
kubectl apply --dry-run="client" -f m.yaml
FIXTURE
bash -n "$fixtures/scripts/by-stdin.sh" || fail "fixture by-stdin is not valid shell"
bash -n "$fixtures/scripts/apply-negative.sh" || fail "fixture apply-negative is not valid shell"
cat > "$fixtures/scripts/redefine.sh" <<'FIXTURE'
E2E_RUN_LABEL_YAML="team: x"
export E2E_RUN_LABEL_YAML="team: x"
local E2E_RUN_LABEL_YAML="team: x"
declare E2E_RUN_LABEL_YAML="team: x"
readonly E2E_RUN_LABEL_YAML="team: x"
: "${E2E_RUN_LABEL_YAML:=team: x}"
echo "$E2E_RUN_LABEL_YAML" "${E2E_RUN_LABEL_YAML}" "${#E2E_RUN_LABEL_YAML}" "${E2E_RUN_LABEL_YAML:-x}"
# E2E_RUN_LABEL_YAML="team: x" in a comment
FIXTURE
bash -n "$fixtures/scripts/redefine.sh" || fail "fixture redefine is not valid shell"

want_fixture_scan="SITE substitutions.sh:1
SITE alias-api.sh:1
UNLABELLED alias-api.sh:6
SITE anchor-kind.sh:1
UNLABELLED anchor-kind.sh:2
SITE apply-yaml.sh:1
UNLABELLED apply-yaml.sh:2
BYPATH captured-operator-kind.sh:10
BYPATH outside.sh:1
BYPATH outside-quoted.sh:1
BYPATH outside-quoted.sh:2
BYPATH outside-quoted.sh:3
BYPATH by-stdin.sh:23
BYPATH by-stdin.sh:29
BYPATH by-stdin.sh:35
BYPATH by-stdin.sh:36
BYPATH by-stdin.sh:37
BYPATH by-stdin.sh:38
BYPATH awk-pipe.sh:1
BYPATH jq-pipe.sh:1
BYPATH yq-pipe.sh:1
BYPATH head-pipe.sh:1
BYPATH kustomize-pipe.sh:1
BYPATH curl-pipe.sh:1
BYPATH tee-pipe.sh:1
BYPATH cat-in-pod.sh:1
BYPATH here-string-subst.sh:1
BYPATH var-echo.sh:2
BYPATH var-printf.sh:2
BYPATH var-here-string.sh:2
BYPATH lt-procsub.sh:1
BYPATH procsub.sh:1
BYPATH dev-stdin.sh:1
BYPATH no-feeder.sh:1
BYPATH kubectl-var.sh:2
BYPATH kubectl-var.sh:3
BYPATH kubectl-var.sh:4
BYPATH kubectl-option.sh:1
BYPATH heredoc-then-here-string.sh:1
BYPATH heredoc-then-redirect.sh:1
BYPATH function-fed.sh:2
BYPATH function-fed.sh:3
BYPATH function-fed.sh:4
BYPATH after-function.sh:2
BYPATH after-function.sh:6
SITE pipe-then-heredoc.sh:1
BYPATH wrapper-order.sh:7
BYPATH wrapper-chain.sh:7
BYPATH wrapper-chain-forward.sh:10
BYPATH wrapper-call-piped.sh:3
BYPATH function-keyword.sh:4
BYPATH lone-pair.sh:4
BYPATH brace-word-pair.sh:4
BYPATH brace-word-pair.sh:5
BYPATH default-json.sh:5
BYPATH brace-groups.sh:14
UNREADABLE unclosed-body.sh:1
BYPATH same-body-one.sh:2
BYPATH same-body-two.sh:8
BYPATH subshell-same-body-one.sh:2
BYPATH subshell-same-body-two.sh:4
BYPATH nested-one-line.sh:4
BYPATH brace-word-in-body.sh:5
BYPATH nested-subshell-one-line.sh:4
BYPATH two-on-one-line.sh:2
BYPATH def-after-semi.sh:4
BYPATH call-prefixes.sh:7
BYPATH call-prefixes.sh:8
BYPATH call-prefixes.sh:9
BYPATH call-prefixes.sh:10
BYPATH call-prefixes.sh:11
BYPATH call-prefixes.sh:12
BYPATH call-prefixes.sh:13
BYPATH call-prefixes.sh:15
BYPATH call-prefixes.sh:19
BYPATH call-prefixes.sh:20
SITE heredoc-first.sh:2
BYPATH fd-heredoc-first.sh:2
FILEDOC fd-heredoc-first.sh:2
BYPATH arg-named-runner.sh:3
BYPATH odd-names.sh:2
BYPATH odd-names.sh:4
BYPATH odd-names.sh:6
BYPATH odd-names.sh:8
BYPATH odd-names.sh:10
BYPATH odd-names.sh:12
UNREADABLE brace-name.sh:2
BYPATH ansi-swallow.sh:2
BYPATH ansi-dollars.sh:2
UNREADABLE unclosed-single.sh:4
UNREADABLE unclosed-double.sh:3
UNREADABLE unclosed-bash-c.sh:2
BYPATH unclosed-bash-c.sh:2
UNLABELLED heredoc-first.sh:3
BYPATH brace-time.sh:8
BYPATH def-after-and.sh:2
BYPATH def-after-semi.sh:2
BYPATH def-keyword-after-semi.sh:2
BYPATH call-prefixes.sh:2
BYPATH call-prefixes.sh:3
BYPATH call-prefixes.sh:4
BYPATH call-prefixes.sh:5
BYPATH call-prefixes.sh:6
BYPATH arg-named-wrapper.sh:6
BYPATH arg-named-wrapper.sh:7
BYPATH arg-named-wrapper.sh:8
FILEDOC redirect-named-wrapper.sh:2
BYPATH subshell-body.sh:4
BYPATH subshell-body-one-line.sh:2
BYPATH subshell-body-group.sh:5
BYPATH brace-after-subshell.sh:6
BYPATH subshell-then-brace.sh:8
BYPATH subshell-then-brace.sh:9
BYPATH dry-run-spaced.sh:1
BYPATH dry-run-bare.sh:1
BYPATH dry-run-server.sh:1
BYPATH dry-run-none.sh:1
SITE wrapper-order-heredoc.sh:7
UNLABELLED wrapper-order-heredoc.sh:8
SITE wrapper-chain-heredoc.sh:10
UNLABELLED wrapper-chain-heredoc.sh:11
BYPATH wrapper-call-fed.sh:2
FILEDOC wrapper-call-fed.sh:7
DUPLICATE same-name-wrapper.sh:1
SITE same-name-wrapper.sh:2
UNLABELLED same-name-wrapper.sh:3
SITE same-name-other.sh:2
UNLABELLED same-name-other.sh:3
DUPLICATE heredoc-body-two.sh:1
DUPLICATE quoted-body-two.sh:1
DUPLICATE opener-quoted-two.sh:1
DUPLICATE dotted-name-two.sh:1
BYPATH fd3-heredoc.sh:1
FILEDOC fd3-heredoc.sh:1
SITE fd0-heredoc.sh:1
BYPATH kubectl-array.sh:2
BYPATH kubectl-function.sh:2
BYPATH sudo-kubectl.sh:1
BYPATH wrapper-redirect.sh:4
SITE kubectl-var-heredoc.sh:2
UNLABELLED kubectl-var-heredoc.sh:3
SITE wrapper-heredoc.sh:2
UNLABELLED wrapper-heredoc.sh:3
BYPATH by-path.sh:1
BYPATH by-path.sh:2
BYPATH by-path.sh:3
BYPATH by-path.sh:4
BYPATH by-path.sh:5
BYPATH by-path.sh:6
BYPATH by-path.sh:7
BYPATH by-path.sh:8
BYPATH by-path.sh:10
BYPATH by-path.sh:11
SITE by-path-stdin.sh:1
CAPTURED captured-alias-api.sh:6
CAPTURED captured-anchor-kind.sh:2
CAPTURED captured-dup.sh:2
CAPTURED captured-folded-kind.sh:2
NESTED captured-list.sh:11
NESTED captured-list.sh:5
CAPTURED captured-operator-kind.sh:2
FILEDOC captured-other-kind.sh:1
SITE captured.sh:1
UNLABELLED captured.sh:2
CAPTURED captured-tag-kind.sh:2
FILEDOC captured-to-file.sh:1
UNPARSED captured-unparsed.sh:1
SITE comment-heredoc.sh:2
UNLABELLED comment-heredoc.sh:3
SITE continued.sh:2
UNLABELLED continued.sh:3
SITE dash.sh:1
SITE default-label.sh:1
UNLABELLED default-label.sh:2
SITE job-label-only.sh:1
UNLABELLED job-label-only.sh:2
UNLABELLED dash.sh:2
SITE entry-comment.sh:1
UNLABELLED entry-comment.sh:2
SITE exec-apply.sh:1
UNLABELLED exec-apply.sh:2
FILEDOC file-operator-kind.sh:1
SITE flow-doc.sh:1
SITE flow-labels.sh:1
NESTED flow-list.sh:2
SITE flow-metadata.sh:1
SITE flow-multiline.sh:1
UNLABELLED flow-multiline.sh:3
SITE folded-kind.sh:1
UNLABELLED folded-kind.sh:2
SITE hand-spelled.sh:1
HANDSPELLED hand-spelled.sh:2
UNTERMINATED heredoc-unterminated.sh:1
FILEDOC in-pod.sh:1
SITE json-doc.sh:1
UNLABELLED json-doc.sh:3
SITE json-hand.sh:1
HANDSPELLED json-hand.sh:3
SITE key-prefix.sh:1
UNLABELLED key-prefix.sh:2
SITE label-absent.sh:1
UNLABELLED label-absent.sh:2
SITE labelled.sh:1
SITE labels-comment.sh:1
UNLABELLED labels-comment.sh:2
NESTED list.sh:5
SITE literal-kind.sh:1
UNLABELLED literal-kind.sh:2
SITE multi-doc.sh:1
UNLABELLED multi-doc.sh:12
NESTED nested-map.sh:8
SITE nested-metadata.sh:1
UNLABELLED nested-metadata.sh:2
NOKIND no-kind.sh:2
SITE no-labels.sh:1
UNLABELLED no-labels.sh:2
OTHERKIND other-kind.sh:2
OUTSIDE outside-quoted.sh:1
OUTSIDE outside-quoted.sh:2
OUTSIDE outside-quoted.sh:3
OUTSIDE outside.sh:1
SITE quoted-key.sh:1
UNLABELLED quoted-key.sh:2
SITE quoted-kind.sh:1
UNLABELLED quoted-kind.sh:2
SITE quoted.sh:1
QUOTED quoted.sh:2
SITE rc-ok-comment.sh:1
UNLABELLED rc-ok-comment.sh:2
NOKIND seq-kind.sh:2
SITE spaced-colon.sh:1
UNLABELLED spaced-colon.sh:2
SITE spaced-kind.sh:1
UNLABELLED spaced-kind.sh:2
SITE split-key.sh:1
UNLABELLED split-key.sh:2
SITE tagged-value.sh:1
UNLABELLED tagged-value.sh:2
SITE tag-kind.sh:1
UNPARSED unparsed.sh:1
SITE var-prefix.sh:1
UNLABELLED var-prefix.sh:2
BYPATH by-stdin.sh:1
BYPATH by-stdin.sh:10
BYPATH by-stdin.sh:12
BYPATH by-stdin.sh:14
BYPATH by-stdin.sh:15
BYPATH by-stdin.sh:16
BYPATH by-stdin.sh:17
BYPATH by-stdin.sh:18
BYPATH by-stdin.sh:22
BYPATH by-stdin.sh:2
BYPATH by-stdin.sh:3
BYPATH by-stdin.sh:4
BYPATH by-stdin.sh:5
BYPATH by-stdin.sh:6
BYPATH by-stdin.sh:7
BYPATH by-stdin.sh:8
BYPATH by-stdin.sh:9
CONTINUED continued-label.sh:9
EXPANDED api-var.sh:2
EXPANDED captured-kind-var.sh:2
EXPANDED captured-whole.sh:2
EXPANDED kind-default.sh:2
EXPANDED kind-var.sh:2
EXPANDED whole-backtick.sh:2
EXPANDED whole-item.sh:5
EXPANDED whole-subst.sh:2
EXPANDED whole-var.sh:3
QUOTED quoted-backtick.sh:3
QUOTED quoted-continued.sh:2
REDEFINED redefine.sh:1
REDEFINED redefine.sh:2
REDEFINED redefine.sh:3
REDEFINED redefine.sh:4
REDEFINED redefine.sh:5
REDEFINED redefine.sh:6
SITE continued-label.sh:1
SITE lead-lines.sh:1
SITE lead-marker.sh:1
SITE quoted-backtick.sh:1
SITE quoted-continued.sh:1
UNLABELLED lead-lines.sh:5
UNLABELLED lead-marker.sh:3"
fixture_scan="$(scan_run_labels "$kinds" "$fixtures/scripts")"
got_fixture_scan="$(awk '{print $1, $2}' <<<"$fixture_scan" | sed "s|$fixtures/scripts/||; s|:\$||" | sort)"
if [ "$got_fixture_scan" = "$(sort <<<"$want_fixture_scan")" ]; then
    pass "the run-label scan reports each planted fixture it should and no other"
else
    fail "the run-label scan judged the planted fixtures wrongly (< want, > got):"
    diff <(sort <<<"$want_fixture_scan") <(printf '%s\n' "$got_fixture_scan") | grep '^[<>]' | sed 's/^/      /' || true
fi

# expect_red <label> <verdict> <pattern>: the verdict must hold a line matching
# the pattern.
expect_red() {
    if grep -qE -- "$3" <<<"$2"; then pass "$1"; else fail "$1: verdict was: ${2:-clean}"; fi
}

# copy_tree <dest>: a scratch copy of every suite's scripts directory.
copy_tree() {
    local dir suite
    for dir in "$e2e_root"/*/scripts; do
        suite="$(basename "$(dirname "$dir")")"
        mkdir -p "$1/$suite"
        cp -R "$dir" "$1/$suite/"
    done
}
# tree_verdict_of <root>: the verdict on a scratch tree.
tree_verdict_of() {
    local dirs
    mapfile -t dirs < <(label_dirs "$1")
    label_verdict "$1" "$(scan_run_labels "$kinds" "${dirs[@]}")"
}

tree="$scratch/tree"
copy_tree "$tree"
probe="$tree/operator/scripts/test-configpolicy.sh"
label_line="$(grep -nxF "    \${E2E_RUN_LABEL_YAML}" "$probe" | head -n1 | cut -d: -f1)"
sed -i "${label_line}d" "$probe"
probe_verdict="$(tree_verdict_of "$tree")"
if [ "$(wc -l <<<"$probe_verdict")" -eq 1 ] && grep -q "^UNLABELLED $probe:" <<<"$probe_verdict"; then
    pass "removing one real label line makes the scan report that object"
else
    fail "removing line $label_line of test-configpolicy.sh: verdict was: ${probe_verdict:-clean}"
fi

floored="$scratch/floored"
copy_tree "$floored"
mv "$floored/full-stack/scripts" "$scratch/full-stack-moved"
mkdir -p "$floored/full-stack/scripts"
printf 'true\n' > "$floored/full-stack/scripts/no-sites.sh"
floor_verdict="$(tree_verdict_of "$floored")"
if [ "$floor_verdict" = "FLOOR full-stack: only 0 heredocs apply an operator object (want at least ${run_label_floors[1]}); the scan has lost its population" ]; then
    pass "a suite emptied of sites fails its own floor while the others hold theirs"
else
    fail "full-stack emptied of sites: verdict was: ${floor_verdict:-clean}"
fi
missing="$scratch/missing"
copy_tree "$missing"
mv "$missing/gateway" "$scratch/gateway-moved"
missing_verdict="$(tree_verdict_of "$missing")"
expect_red "a floored suite with no scripts directory fails the scan" "$missing_verdict" "^EMPTY $missing/gateway/scripts"
expect_red "a floored suite with no scripts directory fails its floor" "$missing_verdict" "^FLOOR gateway: only 0 "

by_path_tree="$scratch/by-path"
copy_tree "$by_path_tree"
printf 'kubectl apply -f mc.yaml\n' > "$by_path_tree/operator/scripts/zz-by-path.sh"
by_path_verdict="$(tree_verdict_of "$by_path_tree")"
if [ "$by_path_verdict" = "BYPATH $by_path_tree/operator/scripts/zz-by-path.sh:1: the scan cannot read a manifest applied by path; apply it from a heredoc" ]; then
    pass "a manifest applied by path in a floored suite fails the scan, and the crossplane suite's do not"
else
    fail "a manifest applied by path in the operator suite: verdict was: ${by_path_verdict:-clean}"
fi
expect_red "a call of apply_yaml is judged when the script that defines it is not scanned" \
    "$(scan_run_labels "$kinds" "$fixtures/scripts/by-stdin.sh")" "^BYPATH $fixtures/scripts/by-stdin.sh:9: "
expect_red "a function body still open when the last file scanned ends fails the scan" \
    "$(scan_run_labels "$kinds" "$fixtures/scripts/unclosed-body.sh")" "^UNREADABLE $fixtures/scripts/unclosed-body.sh:1: "
expect_red "a function name two scanned scripts define with two bodies names both definitions" \
    "$(scan_run_labels "$kinds" "$fixtures/scripts/same-name-other.sh" "$fixtures/scripts/same-name-wrapper.sh")" \
    "^DUPLICATE $fixtures/scripts/same-name-wrapper.sh:1: function w is also defined at $fixtures/scripts/same-name-other.sh:1 "
mkdir -p "$scratch/visible/a/scripts" "$scratch/visible/b/scripts"
# shellcheck disable=SC2016 # the lines are fixture scripts' text
{
    printf '%s\n' 'w2() {' '    kubectl apply -f -' '}' > "$scratch/visible/a/scripts/defs.sh"
    printf '%s\n' 'w2 < "$f"' > "$scratch/visible/a/scripts/same-dir.sh"
    printf '%s\n' 'source "$SCRIPT_DIR/../../a/scripts/defs.sh"' 'w2 < "$f"' > "$scratch/visible/b/scripts/use.sh"
}
visible="$(scan_run_labels "$kinds" "$scratch/visible/a/scripts" "$scratch/visible/b/scripts" | cut -d: -f1-2 | sort)"
if [ "$visible" = "BYPATH $scratch/visible/a/scripts/same-dir.sh:1"$'\n'"BYPATH $scratch/visible/b/scripts/use.sh:2" ]; then
    pass "a wrapper one suite defines is a wrapper in every scanned script"
else
    fail "a wrapper called from its own suite and from another: verdict was: ${visible:-clean}"
fi
mkdir -p "$scratch/helpers-wrapper/scripts"
printf '%s\n' 'hw() { kubectl apply -f -; }' > "$scratch/helpers-wrapper/helpers.sh"
# shellcheck disable=SC2016 # the line is a fixture script's text
printf '%s\n' 'hw < "$f"' > "$scratch/helpers-wrapper/scripts/calls-hw.sh"
expect_red "a wrapper helpers.sh defines is a wrapper in the scripts that source it" \
    "$(helpers="$scratch/helpers-wrapper/helpers.sh" scan_run_labels "$kinds" "$scratch/helpers-wrapper/helpers.sh" "$scratch/helpers-wrapper/scripts")" \
    "^BYPATH $scratch/helpers-wrapper/scripts/calls-hw.sh:1: "
expect_red "a manifest applied by path in helpers.sh fails the scan" \
    "$(by_path_in_scope "$e2e_root" "BYPATH $helpers:9: applied by path")" "^BYPATH $helpers:9:"

mkdir -p "$scratch/empty" "$scratch/broken"
ln -s "$scratch/nowhere" "$scratch/broken/gone.sh"
expect_red "a directory with no files fails the scan" "$(scan_run_labels "$kinds" "$scratch/empty")" "^EMPTY $scratch/empty"
expect_red "a file the scan cannot read fails it" "$(scan_run_labels "$kinds" "$scratch/broken")" "^UNREADABLE $scratch/broken/gone.sh"
# fail_awk <dir> <case pattern>: an awk on PATH that exits 2 when its arguments
# match the pattern and runs the real awk otherwise.
fail_awk() {
    mkdir -p "$1"
    printf '#!/bin/sh\ncase "$*" in %s) exit 2 ;; esac\nexec %s "$@"\n' "$2" "$(command -v awk)" > "$1/awk"
    chmod +x "$1/awk"
}
fail_awk "$scratch/fail-reader" '*heredocs.awk*'
fail_awk "$scratch/fail-scan" '*kinds=*'
expect_red "heredocs.awk failing to read the scripts fails the scan" \
    "$(PATH="$scratch/fail-reader:$PATH" scan_run_labels "$kinds" "$fixtures/scripts")" '^UNREADABLE heredocs.awk exited 2'
expect_red "the scan's own awk failing fails the scan" \
    "$(PATH="$scratch/fail-scan:$PATH" scan_run_labels "$kinds" "$fixtures/scripts")" "^UNREADABLE the scan's awk exited 2"
fail_awk "$scratch/fail-collector" '*work=*'
expect_red "the scan's heredoc collector failing fails the scan" \
    "$(PATH="$scratch/fail-collector:$PATH" scan_run_labels "$kinds" "$fixtures/scripts")" "^UNREADABLE the scan's heredoc collector exited 2"
# An awk whose quote mask runs one character long on every line.
mkdir -p "$scratch/skew-mask"
# shellcheck disable=SC2016 # the stub's own text, expanded when it runs
printf '%s\n' '#!/bin/sh' \
    'for a; do shift; case "$a" in *"function squash"*) a="$(printf "%s" "$a" | sed "s/kept = s; mask = \"\"/kept = s; mask = \"Z\"/")" ;; esac; set -- "$@" "$a"; done' \
    "exec $(command -v awk) \"\$@\"" > "$scratch/skew-mask/awk"
chmod +x "$scratch/skew-mask/awk"
expect_red "a quote mask out of step with its line fails the scan" \
    "$(PATH="$scratch/skew-mask:$PATH" scan_run_labels "$kinds" "$fixtures/scripts/labelled.sh")" "^UNREADABLE $fixtures/scripts/labelled.sh:1: the scan lost its place"
# A yq that fails without naming a body has each body read on its own, so its
# error reaches every heredoc the cluster would be sent.
mkdir -p "$scratch/fail-yq"
printf '#!/bin/sh\necho "Error: no yq here" >&2\nexit 1\n' > "$scratch/fail-yq/yq"
chmod +x "$scratch/fail-yq/yq"
expect_red "yq failing on every body fails the scan" \
    "$(PATH="$scratch/fail-yq:$PATH" scan_run_labels "$kinds" "$fixtures/scripts/labelled.sh")" "^UNPARSED $fixtures/scripts/labelled.sh:1: .*Error: no yq here"

# A new `export NAME_YAML="key: ..."` line in helpers.sh renders as its entry
# with no edit to the scan.
mkdir -p "$scratch/x-yaml/scripts"
cp "$helpers" "$scratch/x-yaml/helpers.sh"
# shellcheck disable=SC2016 # the line is written to a helpers.sh copy as text
printf '%s\n' 'export E2E_X_YAML="x.io/y: \"$Z\""' >> "$scratch/x-yaml/helpers.sh"
printf '%s\n' "$apply" "${module_labelled//app.kubernetes.io\/part-of: e2e/\$\{E2E_X_YAML\}}" EOF > "$scratch/x-yaml/scripts/x-yaml.sh"
x_scan="$(yaml_entries="$(yaml_entries_of "$scratch/x-yaml/helpers.sh")" scan_run_labels "$kinds" "$scratch/x-yaml/scripts")"
if [ "$x_scan" = "SITE $scratch/x-yaml/scripts/x-yaml.sh:1" ]; then
    pass "a YAML entry added to helpers.sh renders as its key and a value"
else
    fail "a heredoc using a YAML entry added to helpers.sh: scan was: ${x_scan:-nothing}"
fi
expect_red "the same heredoc against helpers.sh without that entry fails the scan" \
    "$(scan_run_labels "$kinds" "$scratch/x-yaml/scripts")" "^UNPARSED $scratch/x-yaml/scripts/x-yaml.sh:1: "
expect_red "a helpers.sh without the run label's entry fails the scan" \
    "$(yaml_entries="E2E_JOB_LABEL_YAML=cfgd.io/e2e-job" scan_run_labels "$kinds" "$fixtures/scripts/labelled.sh")" "^UNREADABLE helpers.sh has no export E2E_RUN_LABEL_YAML="

printf 'a: 1\n' > "$scratch/no-kinds.yaml"
expect_red "a CRD file that names no kinds fails the kind list" "$(operator_kinds "$scratch/no-kinds.yaml")" "^FAIL $scratch/no-kinds.yaml names no CRD kinds"
expect_red "a CRD file yq cannot read fails the kind list" "$(operator_kinds "$scratch/no-such.yaml")" "^FAIL yq could not read $scratch/no-such.yaml"

# require_release_webhooks_scoped against a stub kubectl that serves
# $WH_DIR/<name>.json: a get with --ignore-not-found prints nothing for a name
# with no file, and a name listed in WH_UNREADABLE fails every get.
wh_fixtures="$here/fixtures/release-webhooks"
mkdir -p "$scratch/wh-bin"
cat > "$scratch/wh-bin/kubectl" <<'STUB'
#!/usr/bin/env bash
[ "$1" = get ] || exit 1
for name in ${WH_UNREADABLE:-}; do
    if [ "$3" = "$name" ]; then
        echo "Error from server (Forbidden): $2 \"$3\" is forbidden" >&2
        exit 1
    fi
done
file="$WH_DIR/$3.json"
[ -f "$file" ] || exit 0
case "$*" in
    *tracking-id*) if grep -q 'argocd.argoproj.io/tracking-id' "$file"; then printf 'tracked'; fi ;;
    *) cat "$file" ;;
esac
STUB
chmod +x "$scratch/wh-bin/kubectl"

# wh_case <validating fixture|-> <mutating fixture|-> [VAR=value...]: runs the
# check with each fixture served under its configuration's name (- serves
# none) and prints its output, then `rc=<status>`.
# shellcheck disable=SC2016 # the inner script expands its own positional args
wh_case() {
    local dir
    dir="$(mktemp -d "$scratch/wh.XXXXXX")"
    [ "$1" = - ] || cp "$wh_fixtures/$1" "$dir/cfgd-validating-webhooks.json"
    [ "$2" = - ] || cp "$wh_fixtures/$2" "$dir/cfgd-mutating-webhooks.json"
    shift 2
    env -u GITHUB_RUN_ID -u CFGD_NAMESPACE PATH="$scratch/wh-bin:$PATH" WH_DIR="$dir" \
        REGISTRY=r.example CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"; rc=0; require_release_webhooks_scoped 2>&1 || rc=$?; echo "rc=$rc"' _ "$e2e_root"
}

# expect_wh <label> <want output> <case args...>; jq's own error text differs
# between jq versions, so its lines are left out of the comparison.
expect_wh() {
    local label="$1" want="$2" got
    shift 2
    got="$(wh_case "$@" | grep -v '^jq: ')"
    if [ "$got" = "$want" ]; then pass "$label"; else fail "$label: got [$got], want [$want]"; fi
}

wh_rescope="ERROR: a setup from a branch without the PR-install scoping re-applied the release webhooks; rerun setup from this branch."
expect_wh "require_release_webhooks_scoped passes when every release webhook entry leaves run-labelled objects and namespaces alone" \
    "rc=0" validating-scoped.json mutating-scoped.json
expect_wh "require_release_webhooks_scoped names each validating entry whose objectSelector lacks the run-label DoesNotExist expression" \
    "$wh_rescope validatingwebhookconfiguration/cfgd-validating-webhooks entries whose objectSelector lacks cfgd.io/e2e-run DoesNotExist: validate-module.cfgd.io validate-driftalert.cfgd.io
rc=1" validating-unscoped.json mutating-scoped.json
expect_wh "require_release_webhooks_scoped names the mutating entry whose namespaceSelector lacks the run-label DoesNotExist expression, even when its objectSelector holds it" \
    "$wh_rescope mutatingwebhookconfiguration/cfgd-mutating-webhooks entries whose namespaceSelector lacks cfgd.io/e2e-run DoesNotExist: inject-modules.cfgd.io
rc=1" validating-scoped.json mutating-unscoped.json
expect_wh "require_release_webhooks_scoped stops on a configuration ArgoCD tracks, as setup does" \
    "ERROR: validatingwebhookconfiguration/cfgd-validating-webhooks carries an argocd.argoproj.io/tracking-id annotation, so ArgoCD owns it and setup does not scope it. Add the cfgd.io/e2e-run DoesNotExist selectors from setup-cluster.sh's webhook step to its manifest in the GitOps repo, drop it from that step's heredoc, then rerun setup from this branch.
rc=1" validating-tracked.json mutating-scoped.json
expect_wh "require_release_webhooks_scoped stops on a missing configuration" \
    "ERROR: mutatingwebhookconfiguration/cfgd-mutating-webhooks is missing; setup applies it, so rerun setup from this branch.
rc=1" validating-scoped.json -
expect_wh "require_release_webhooks_scoped stops on a configuration it cannot read" \
    "Error from server (Forbidden): validatingwebhookconfiguration \"cfgd-validating-webhooks\" is forbidden
ERROR: could not read validatingwebhookconfiguration/cfgd-validating-webhooks. Check that the runner can get validatingwebhookconfiguration objects, then rerun setup from this branch.
rc=1" validating-scoped.json mutating-scoped.json WH_UNREADABLE=cfgd-validating-webhooks
expect_wh "require_release_webhooks_scoped stops on a configuration whose webhooks are not JSON" \
    "ERROR: could not read the webhooks of mutatingwebhookconfiguration/cfgd-mutating-webhooks as JSON: kubectl returned something other than JSON, or jq is not on PATH. Check both, then rerun setup from this branch.
rc=1" validating-scoped.json garbled.json

# The suites that apply run-labelled objects call the check before any case.
for suite in "${run_label_suites[@]}"; do
    rc=0
    grep -qE '^require_release_webhooks_scoped( |$)' "$e2e_root/$suite/scripts/"setup-*-env.sh 2>/dev/null || rc=$?
    if [ "$rc" -gt 1 ]; then
        fail "could not read the $suite suite's setup-*-env.sh to see whether it calls require_release_webhooks_scoped"
    elif [ "$rc" -eq 0 ]; then
        pass "the $suite suite's setup calls require_release_webhooks_scoped"
    else
        fail "the $suite suite's setup does not call require_release_webhooks_scoped before its cases; a setup from a branch without the PR-install scoping would go unseen"
    fi
done

# An ERROR on stdout is lost where a caller captures or discards stdout, and
# interleaves with the output a case parses. scan_stdout_errors FILE... prints
# `STDOUT file:line` for each echo or printf of an ERROR line, outside comments
# and heredoc bodies, that is not sent to stderr. A statement is a line with
# the lines a trailing backslash or an open quote joins to it, and its
# commands are split at each ;, &&, ||, | and & outside quotes, read off
# squash's mask; a >& or <& and an &> are redirects. A command's word is read
# past assignments, redirects, a function header (f() or function f), a case
# pattern, the openers { ( if while until for select and case ... in, and the
# prefix words ! then do else elif time command builtin exec. A command passes
# with its own >&2, 1>&2 or >/dev/stderr, or when a compound enclosing it is
# redirected so at its closer (}, ), fi, done or esac): one later on the
# statement, or else on a later line indented less. Such a line that is not a
# closer, else, elif, then, do, ;; or a case pattern ends the compounds
# indented more than it, unless it continues the line before it (after an open
# quote or a trailing backslash), where its indent says nothing.
# shellcheck disable=SC2016 # an awk program; the $ fields belong to awk
scan_stdout_errors() {
    { awk -f "$here/heredocs.awk" "$@" || echo "UNREADABLE heredocs.awk exited $?"; } | awk -F '\t' "$squash_awk"'
        function indent(s) { match(s, /^[ \t]*/); return RLENGTH }
        function to_stderr(m) { return m ~ /(^|[^0-9])1?(>&2|>>?[ \t]*\/dev\/stderr)([^0-9A-Za-z_]|$)/ }
        # lead(m): the offset in the masked command m of its command word,
        # past any assignment, redirect, function header, case pattern and
        # prefix word. opens counts the compounds it opens there.
        function lead(m,   p, r) {
            p = 1; opens = 0
            for (;;) {
                r = substr(m, p)
                if (match(r, /^[ \t\n]+/) ||
                    match(r, /^(function[ \t]+[^ \t\n()]+([ \t]*\(\))?|[A-Za-z_][A-Za-z0-9_:.-]*[ \t]*\(\))/) ||
                    match(r, /^\(?[^ \t\n()]+\)/) ||
                    match(r, /^[0-9]*(>>?|<|>&|<&|&>>?)[ \t]*[^ \t\n<>&]+/) ||
                    match(r, /^[A-Za-z_][A-Za-z0-9_]*\+?=[^ \t\n]*/) ||
                    match(r, /^(!|then|do|else|elif|time([ \t]+-p)?|command|builtin|exec)([ \t\n]|$)/)) { p += RLENGTH; continue }
                if (match(r, /^(case[ \t]+[^ \t\n]+[ \t]+in|[{]|if|while|until|for|select)([ \t\n]|$)/)) { p += RLENGTH; opens++; continue }
                if (r ~ /^\(([^(]|$)/) { p++; opens++; continue }
                return p
            }
        }
        # closer(m): the length of the }, fi, done or esac that starts m, or 0.
        function closer(m) {
            if (!match(m, /^[ \t\n]*([}]|fi|done|esac)/)) return 0
            return substr(m, RLENGTH + 1, 1) ~ /^[A-Za-z0-9_]$/ ? 0 : RLENGTH
        }
        # close_group(rest): a compound closes, with rest after its closer. One
        # opened after the judged command lowers the depth d; one closing at
        # depth 0 encloses the command, and passes it when rest redirects to
        # stderr.
        function close_group(rest) {
            if (d > 0) d--
            else if (to_stderr(rest)) ok = 1
        }
        # commands(k, m): the commands of the statement whose kept text is k
        # and mask m, as cmd[1..nc], with their masks in cmk[] and in cl[] the
        # statement line each starts on, counted from 0.
        function commands(k, m,   i, c, p, start, line) {
            nc = 0; start = 1; line = 0
            for (i = 1; i <= length(m); i++) {
                c = substr(m, i, 1); p = (i > 1) ? substr(m, i - 1, 1) : ""
                if (c == "&" && (p == ">" || p == "<" || substr(m, i + 1, 1) == ">")) continue
                if (c == "\n" && p == "\\") continue
                if (c != ";" && c != "&" && c != "|" && c != "\n") continue
                cut(k, m, start, i, line)
                if ((c == "&" || c == "|") && substr(m, i + 1, 1) ~ /[&|]/) i++
                start = i + 1
            }
            cut(k, m, start, length(m) + 1, line)
        }
        function cut(k, m, a, b, line,   t) {
            t = substr(k, 1, a - 1); gsub(/[^\n]/, "", t)
            cmd[++nc] = substr(k, a, b - a); cmk[nc] = substr(m, a, b - a); cl[nc] = length(t)
        }
        function judge(first, last,   i, j, k, m, r, p, b, x, ind) {
            k = kept_at[first]; m = mask_at[first]
            for (i = first + 1; i <= last; i++) {
                k = k "\n" kept_at[i]
                m = m (quoted_at[i - 1] ? "Q" : "\n") mask_at[i]
            }
            commands(k, m)
            for (i = 1; i <= nc; i++) {
                if (substr(cmk[i], lead(cmk[i])) !~ /^(echo|printf)([ \t]|$)/ || cmd[i] !~ /ERROR/) continue
                if (to_stderr(cmk[i])) continue
                ok = 0; d = 0
                for (j = i + 1; j <= nc && !ok; j++) {
                    r = cmk[j]
                    if ((p = closer(r))) { r = substr(r, p + 1); close_group(r) }
                    else { r = substr(r, lead(r)); d += opens }
                    b = 0
                    for (x = 1; x <= length(r) && !ok; x++) {
                        if (substr(r, x, 1) == "(") b++
                        else if (substr(r, x, 1) == ")") { if (b > 0) b--; else close_group(substr(r, x + 1)) }
                    }
                }
                ind = indent(raw[first + cl[i]])
                for (j = last + 1; j <= n && !ok; j++) {
                    if (open_at[j - 1] || mask_at[j - 1] ~ /\\$/ || mask_at[j] ~ /^[ \t]*$/ || indent(raw[j]) >= ind) continue
                    if ((p = closer(mask_at[j])) || match(mask_at[j], /^[ \t]*\)/) && (p = RLENGTH)) {
                        if (to_stderr(substr(mask_at[j], p + 1))) ok = 1
                        else ind = indent(raw[j])
                    }
                    else if (mask_at[j] !~ /^[ \t]*((else|elif|then|do)([^A-Za-z0-9_]|$)|;;|\(?[^ \t\n()]+\))/) ind = indent(raw[j])
                }
                if (!ok) print "STDOUT " file ":" num[first + cl[i]]
            }
        }
        function judge_file(   i, first) {
            inq = ""; cq = ""; qbuf = ""; ansi = 0
            for (i = 1; i <= n; i++) {
                squash(raw[i], i)
                kept_at[i] = kept; mask_at[i] = mask
                quoted_at[i] = (inq != ""); open_at[i] = (inq != "" || cq != "")
            }
            for (first = 1; first <= n; first = i + 1) {
                for (i = first; i < n && (open_at[i] || mask_at[i] ~ /\\$/); i++) ;
                judge(first, i)
            }
            n = 0
        }
        /^UNREADABLE / { print; next }
        $1 == "FILE" { judge_file(); file = $2; next }
        $1 == "SH" { r = $0; sub(/^[^\t]*\t[^\t]*\t[^\t]*\t/, "", r); raw[++n] = r; num[n] = $3 }
        END { judge_file() }
    '
}

stderr_fixtures="$here/fixtures/stderr-errors"
stderr_got="$(cd "$stderr_fixtures" && scan_stdout_errors hits.bash allowed.bash 2>&1)"
stderr_want="$(printf 'STDOUT hits.bash:%s\n' 1 2 3 5 10 13 14 15 16 18 19 23 24 25 26 27 29 30 31 33 34 36 37 38 39 40 41 42 43 44 45 46 47 50 54)"
if [ "$stderr_got" = "$stderr_want" ]; then
    pass "the stderr scan reports each planted ERROR echo or printf on stdout, in a bare statement, an unredirected group or function, after an option or &&, across a continued line or a quoted newline, before a later group of its own, after a redirected command on its line, in a one-line group closed without a redirect or around an inner group that alone is redirected, beside a quoted >&2 or >&20, after a function header, a case pattern, an assignment, a redirect, if and each prefix word, and in an if, loop, subshell or function closed without a redirect or ended by a plain line, and stays quiet on >&2, 1>&2, >/dev/stderr, separators inside quotes, groups, function bodies, ifs, loops, cases and subshells redirected at their closer on one line or several, with a string or command continued at column 0 inside them, branches before an else and case arms before another arm inside them, one-line functions and case arms with their own redirect, comments, quoted text and heredoc bodies"
else
    fail "the stderr scan printed [$stderr_got], want [$stderr_want]"
fi
stderr_after_open="$(cd "$stderr_fixtures" && scan_stdout_errors open-quote.bash hits.bash allowed.bash 2>&1)"
if [ "$stderr_after_open" = "$stderr_want" ]; then
    pass "the stderr scan starts each file outside quotes, after one that ends inside a quote"
else
    fail "the stderr scan after a file that ends inside a quote printed [$stderr_after_open], want [$stderr_want]"
fi
stderr_bad="$(scan_stdout_errors "$stderr_fixtures/absent.bash" 2>/dev/null)"
if [[ "$stderr_bad" == UNREADABLE* ]]; then
    pass "the stderr scan reports a file it cannot read"
else
    fail "the stderr scan over a missing file printed [$stderr_bad], want an UNREADABLE line"
fi
if [ "${#e2e_scripts[@]}" -eq 0 ]; then
    fail "git ls-files 'tests/e2e/*.sh' matched no script, so the stderr scan read nothing"
elif stdout_errors="$(cd "$repo_root" && scan_stdout_errors "${e2e_scripts[@]}")" && [ -z "$stdout_errors" ]; then
    pass "every ERROR line the e2e scripts print goes to stderr (${#e2e_scripts[@]} scripts)"
else
    fail "an ERROR on stdout is lost where stdout is captured; add >&2 to: [${stdout_errors:-the scan failed}]"
fi

# A release target spelled by hand in the operator or full-stack suite reaches
# the live release. helpers.sh names this run's install. The scan reads each
# suite's tracked *.sh files only: the YAML under operator/manifests/ is the
# release operator's own definition, which setup applies where ArgoCD does not
# run it.
# The resource words are kubectl's spellings of a Deployment, Endpoints and a
# Service, singular, plural, short and group-qualified.
q="[\"']?"
release_target='cfgd-system([^[:alnum:]-]|$)'
release_target+='|\$\{?CFGD_NAMESPACE([^[:alnum:]_]|$)'
release_target+='|app(\.kubernetes\.io/name)?='"$q"'cfgd-operator([^[:alnum:]-]|$)'
release_target+='|(^|[^[:alnum:]_.-])(deploy|deployments?(\.apps)?|endpoints|ep|svc|services?)[/ ]+'"$q"'cfgd-operator([^[:alnum:]-]|$)'
release_target+='|cfgd-(validating|mutating)-webhooks|csi\.cfgd\.io'
# scan_release_targets FILE...: file:line:text of each line naming one.
scan_release_targets() {
    local rc=0
    grep -HnE "$release_target" "$@" || rc=$?
    [ "$rc" -le 1 ]
}
targets_got="$(cd "$here/fixtures/release-targets" && scan_release_targets spelled.bash clean.bash | cut -d: -f1,2)" ||
    targets_got="(the scan failed)"
targets_want="$(seq -f 'spelled.bash:%g' 1 26)"
if [ "$targets_got" = "$targets_want" ]; then
    pass "the release-target scan reports the release namespace, CFGD_NAMESPACE, both operator label selectors, every kubectl spelling of the operator Deployment, Endpoints and Service, bare or quoted, the webhook configurations and the CSI driver, in code, comments and heredocs, and stays quiet on the PR install's names, the leader lease, longer names that start the same and other cfgd.io names"
else
    fail "the release-target scan printed [$targets_got], want [$targets_want]"
fi
if scan_release_targets "$here/fixtures/release-targets/absent.bash" >/dev/null 2>&1; then
    fail "the release-target scan passed over a file it could not read"
else
    pass "the release-target scan fails over a file it cannot read"
fi
# release_target_verdict KEPT FILE...: KEPT holds one tab-separated file and
# text row per line it clears, so a text kept once clears one line only. Prints
# `HIT file:line:text` for each line naming a release target, leading blanks
# dropped from its text, beyond the rows KEPT holds for it, then `STALE entry`
# for each row left unused. KEPT reaches awk through the environment, since -v
# would read a trailing backslash as an escape. Fails only when a file cannot
# be read.
release_target_verdict() {
    local kept="$1" hits
    shift
    hits="$(scan_release_targets "$@")" || return 1
    KEPT="$kept" awk 'BEGIN { n = split(ENVIRON["KEPT"], k, "\n"); for (i = 1; i <= n; i++) if (k[i] != "") rows[k[i]]++ }
        $0 != "" {
            file = $0; sub(/:.*/, "", file)
            text = $0; sub(/^[^:]*:[0-9]+:[ \t]*/, "", text)
            key = file "\t" text
            if (used[key] < rows[key]) used[key]++; else print "HIT " $0
        }
        END { for (i = 1; i <= n; i++) if (k[i] != "" && ++unused[k[i]] > used[k[i]]) print "STALE " k[i] }' <<<"$hits"
}
kept_got="$(cd "$here/fixtures/release-targets" && release_target_verdict "$(cat kept.tsv)" kept.bash)" ||
    kept_got="(the scan failed)"
kept_want="HIT kept.bash:4:kubectl get deployment cfgd-operator -n cfgd-system
HIT kept.bash:5:echo \"the release namespace is cfgd-system.\"
HIT kept.bash:6:kubectl get deployment cfgd-server -n cfgd-system
STALE kept.bash	kubectl get pods -n cfgd-system
STALE kept.bash	wait_for_k8s_field machineconfig mc-1 cfgd-system"
if [ "$kept_got" = "$kept_want" ]; then
    pass "the release-target verdict clears one line per kept row, indented or not and with a trailing backslash, and reports every other line, a repeat beyond its rows and each row no line uses"
else
    fail "the release-target verdict printed [$kept_got], want [$kept_want]"
fi
if release_target_verdict "" "$here/fixtures/release-targets/absent.bash" >/dev/null 2>&1; then
    fail "the release-target verdict passed over a file it could not read"
else
    pass "the release-target verdict fails over a file it cannot read"
fi

# release-targets-kept.tsv lists the lines that name a release target on
# purpose, one row per line, each with its reason.
kept_rc=0
release_target_kept="$(grep -v -e '^#' -e '^$' "$here/release-targets-kept.tsv")" || kept_rc=$?
if [ "$kept_rc" -eq 1 ]; then
    fail "no entries in $here/release-targets-kept.tsv"
elif [ "$kept_rc" -gt 1 ]; then
    fail "could not read $here/release-targets-kept.tsv"
fi

# Each suite that drives the PR install has a floor of scripts; a pathspec
# that matches fewer fails the check.
release_target_suites=(operator full-stack)
declare -A release_target_floors=([operator]=12 [full-stack]=10)
for suite in "${release_target_suites[@]}"; do
    if [ -z "${release_target_floors[$suite]:-}" ]; then
        fail "release_target_floors has no floor for the $suite suite"
        continue
    fi
    mapfile -t suite_scripts < <(git -C "$repo_root" ls-files "tests/e2e/$suite/*.sh")
    if [ "${#suite_scripts[@]}" -lt "${release_target_floors[$suite]}" ]; then
        fail "git ls-files 'tests/e2e/$suite/*.sh' matched ${#suite_scripts[@]} scripts, fewer than the floor of ${release_target_floors[$suite]}, so the release-target scan missed part of the $suite suite"
        continue
    fi
    suite_kept="$(grep "^tests/e2e/$suite/" <<<"$release_target_kept" || true)" # rc-ok: a suite with no kept entry is valid
    if ! release_verdict="$(cd "$repo_root" && release_target_verdict "$suite_kept" "${suite_scripts[@]}")"; then
        fail "the release-target scan could not read the $suite suite"
    elif [ -z "$release_verdict" ]; then
        pass "no $suite suite script names a release target by hand outside its kept list (${#suite_scripts[@]} scripts)"
    else
        fail "the $suite suite drives the PR install; use \$E2E_INSTALL_NS, \$E2E_OPERATOR_PODS, \$E2E_OPERATOR_DEPLOY, \$E2E_WEBHOOK_SVC, \$E2E_CSI_DS, \$E2E_CSI_PODS, \$E2E_VALIDATING_WEBHOOK, \$E2E_MUTATING_WEBHOOK or \$CSI_DRIVER_NAME from helpers.sh. A line that names the release on purpose goes in common/release-targets-kept.tsv with its reason; a kept entry that matches no line is removed or updated. [$release_verdict]"
    fi
done
unscanned_kept="$(grep -vE "^tests/e2e/($(IFS='|'; echo "${release_target_suites[*]}"))/" <<<"$release_target_kept" || true)" # rc-ok: no entry outside the scanned suites is the passing outcome
if [ -n "$unscanned_kept" ]; then
    fail "release-targets-kept.tsv lists lines of a file the release-target scan does not read, so they can never go stale; remove them: [$unscanned_kept]"
fi

# Setup stops when the PR install or a tool the suite needs is missing, so no
# full-stack case has a reason left to skip. scan_skip_tests FILE...: file:line
# of each skip_test call; fails only when a file cannot be read.
scan_skip_tests() {
    local rc=0
    grep -HnwF skip_test "$@" || rc=$?
    [ "$rc" -le 1 ]
}
skips_got="$(cd "$here/fixtures/skip-tests" && scan_skip_tests skips.bash | cut -d: -f1,2)" ||
    skips_got="(the scan failed)"
if [ "$skips_got" = "skips.bash:2
skips.bash:5" ]; then
    pass "the skip scan reports each skip_test call, on its own line or after ||, and stays quiet on longer names and prose"
else
    fail "the skip scan printed [$skips_got], want [skips.bash:2 skips.bash:5]"
fi
if scan_skip_tests "$here/fixtures/skip-tests/absent.bash" >/dev/null 2>&1; then
    fail "the skip scan passed over a file it could not read"
else
    pass "the skip scan fails over a file it cannot read"
fi
mapfile -t fullstack_scripts < <(git -C "$repo_root" ls-files 'tests/e2e/full-stack/*.sh')
if [ "${#fullstack_scripts[@]}" -lt "${release_target_floors[full-stack]}" ]; then
    fail "git ls-files 'tests/e2e/full-stack/*.sh' matched ${#fullstack_scripts[@]} scripts, fewer than the floor of ${release_target_floors[full-stack]}, so the skip scan missed part of the suite"
elif ! fullstack_skips="$(cd "$repo_root" && scan_skip_tests "${fullstack_scripts[@]}")"; then
    fail "the skip scan could not read the full-stack suite"
elif [ -z "$fullstack_skips" ]; then
    pass "no full-stack case calls skip_test (${#fullstack_scripts[@]} scripts)"
else
    fail "a full-stack case skips; setup stops when what it needs is missing, so the case fails instead: [$fullstack_skips]"
fi

# The full-stack Helm suite installs the chart beside the PR install; an
# install without HELM_SCOPE runs an operator, validating webhook and pod
# injector that reconcile, admit and inject across every run. scan_helm_scope
# FILE...: for each helm install or upgrade in command position, its
# backslash-continued lines joined, `SCOPED file:line` when it passes
# "${HELM_SCOPE[@]}" and no later flag sets one of the keys HELM_SCOPE sets or
# their parent, otherwise `UNSCOPED file:line`. A file's first HELM_SCOPE=(
# array gives `SCOPEDEF file:line` when it holds exactly the three flags that
# set those keys to the namespace label, otherwise `BADSCOPE file:line` with
# `missing: KEY...` and `extra: TEXT`; Helm applies a later --set over an
# earlier one, so an extra element can undo a wanted one. Any other line that
# names HELM_SCOPE beyond expanding "${HELM_SCOPE[@]}" (an append, a second
# definition, an element write, an unset) is `UNSCOPED file:line`. Fails only
# when a file cannot be read.
scan_helm_scope() {
    awk '
        BEGIN {
            scope = "\"${HELM_SCOPE[@]}\""
            key[1] = "operator.watchLabelSelector"
            pair[1] = "--set-string \"operator.watchLabelSelector=cfgd.io/e2e-helm=${HELM_NS}\""
            key[2] = "webhook.objectSelector"
            pair[2] = "--set-json \"webhook.objectSelector={\\\"matchLabels\\\":{\\\"cfgd.io/e2e-helm\\\":\\\"${HELM_NS}\\\"}}\""
            key[3] = "mutatingWebhook.namespaceSelector"
            pair[3] = "--set-json \"mutatingWebhook.namespaceSelector={\\\"matchExpressions\\\":null,\\\"matchLabels\\\":{\\\"cfgd.io/e2e-helm\\\":\\\"${HELM_NS}\\\"}}\""
        }
        FNR == 1 { cmd = ""; def = ""; defined = 0 }
        def == "" && !defined && /^[[:space:]]*HELM_SCOPE=\(/ { def = " "; defstart = FNR; defined = 1 }
        def != "" {
            def = def $0 " "
            if ($0 !~ /\)[[:space:]]*$/) next
            body = def
            sub(/^[[:space:]]*HELM_SCOPE=\(/, "", body)
            sub(/\)[[:space:]]*$/, "", body)
            gsub(/[[:space:]]+/, " ", body)
            missing = ""
            for (k = 1; k <= 3; k++) {
                at = index(body, pair[k])
                if (at) body = substr(body, 1, at - 1) " " substr(body, at + length(pair[k]))
                else missing = missing " " key[k]
            }
            gsub(/  +/, " ", body)
            sub(/^ /, "", body)
            sub(/ $/, "", body)
            if (missing == "" && body == "") print "SCOPEDEF " FILENAME ":" defstart
            else print "BADSCOPE " FILENAME ":" defstart (missing == "" ? "" : " missing:" missing) (body == "" ? "" : " extra: " body)
            def = ""
            next
        }
        {
            if (cmd == "") start = FNR
            line = $0
            if (sub(/\\$/, "", line)) { cmd = cmd line " "; next }
            cmd = cmd line
            rest = cmd
            while ((at = index(rest, scope))) rest = substr(rest, 1, at - 1) substr(rest, at + length(scope))
            stray = rest ~ /HELM_SCOPE/ && cmd !~ /^[[:space:]]*#/
            if (cmd ~ /(^|[;&|(!]|[^[:alnum:]_](if|then|else|elif|do|while|until)|^(if|then|else|elif|do|while|until))[[:space:]]*helm[[:space:]]+(install|upgrade)([[:space:]]|$)/) {
                at = index(cmd, scope)
                ok = at && !stray && substr(cmd, at + length(scope)) !~ /(^|[^[:alnum:]_.])(operator|webhook|mutatingWebhook)(\.(watchLabelSelector|objectSelector|namespaceSelector)[^=[:space:]]*)?=/
                print (ok ? "SCOPED " : "UNSCOPED ") FILENAME ":" start
            } else if (stray) print "UNSCOPED " FILENAME ":" start
            cmd = ""
        }
    ' "$@"
}
scope_got="$(cd "$here/fixtures/helm-scope" && scan_helm_scope installs.bash scope-exact.bash scope-extra.bash scope-missing.bash scope-no-null.bash scope-wrong-label.bash)" || scope_got="(the scan failed)"
scope_want="$(cat "$here/fixtures/helm-scope/want.txt")"
if [ "$scope_got" = "$scope_want" ]; then
    pass "the Helm scope scan reports each Helm install and upgrade, on one line or continued, at line start, after \$(, ! or &&, as scoped only when it passes \"\${HELM_SCOPE[@]}\" and no later flag sets one of its keys or their parent, reports a file's first HELM_SCOPE array that holds anything but its three flags, each append, second definition, element write or unset of it, and stays quiet on comments, quoted text, uninstall, template and longer words"
else
    fail "the Helm scope scan printed [$scope_got], want [$scope_want]"
fi
if scan_helm_scope "$here/fixtures/helm-scope/absent.bash" >/dev/null 2>&1; then
    fail "the Helm scope scan passed over a file it could not read"
else
    pass "the Helm scope scan fails over a file it cannot read"
fi
# Eight installs and FS-HELM-05's upgrade.
declare -A helm_scope_floors=([tests/e2e/full-stack/scripts/test-helm.sh]=9)
for scope_file in "${!helm_scope_floors[@]}"; do
    if ! git -C "$repo_root" ls-files --error-unmatch "$scope_file" >/dev/null 2>&1; then
        fail "$scope_file is not tracked, so the Helm scope scan cannot hold it to its floor"
    fi
done
if ! scope_verdict="$(cd "$repo_root" && scan_helm_scope "${fullstack_scripts[@]}")"; then
    fail "the Helm scope scan could not read the full-stack suite"
else
    scope_short=""
    for scope_file in "${!helm_scope_floors[@]}"; do
        scope_sites="$(grep -cE "^(UN)?SCOPED $scope_file:" <<<"$scope_verdict" || true)" # rc-ok: zero sites is reported against the floor below
        if [ "$scope_sites" -lt "${helm_scope_floors[$scope_file]}" ]; then
            scope_short+=" $scope_file has $scope_sites Helm installs and upgrades, fewer than its floor of ${helm_scope_floors[$scope_file]};"
        fi
        if ! grep -q "^SCOPEDEF $scope_file:" <<<"$scope_verdict"; then
            scope_short+=" $scope_file defines no complete HELM_SCOPE array;"
        fi
    done
    scope_unscoped="$(grep -E '^(UNSCOPED|BADSCOPE) ' <<<"$scope_verdict" || true)" # rc-ok: no unscoped install or incomplete array is the passing outcome
    if [ -n "$scope_unscoped" ]; then
        fail "a full-stack Helm install or upgrade runs an operator, webhook or pod injector that acts on every run's objects; pass \"\${HELM_SCOPE[@]}\" from helm_test_ns with no later flag setting a key it sets, keep the array exactly its three flags set to the namespace label, and write it nowhere else: [$scope_unscoped]"
    elif [ -n "$scope_short" ]; then
        fail "the Helm scope scan has lost its population:$scope_short"
    else
        pass "every full-stack Helm install and upgrade passes \"\${HELM_SCOPE[@]}\" and overrides none of its keys, and HELM_SCOPE scopes the operator, validating webhook and pod injector ($(grep -cE '^(UN)?SCOPED ' <<<"$scope_verdict") sites in ${#fullstack_scripts[@]} scripts)"
    fi
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all PR-install naming checks passed"
