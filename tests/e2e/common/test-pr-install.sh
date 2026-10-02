#!/usr/bin/env bash
# Checks without a cluster that helpers.sh names the PR-owned install (its
# release, namespace, workloads, pod selectors and CSI driver) from one run id,
# that a local run id is the same in every process of one checkout, and that
# ensure_namespace and running_image address the namespaces they are given, and
# that every cfgd.io object the operator and full-stack suites apply carries the
# run label. kubectl is a stub on PATH that logs its arguments, so nothing reaches a cluster.
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

# Every cfgd.io object a suite applies must carry the run label in its own
# metadata.labels: the PR operator reconciles only objects with that label, so an
# unlabelled object makes its case fail for a reason unrelated to the case.
#
# scan_run_labels <dir...> reads every file in each dir and prints one line per
# finding, tag first:
#   SITE        a heredoc applied to the cluster that holds a cfgd.io document
#   INPOD       a heredoc written inside a pod (exec_in_pod, kubectl exec): a cfgd
#               config file in the pod, which needs no run label
#   UNLABELLED  a cfgd.io document whose metadata.labels lacks the run label
#   QUOTED      a cfgd.io document in a quoted-delimiter heredoc applied to the
#               cluster, where ${E2E_RUN_LABEL_YAML} cannot expand
#   NOSINK      a heredoc holding a cfgd.io document whose destination the scan
#               does not recognise
#   OUTSIDE     a cfgd.io apiVersion outside any heredoc
#   UNTERMINATED, UNREADABLE, EMPTY   the scan could not read what it was given
scan_run_labels() {
    local dir f files=() found
    for dir in "$@"; do
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
    # POSIX awk only: CI runners ship mawk.
    awk '
        function indent(s) { match(s, /^[ \t]*/); return RLENGTH }
        function strip(s) { sub(/^[ \t]+/, "", s); return s }
        function blank_or_comment(s) { s = strip(s); return s == "" || substr(s, 1, 1) == "#" }
        function cfgd_api(s) { return s ~ /^apiVersion:[ \t]*["\047]?cfgd\.io\// }
        function check_doc(first, last,    i, base, s, api, kind, name, inmeta, mchild, inlab, lind, labelled) {
            base = -1
            for (i = first; i <= last; i++) {
                if (blank_or_comment(body[i])) continue
                if (base < 0) base = indent(body[i])
                if (indent(body[i]) != base) continue
                s = strip(body[i])
                if (cfgd_api(s)) api = bline[i]
                if (s ~ /^kind:/) { kind = s; sub(/^kind:[ \t]*/, "", kind) }
            }
            if (!api) return 0
            for (i = first; i <= last; i++) {
                if (blank_or_comment(body[i])) continue
                s = strip(body[i])
                if (indent(body[i]) <= base) {
                    inmeta = (indent(body[i]) == base && s ~ /^metadata:[ \t]*$/)
                    mchild = -1; inlab = 0
                    continue
                }
                if (!inmeta) continue
                if (mchild < 0) mchild = indent(body[i])
                if (indent(body[i]) == mchild) {
                    if (s ~ /^name:/ && name == "") { name = s; sub(/^name:[ \t]*/, "", name) }
                    inlab = (s ~ /^labels:/)
                    lind = indent(body[i])
                    if (inlab && s ~ /cfgd\.io\/e2e-run|E2E_RUN_LABEL_YAML/) labelled = 1
                    continue
                }
                if (inlab && indent(body[i]) > lind && s ~ /^(cfgd\.io\/e2e-run|["\047]cfgd\.io\/e2e-run|\$\{?E2E_RUN_LABEL_YAML)/) labelled = 1
            }
            if (class == "INPOD") return 1
            if (class == "NOSINK") return 1
            if (quoted) {
                print "QUOTED " FILENAME ":" api " " kind " " name ": the heredoc delimiter is quoted, so ${E2E_RUN_LABEL_YAML} cannot expand"
            } else if (!labelled) {
                print "UNLABELLED " FILENAME ":" api " " kind " " name ": metadata.labels has no cfgd.io/e2e-run; add ${E2E_RUN_LABEL_YAML}"
            }
            return 1
        }
        function close_heredoc(    i, first, has) {
            first = 1
            for (i = 1; i <= n + 1; i++) {
                if (i == n + 1 || body[i] ~ /^---([ \t]|$)/) {
                    if (check_doc(first, i - 1)) has = 1
                    first = i + 1
                }
            }
            if (has && class == "SITE") print "SITE " FILENAME ":" open
            if (has && class == "INPOD") print "INPOD " FILENAME ":" open " (" delim "): a cfgd config file written inside a pod, which needs no run label"
            if (has && class == "NOSINK") print "NOSINK " FILENAME ":" open ": a heredoc holding a cfgd.io document that is not piped to kubectl apply/create/replace or apply_yaml"
            inh = 0; n = 0
        }
        function unterminated() { print "UNTERMINATED " curfile ":" open ": no " delim " line closes this heredoc"; inh = 0 }
        FNR == 1 { if (inh) unterminated(); curfile = FILENAME; cont = "" }
        inh {
            if ($0 == delim || (class == "INPOD" && ($0 == delim "\047" || $0 == delim "\""))) { close_heredoc(); next }
            body[++n] = $0; bline[n] = FNR
            next
        }
        blank_or_comment($0) { cont = ""; next }
        {
            cmd = cont $0
            cont = ($0 ~ /\\$/) ? substr(cmd, 1, length(cmd) - 1) " " : ""
            if (match($0, /<<[ \t]*[\\\047"]?[A-Za-z_][A-Za-z0-9_]*/) && substr($0, RSTART + 2, 1) != "<" && (RSTART == 1 || substr($0, RSTART - 1, 1) != "<")) {
                delim = substr($0, RSTART + 2, RLENGTH - 2)
                sub(/^[ \t]*/, "", delim)
                quoted = (delim ~ /^[\\\047"]/)
                sub(/^[\\\047"]/, "", delim)
                if (cmd ~ /exec_in_pod|kubectl([ \t].*)?[ \t]exec([ \t]|$)/) class = "INPOD"
                else if (cmd ~ /kubectl([ \t].*)?[ \t](apply|create|replace)([ \t]|$)/ || cmd ~ /(^|[^A-Za-z0-9_])apply_yaml([ \t]|$)/) class = "SITE"
                else class = "NOSINK"
                inh = 1; n = 0; open = FNR; cont = ""
                next
            }
            if ($0 ~ /apiVersion"?:[ \t]*["\047]?cfgd\.io\//) print "OUTSIDE " FILENAME ":" FNR ": a cfgd.io apiVersion outside any heredoc"
        }
        END { if (inh) unterminated() }
    ' "${files[@]}" || echo "UNREADABLE awk exited $? reading ${files[*]}"
}

run_label_floor=40

# label_verdict <scan output>: prints nothing when the scan is clean and holds at
# least $run_label_floor sites, otherwise one line per problem.
label_verdict() {
    local out="$1" sites
    sites="$(grep -c '^SITE ' <<<"$out" || true)"
    grep -Ev '^(SITE|INPOD) ' <<<"$out" || true
    if [ "$sites" -lt "$run_label_floor" ]; then
        echo "FLOOR only $sites heredocs apply a cfgd.io object (want at least $run_label_floor); the scan has lost its population"
    fi
}

tree_scan="$(scan_run_labels "$e2e_root/operator/scripts" "$e2e_root/full-stack/scripts")"
tree_verdict="$(label_verdict "$tree_scan")"
if [ -z "$tree_verdict" ]; then
    pass "every cfgd.io object the operator and full-stack suites apply carries the run label ($(grep -c '^SITE ' <<<"$tree_scan") heredocs)"
else
    fail "cfgd.io objects the PR operator would ignore:"
    printf '      %s\n' "${tree_verdict//$'\n'/$'\n'      }"
fi
while IFS= read -r line; do
    [ -n "$line" ] && pass "written inside a pod: ${line#INPOD }"
done < <(grep '^INPOD ' <<<"$tree_scan" || true)

# The scan against planted fixtures, one per way a suite writes a cfgd.io object
# and one per placement of the label. Each body is checked as YAML first, so
# each fixture is an object the scan can judge.
fixtures="$scratch/label-fixtures"
mkdir -p "$fixtures/scripts"
# plant <name> <opener> <body> [closer]
plant() {
    local name="$1" opener="$2" body="$3" closer="${4-EOF}" parsed
    printf '%s\n%s\n%s\n' "$opener" "$body" "$closer" > "$fixtures/scripts/$name.sh"
    parsed="$(printf '%s\n' "${body//\$\{E2E_RUN_LABEL_YAML\}/cfgd.io/e2e-run: \"42\"}" | yq '.' 2>&1 >/dev/null)" ||
        fail "fixture $name is not valid YAML: $parsed"
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
apply="kubectl apply -n \"\$E2E_NAMESPACE\" -f - <<EOF"

plant labelled "$apply" "$module_labelled"
plant label-absent "$apply" 'apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: label-absent
  labels:
    app.kubernetes.io/part-of: e2e
spec:
  packages: []'
plant no-labels "$apply" "$module_unlabelled"
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
plant quoted "kubectl apply -f - <<'EOF'" "$module_labelled"
plant in-pod "exec_in_pod bash -c 'cat > /etc/cfgd/in-pod.yaml << \"INNEREOF\"" 'apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: in-pod
spec:
  profile: base' "INNEREOF'"
plant apply-yaml "apply_yaml \"T01\" <<EOF" "$module_unlabelled"
plant captured "RESULT=\$(kubectl apply -f - 2>&1 <<EOF || true" "$module_unlabelled" 'EOF
)'
plant continued "kubectl apply -n \"\$E2E_NAMESPACE\" \\"$'\n'"    -f - <<EOF" "$module_unlabelled"
plant no-sink "yaml=\$(cat <<EOF" "$module_labelled" "EOF"$'\n'")"$'\n'"echo \"\$yaml\" | kubectl apply -f -"
plant heredoc-unterminated "$apply" "$module_labelled" ''
outside_yaml='apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: outside\n'
printf '%s\n' "printf '$outside_yaml' | kubectl apply -f -" > "$fixtures/scripts/outside.sh"
# shellcheck disable=SC2059 # the payload is the printf format the fixture runs
parsed="$(printf "$outside_yaml" | yq '.' 2>&1 >/dev/null)" || fail "fixture outside is not valid YAML: $parsed"

want_fixture_scan="SITE apply-yaml.sh:1
UNLABELLED apply-yaml.sh:2
SITE captured.sh:1
UNLABELLED captured.sh:2
SITE comment-heredoc.sh:2
UNLABELLED comment-heredoc.sh:3
SITE continued.sh:2
UNLABELLED continued.sh:3
UNTERMINATED heredoc-unterminated.sh:1
INPOD in-pod.sh:1
SITE label-absent.sh:1
UNLABELLED label-absent.sh:2
SITE labelled.sh:1
SITE multi-doc.sh:1
UNLABELLED multi-doc.sh:12
UNLABELLED nested-metadata.sh:2
SITE nested-metadata.sh:1
SITE no-labels.sh:1
UNLABELLED no-labels.sh:2
NOSINK no-sink.sh:1
OUTSIDE outside.sh:1
QUOTED quoted.sh:2
SITE quoted.sh:1"
fixture_scan="$(scan_run_labels "$fixtures/scripts")"
got_fixture_scan="$(awk '{print $1, $2}' <<<"$fixture_scan" | sed "s|$fixtures/scripts/||; s|:\$||" | sort)"
if [ "$got_fixture_scan" = "$(sort <<<"$want_fixture_scan")" ]; then
    pass "the run-label scan reports each planted fixture it should and no other"
else
    fail "the run-label scan judged the planted fixtures wrongly (< want, > got):"
    diff <(sort <<<"$want_fixture_scan") <(printf '%s\n' "$got_fixture_scan") | grep '^[<>]' | sed 's/^/      /' || true
fi

# expect_red <label> <scan output> <pattern>: label_verdict must fail with a line
# matching the pattern.
expect_red() {
    local label="$1" verdict
    verdict="$(label_verdict "$2")"
    if grep -qE -- "$3" <<<"$verdict"; then pass "$label"; else fail "$label: verdict was: ${verdict:-clean}"; fi
}
expect_red "the floor fails a scan that finds fewer than $run_label_floor sites" "$fixture_scan" '^FLOOR only 10 '

# A real script with one label line removed, in a scratch copy of the tree.
tree="$scratch/tree"
mkdir -p "$tree/operator" "$tree/full-stack"
cp -R "$e2e_root/operator/scripts" "$tree/operator/"
cp -R "$e2e_root/full-stack/scripts" "$tree/full-stack/"
probe="$tree/operator/scripts/test-configpolicy.sh"
label_line="$(grep -nxF "    \${E2E_RUN_LABEL_YAML}" "$probe" | head -n1 | cut -d: -f1)"
sed -i "${label_line}d" "$probe"
probe_verdict="$(label_verdict "$(scan_run_labels "$tree/operator/scripts" "$tree/full-stack/scripts")")"
if [ "$(wc -l <<<"$probe_verdict")" -eq 1 ] && grep -q "^UNLABELLED $probe:" <<<"$probe_verdict"; then
    pass "removing one real label line makes the scan report that object"
else
    fail "removing line $label_line of test-configpolicy.sh: verdict was: ${probe_verdict:-clean}"
fi

mkdir -p "$scratch/empty" "$scratch/broken"
ln -s "$scratch/nowhere" "$scratch/broken/gone.sh"
expect_red "a directory with no files fails the scan" "$(scan_run_labels "$scratch/empty")" "^EMPTY $scratch/empty"
expect_red "a file the scan cannot read fails it" "$(scan_run_labels "$scratch/broken")" "^UNREADABLE $scratch/broken/gone.sh"
mkdir -p "$scratch/failawk"
printf '#!/bin/sh\nexit 2\n' > "$scratch/failawk/awk"
chmod +x "$scratch/failawk/awk"
expect_red "an awk that fails reading the scripts fails the scan" \
    "$(PATH="$scratch/failawk:$PATH" scan_run_labels "$fixtures/scripts")" '^UNREADABLE awk exited 2'

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all PR-install naming checks passed"
