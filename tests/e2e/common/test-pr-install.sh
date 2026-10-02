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

# Every operator object a suite applies must carry the run label in its own
# metadata.labels: the PR operator reconciles only objects with that label, so an
# unlabelled object makes its case fail for a reason unrelated to the case. The
# operator's kinds come from the CRDs it serves, so a new CRD joins the rule.
repo_root="$(dirname "$(dirname "$e2e_root")")"

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

# scan_run_labels <kinds> <dir...> reads every file in each dir through
# heredocs.awk and prints one line per finding, tag first:
#   SITE         a heredoc fed to kubectl apply/create/replace or apply_yaml that
#                holds an operator object
#   FILEDOC      a heredoc written to a file (cat >, or inside a pod) that holds a
#                cfgd.io document; such a file is never a cluster object
#   OTHERKIND    a cfgd.io document of a kind the operator does not serve, applied
#                to the cluster
#   UNLABELLED   an operator object whose metadata.labels lacks ${E2E_RUN_LABEL_YAML}
#   HANDSPELLED  an operator object whose label is spelled by hand
#   FLOWMETA     an operator object whose metadata is in flow form
#   NESTED       a cfgd.io object nested in another document, such as a List
#   QUOTED       an operator object in a quoted-delimiter heredoc, where
#                ${E2E_RUN_LABEL_YAML} cannot expand
#   OUTSIDE      a cfgd.io apiVersion outside any heredoc
#   UNTERMINATED, UNREADABLE, EMPTY   the scan could not read what it was given
scan_run_labels() {
    local kinds="$1" dir f files=() found
    shift
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
    { awk -f "$here/heredocs.awk" "${files[@]}" || echo "UNREADABLE heredocs.awk exited $? reading ${files[*]}"; } |
        awk -F '\t' -v kinds="$(tr '\n' ' ' <<<"$kinds")" '
        function rest(n,   i, p) { p = 0; for (i = 1; i <= n; i++) p += length($i) + 1; return substr($0, p + 1) }
        function indent(s) { match(s, /^[ \t]*/); return RLENGTH }
        function strip(s) { sub(/^[ \t]+/, "", s); sub(/[ \t]+#.*$/, "", s); sub(/[ \t]+$/, "", s); return s }
        function blank_or_comment(s) { s = strip(s); return s == "" || substr(s, 1, 1) == "#" }
        function cfgd_api(s) { return s ~ /^(- )?apiVersion:[ \t]*["\047]?cfgd\.io\// }
        function hand(s) { return s ~ /^["\047]?cfgd\.io\/e2e-run["\047]?:/ }
        function label_entry(s) {
            if (s == "${E2E_RUN_LABEL_YAML}") labelled = 1
            else if (hand(s)) handspelled = 1
        }
        function flow_labels(s,   n, parts, i) {
            sub(/^labels:[ \t]*\{/, "", s); sub(/\}[ \t]*$/, "", s)
            n = split(s, parts, ",")
            for (i = 1; i <= n; i++) { sub(/^[ \t]+/, "", parts[i]); sub(/[ \t]+$/, "", parts[i]); label_entry(parts[i]) }
        }
        function check_doc(first, last,    i, base, s, api, nested, kind, name, inmeta, flowmeta, mchild, inlab, lind) {
            base = -1; labelled = 0; handspelled = 0
            for (i = first; i <= last; i++) {
                if (blank_or_comment(body[i])) continue
                if (base < 0) base = indent(body[i])
                s = strip(body[i])
                if (indent(body[i]) != base) { if (cfgd_api(s)) nested = bline[i]; continue }
                if (cfgd_api(s)) api = bline[i]
                if (s ~ /^kind:/) { kind = s; sub(/^kind:[ \t]*/, "", kind); gsub(/["\047]/, "", kind) }
                if (s ~ /^metadata:[ \t]*\{/) flowmeta = 1
            }
            if (!api && !nested) return
            has_cfgd = 1
            if (class == "FILE") return
            if (!api) {
                print "NESTED " file ":" nested ": a cfgd.io object inside a List; apply it as its own document"
                return
            }
            if (!((" " kind " ") in operator)) { print "OTHERKIND " file ":" api " " kind; return }
            site = 1
            if (quoted) {
                print "QUOTED " file ":" api " " kind ": the heredoc delimiter is quoted, so ${E2E_RUN_LABEL_YAML} cannot expand"
                return
            }
            if (flowmeta) {
                print "FLOWMETA " file ":" api " " kind ": write metadata in block form; the scan reads labels from block-form metadata"
                return
            }
            for (i = first; i <= last; i++) {
                if (blank_or_comment(body[i])) continue
                s = strip(body[i])
                if (indent(body[i]) <= base) {
                    inmeta = (indent(body[i]) == base && s ~ /^metadata:$/)
                    mchild = -1; inlab = 0
                    continue
                }
                if (!inmeta) continue
                if (mchild < 0) mchild = indent(body[i])
                if (indent(body[i]) == mchild) {
                    if (s ~ /^name:/ && name == "") { name = s; sub(/^name:[ \t]*/, "", name) }
                    inlab = (s ~ /^labels:$/)
                    lind = indent(body[i])
                    if (s ~ /^labels:[ \t]*\{/) flow_labels(s)
                    continue
                }
                if (inlab && indent(body[i]) > lind) label_entry(s)
            }
            if (handspelled) {
                print "HANDSPELLED " file ":" api " " kind " " name ": spell the label as ${E2E_RUN_LABEL_YAML}"
            } else if (!labelled) {
                print "UNLABELLED " file ":" api " " kind " " name ": metadata.labels has no ${E2E_RUN_LABEL_YAML}"
            }
        }
        function close_heredoc(id,   i, first, n) {
            n = count[id]
            for (i = 1; i <= n; i++) { body[i] = text[id, i]; bline[i] = at[id, i] }
            class = cls[id]; quoted = qtd[id]; has_cfgd = 0; site = 0
            first = 1
            for (i = 1; i <= n + 1; i++) {
                if (i == n + 1 || body[i] ~ /^---([ \t]|$)/) { check_doc(first, i - 1); first = i + 1 }
            }
            if (site) print "SITE " file ":" opened[id]
            if (has_cfgd && class == "FILE") print "FILEDOC " file ":" opened[id]
        }
        BEGIN { n = split(kinds, k, " "); for (i = 1; i <= n; i++) operator[" " k[i] " "] = 1 }
        /^UNREADABLE / { print; next }
        { file = $2 }
        $1 == "UNCLOSED" { print "UNTERMINATED " file ":" $3 ": no " $4 " line closes this heredoc"; next }
        $1 == "OPEN" {
            id = $4; opened[id] = $3; qtd[id] = $6; dash[id] = $7; count[id] = 0
            c = rest(7)
            cls[id] = (c ~ /kubectl([ \t].*)?[ \t](apply|create|replace)([ \t]|$)/ || c ~ /(^|[^A-Za-z0-9_])apply_yaml([ \t]|$)/) ? "CLUSTER" : "FILE"
            next
        }
        $1 == "BODY" {
            id = $4; line = rest(4)
            if (dash[id]) sub(/^\t+/, "", line)
            text[id, ++count[id]] = line; at[id, count[id]] = $3
            next
        }
        $1 == "CLOSE" { close_heredoc($4); next }
        $1 == "SH" {
            line = rest(3)
            if (line !~ /^[ \t]*#/) {
                sub(/[ \t]+#.*$/, "", line)
                if (line ~ /apiVersion"?:[ \t]*["\047]?cfgd\.io\//) print "OUTSIDE " file ":" $3 ": a cfgd.io apiVersion outside any heredoc"
            }
        }
    ' || echo "UNREADABLE the scan's awk exited $?"
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

# label_verdict <root> <scan output>: prints nothing when the scan is clean and
# each floored suite holds its floor, otherwise one line per problem.
label_verdict() {
    local root="$1" out="$2" i suite floor sites
    grep -Ev '^(SITE|FILEDOC|OTHERKIND) ' <<<"$out" || true
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
tree_scan="$(scan_run_labels "$kinds" "${label_scan_dirs[@]}")"
tree_verdict="$(label_verdict "$e2e_root" "$tree_scan")"
if [ -z "$tree_verdict" ]; then
    pass "every operator object the e2e suites apply carries the run label ($(grep -c '^SITE ' <<<"$tree_scan") heredocs: $(for s in "${run_label_suites[@]}"; do printf '%s %s, ' "$s" "$(grep -c "^SITE $e2e_root/$s/scripts/" <<<"$tree_scan")"; done | sed 's/, $//'))"
else
    fail "operator objects the PR operator would ignore:"
    printf '      %s\n' "${tree_verdict//$'\n'/$'\n'      }"
fi
others="$(grep '^OTHERKIND ' <<<"$tree_scan" | cut -d' ' -f2- | sed "s|$e2e_root/||" | paste -sd ';' - | sed 's/;/; /g' || true)"
[ -z "$others" ] || pass "cfgd.io objects outside the operator's watch, so no run label: $others"
pass "$(grep -c '^FILEDOC ' <<<"$tree_scan" || true) heredocs write a cfgd.io document to a file, which needs no run label"

# The scan against planted fixtures: one per way a suite writes a cfgd.io
# document, one per placement of the label and one per spelling the rule
# refuses. Each fixture is checked as shell (bash -n) and its body as YAML,
# rendered the way bash would expand it, before the scan reads it.
fixtures="$scratch/label-fixtures"
mkdir -p "$fixtures/scripts"
render() {
    local s="$1"
    s="${s//\$\{E2E_RUN_LABEL_YAML_OLD\}/cfgd.io/e2e-run-old: \"41\"}"
    s="${s//\$\{E2E_RUN_LABEL_YAML\}/cfgd.io/e2e-run: \"42\"}"
    s="${s//\$\{E2E_RUN_ID\}/42}"
    printf '%s\n' "$s" | sed 's/^\t*//'
}
# plant <name> <opener> <body> [closer]
plant() {
    local name="$1" opener="$2" body="$3" closer="${4-EOF}" f checked
    f="$fixtures/scripts/$name.sh"
    printf '%s\n%s\n%s\n' "$opener" "$body" "$closer" > "$f"
    checked="$(bash -n "$f" 2>&1 | grep -v 'delimited by end-of-file' || true)"
    [ -z "$checked" ] || fail "fixture $name is not valid shell: $checked"
    checked="$(render "$body" | yq '.' 2>&1 >/dev/null)" || fail "fixture $name is not valid YAML: $checked"
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
plant quoted "kubectl apply -f - <<'EOF'" "$module_labelled"
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
plant no-sink "yaml=\$(cat <<EOF" "$module_labelled" "EOF"$'\n'")"$'\n'"echo \"\$yaml\" | kubectl apply -f -"
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
plant heredoc-unterminated "$apply" "$module_labelled" ''
outside_yaml='apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: outside\n'
printf '%s\n' "printf '$outside_yaml' | kubectl apply -f -" > "$fixtures/scripts/outside.sh"
bash -n "$fixtures/scripts/outside.sh" || fail "fixture outside is not valid shell"
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
SITE dash.sh:1
UNLABELLED dash.sh:2
SITE exec-apply.sh:1
UNLABELLED exec-apply.sh:2
SITE entry-comment.sh:1
UNLABELLED entry-comment.sh:2
FILEDOC file-operator-kind.sh:1
SITE flow-labels.sh:1
SITE flow-metadata.sh:1
FLOWMETA flow-metadata.sh:2
SITE hand-spelled.sh:1
HANDSPELLED hand-spelled.sh:2
UNTERMINATED heredoc-unterminated.sh:1
FILEDOC in-pod.sh:1
SITE key-prefix.sh:1
UNLABELLED key-prefix.sh:2
SITE label-absent.sh:1
UNLABELLED label-absent.sh:2
SITE labelled.sh:1
SITE labels-comment.sh:1
UNLABELLED labels-comment.sh:2
NESTED list.sh:5
SITE multi-doc.sh:1
UNLABELLED multi-doc.sh:12
SITE nested-metadata.sh:1
UNLABELLED nested-metadata.sh:2
SITE no-labels.sh:1
UNLABELLED no-labels.sh:2
FILEDOC no-sink.sh:1
OTHERKIND other-kind.sh:2
OUTSIDE outside.sh:1
SITE quoted.sh:1
QUOTED quoted.sh:2
SITE rc-ok-comment.sh:1
UNLABELLED rc-ok-comment.sh:2
SITE var-prefix.sh:1
UNLABELLED var-prefix.sh:2"
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

printf 'a: 1\n' > "$scratch/no-kinds.yaml"
expect_red "a CRD file that names no kinds fails the kind list" "$(operator_kinds "$scratch/no-kinds.yaml")" "^FAIL $scratch/no-kinds.yaml names no CRD kinds"
expect_red "a CRD file yq cannot read fails the kind list" "$(operator_kinds "$scratch/no-such.yaml")" "^FAIL yq could not read $scratch/no-such.yaml"

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all PR-install naming checks passed"
