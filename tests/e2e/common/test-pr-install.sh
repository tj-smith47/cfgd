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

# render_awk defines render(s): one line of a heredoc body as bash expands it,
# so a YAML parser reads what the cluster would be sent. ${E2E_RUN_LABEL_YAML}
# becomes the run label with $label_sentinel as its value, and
# ${E2E_JOB_LABEL_YAML} the job label, as helpers.sh defines both. Every other
# $VAR, ${...}, $(...) and `...` becomes the plain word e2e-value, so each line
# stays one line and a parser's line numbers stay those of the source. A $(...)
# or ${...} still open at the end of the line takes the rest of it.
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
    if (name == "E2E_RUN_LABEL_YAML") return "cfgd.io/e2e-run: \"" sentinel "\""
    if (name == "E2E_JOB_LABEL_YAML") return "cfgd.io/e2e-job: \"e2e-value\""
    return "e2e-value"
}
function render(s,   out, i, c, j, n) {
    out = ""
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        n = substr(s, i + 1, 1)
        if (c == "\\" && n ~ /[$`\\]/) { out = out n; i++ }
        else if (c == "`") { j = index(substr(s, i + 1), "`"); i = j ? i + j : length(s); out = out "e2e-value" }
        else if (c != "$") out = out c
        else if (n == "(") { i = close_of(s, i + 1, "(", ")"); out = out "e2e-value" }
        else if (n == "{") { j = close_of(s, i + 1, "{", "}"); out = out expand(substr(s, i + 2, j - i - 2)); i = j }
        else if (match(substr(s, i + 1), /^[A-Za-z_][A-Za-z0-9_]*/)) { out = out expand(substr(s, i + 1, RLENGTH)); i += RLENGTH }
        else if (n ~ /[0-9*@#?$!-]/) { out = out "e2e-value"; i++ }
        else out = out c
    }
    return out
}'
# The run label's rendered value: random per run, so no label written by hand
# in a script can equal it.
label_sentinel="run-label-$$-$RANDOM$RANDOM"

# render [dash]: stdin, a heredoc body, rendered line by line; with dash 1 the
# leading tabs a <<- heredoc drops go first.
render() {
    awk -v dash="${1:-0}" -v sentinel="$label_sentinel" "$render_awk"'{ if (dash) sub(/^\t+/, ""); print render($0) }'
}

# object_query is the yq program that reads one rendered body. It prints one
# tab-separated line per cfgd.io object, a mapping whose apiVersion is a string
# starting cfgd.io/, at any depth of any document after aliases are resolved:
#   body  line of its apiVersion key  depth (0 at the document root)
#   kind  the document root's kind  metadata.name
#   whether metadata.labels has cfgd.io/e2e-run  whether that value is the sentinel
# kind and the names are strings or empty; tabs and newlines in them become a
# space. An ownerReferences entry is a reference to an object and is skipped.
# yq collects an empty match into one empty list per document, which the
# length check drops.
# shellcheck disable=SC2016 # a yq program; $doc belongs to yq
object_query='explode(.) as $doc | $doc | ..
| select(tag == "!!map" and (.apiVersion | tag) == "!!str" and (.apiVersion | test("^cfgd\.io/")))
| select(path | join("/") | test("(^|/)ownerReferences/[0-9]+$") | not)
| [filename, (.apiVersion | key | line), (path | length),
   ((.kind | select(tag == "!!str")) // ""),
   (($doc | select(tag == "!!map") | .kind | select(tag == "!!str")) // ""),
   ((.metadata | select(tag == "!!map") | .name | select(tag == "!!str")) // ""),
   ((.metadata | select(tag == "!!map") | .labels | select(tag == "!!map") | has("cfgd.io/e2e-run")) // false),
   ((.metadata | select(tag == "!!map") | .labels | select(tag == "!!map") | .["cfgd.io/e2e-run"]
     | (tag == "!!str" and . == strenv(SENTINEL))) // false)]
| select(length > 0) | map(tostring | sub("[\t\n]+"; " ")) | join("\t")'

# yq_error <dir> <body>: the first line of yq's last error, without the
# scratch file name it gives the body.
yq_error() {
    local err
    err="$(head -n1 "$1/yq.err")"
    printf '%s' "${err#"Error: bad file '$2': "}"
}

# parse_bodies <dir>: runs object_query over every body <dir>/index names,
# writing its lines to <dir>/objects and a `body<TAB>error` line for each body
# yq cannot read to <dir>/unparsed. One yq call reads every body; when it stops
# at a body it names, that body is set aside and the call goes on from the next,
# and an error that names no body has each remaining body read on its own.
parse_bodies() {
    local dir="$1" bodies out bad i b
    : > "$dir/objects"
    : > "$dir/unparsed"
    mapfile -t bodies < <(cut -f1 "$dir/index")
    while [ "${#bodies[@]}" -gt 0 ]; do
        if out="$(cd "$dir" && SENTINEL="$label_sentinel" yq -N "$object_query" "${bodies[@]}" 2>"$dir/yq.err")"; then
            [ -z "$out" ] || printf '%s\n' "$out" >> "$dir/objects"
            return 0
        fi
        bad="$(sed -n "1s/^Error: bad file '\([^']*\)'.*/\1/p" "$dir/yq.err")"
        for i in "${!bodies[@]}"; do [ "${bodies[$i]}" != "$bad" ] || break; done
        if [ -z "$bad" ] || [ "${bodies[$i]}" != "$bad" ]; then
            for b in "${bodies[@]}"; do
                if out="$(cd "$dir" && SENTINEL="$label_sentinel" yq -N "$object_query" "$b" 2>"$dir/yq.err")"; then
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

# scan_run_labels <kinds> <dir or file...> reads every file in each dir, and
# each file named, through heredocs.awk. Each heredoc body is rendered the way
# bash would expand it and read with yq, and every fact about a cfgd.io object
# (where it sits, its kind, name and labels, the line of its apiVersion) comes
# from that parse. It prints one line per finding, tag first:
#   SITE         a heredoc fed to kubectl apply/create/replace or apply_yaml that
#                holds an operator object
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
#   UNPARSED     a heredoc fed to the cluster or captured that yq cannot read
#                as YAML once rendered
#   OUTSIDE      a cfgd.io apiVersion on a shell line outside any heredoc
#   BYPATH       kubectl apply/create/replace given a manifest by path (-f other
#                than -, or -k), which the scan cannot read
#   UNTERMINATED, UNREADABLE, EMPTY   the scan could not read what it was given
# A heredoc written to a file is read only when it mentions cfgd.io/, and one
# yq cannot read (a script, say) is not YAML and passes quietly.
scan_run_labels() {
    local kinds="$1" dir f files=() found work
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
    work="$(mktemp -d "$scratch/scan.XXXXXX")" || { echo "UNREADABLE no scratch directory for the scan"; return 0; }
    : > "$work/index"
    { awk -f "$here/heredocs.awk" "${files[@]}" || echo "UNREADABLE heredocs.awk exited $? reading ${files[*]}"; } |
        awk -F '\t' -v work="$work" -v sentinel="$label_sentinel" "$render_awk"'
        function rest(n,   i, p) { p = 0; for (i = 1; i <= n; i++) p += length($i) + 1; return substr($0, p + 1) }
        # by_path(c): c applies a manifest by path when, within the kubectl
        # command, -f names something other than - or -k names a directory.
        # A quoted path is dropped from c, so -f with no word after it is one.
        function by_path(c,   n, t, i, v) {
            while (match(c, /kubectl([ \t][^|;&)]*)?[ \t](apply|create|replace)([ \t][^|;&)]*)?/)) {
                n = split(substr(c, RSTART, RLENGTH), t, /[ \t]+/)
                c = substr(c, RSTART + RLENGTH)
                for (i = 1; i <= n; i++) {
                    if (t[i] ~ /^(-k|--kustomize)/) return 1
                    if (t[i] == "-f" || t[i] == "--filename") v = (i < n) ? t[i + 1] : ""
                    else if (t[i] ~ /^--filename=/) v = substr(t[i], 12)
                    else if (t[i] ~ /^-f./) { v = substr(t[i], 3); sub(/^=/, "", v) }
                    else continue
                    if (v != "-") return 1
                }
            }
            return 0
        }
        function close_heredoc(id,   i, n, yaml, path) {
            n = count[id]
            yaml = (cls[id] != "FILE")
            for (i = 1; i <= n && !yaml; i++) yaml = index(text[id, i], "cfgd.io/") > 0
            if (n == 0 || !yaml) return
            path = work "/" (++bodies) ".yaml"
            for (i = 1; i <= n; i++) print render(text[id, i]) > path
            close(path)
            print bodies ".yaml", file, opened[id], cls[id], qtd[id], at[id, 1] > (work "/index")
        }
        BEGIN { OFS = "\t" }
        /^UNREADABLE / { print; next }
        { file = $2 }
        $1 == "UNCLOSED" { print "UNTERMINATED " file ":" $3 ": no " $4 " line closes this heredoc"; next }
        $1 == "OPEN" {
            id = $4; opened[id] = $3; qtd[id] = $6; dash[id] = $7; count[id] = 0
            c = rest(7)
            if (c ~ /kubectl([ \t].*)?[ \t](apply|create|replace)([ \t]|$)/ || c ~ /(^|[^A-Za-z0-9_])apply_yaml([ \t]|$)/) cls[id] = "CLUSTER"
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
            text[id, ++count[id]] = line; at[id, count[id]] = $3
            next
        }
        $1 == "CLOSE" { close_heredoc($4); next }
        $1 == "CMD" {
            if (by_path(rest(3))) print "BYPATH " file ":" $3 ": the scan cannot read a manifest applied by path; apply it from a heredoc"
            next
        }
        $1 == "SH" {
            line = rest(3)
            if (line !~ /^[ \t]*#/) {
                sub(/[ \t]+#.*$/, "", line)
                if (line ~ /(^|[^A-Za-z0-9_])["\047]?apiVersion["\047]?[ \t]*:[ \t]*["\047]?cfgd\.io\//) print "OUTSIDE " file ":" $3 ": a cfgd.io apiVersion outside any heredoc"
            }
        }
        END { close(work "/index") }
    ' || echo "UNREADABLE the scan's heredoc collector exited $?"
    parse_bodies "$work"
    awk -F '\t' -v kinds="$(tr '\n' ' ' <<<"$kinds")" '
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
            b = $1; at = src[b] ":" (first[b] + $2 - 1); kind = $4; root = $5; name = $6
            sub(/ +$/, "", kind); sub(/ +$/, "", root)
            cfgd[b] = 1
            if (cls[b] == "FILE") next
            if ($3 > 0) {
                print "NESTED " at ": a cfgd.io object nested inside " (root == "" ? "another document" : root) "; apply it as its own document"
                reported[b] = 1
                next
            }
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
            else if ($7 == "true" && $8 != "true") print "HANDSPELLED " at " " kind " " name ": spell the label as ${E2E_RUN_LABEL_YAML}"
            else if ($7 != "true") print "UNLABELLED " at " " kind " " name ": metadata.labels has no ${E2E_RUN_LABEL_YAML}"
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
helpers="$e2e_root/common/helpers.sh"
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
    local want="$1" name="$2" opener="$3" body="$4" closer="${5-EOF}" f checked dash=0
    f="$fixtures/scripts/$name.sh"
    printf '%s\n%s\n%s\n' "$opener" "$body" "$closer" > "$f"
    checked="$(bash -n "$f" 2>&1 | grep -v 'delimited by end-of-file' || true)"
    [ -z "$checked" ] || fail "fixture $name is not valid shell: $checked"
    [[ "$opener" != *'<<-'* ]] || dash=1
    if checked="$(printf '%s\n' "$body" | render "$dash" | yq '.' 2>&1 >/dev/null)"; then
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
kubectl apply -f- < /dev/null
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

want_fixture_scan="SITE substitutions.sh:1
SITE alias-api.sh:1
UNLABELLED alias-api.sh:6
SITE anchor-kind.sh:1
UNLABELLED anchor-kind.sh:2
SITE apply-yaml.sh:1
UNLABELLED apply-yaml.sh:2
BYPATH by-path.sh:1
BYPATH by-path.sh:2
BYPATH by-path.sh:3
BYPATH by-path.sh:4
BYPATH by-path.sh:5
BYPATH by-path.sh:6
BYPATH by-path.sh:7
BYPATH by-path.sh:8
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

by_path_tree="$scratch/by-path"
copy_tree "$by_path_tree"
printf 'kubectl apply -f mc.yaml\n' > "$by_path_tree/operator/scripts/zz-by-path.sh"
by_path_verdict="$(tree_verdict_of "$by_path_tree")"
if [ "$by_path_verdict" = "BYPATH $by_path_tree/operator/scripts/zz-by-path.sh:1: the scan cannot read a manifest applied by path; apply it from a heredoc" ]; then
    pass "a manifest applied by path in a floored suite fails the scan, and the crossplane suite's do not"
else
    fail "a manifest applied by path in the operator suite: verdict was: ${by_path_verdict:-clean}"
fi
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
# A yq that fails without naming a body has each body read on its own, so its
# error reaches every heredoc the cluster would be sent.
mkdir -p "$scratch/fail-yq"
printf '#!/bin/sh\necho "Error: no yq here" >&2\nexit 1\n' > "$scratch/fail-yq/yq"
chmod +x "$scratch/fail-yq/yq"
expect_red "yq failing on every body fails the scan" \
    "$(PATH="$scratch/fail-yq:$PATH" scan_run_labels "$kinds" "$fixtures/scripts/labelled.sh")" "^UNPARSED $fixtures/scripts/labelled.sh:1: .*Error: no yq here"

printf 'a: 1\n' > "$scratch/no-kinds.yaml"
expect_red "a CRD file that names no kinds fails the kind list" "$(operator_kinds "$scratch/no-kinds.yaml")" "^FAIL $scratch/no-kinds.yaml names no CRD kinds"
expect_red "a CRD file yq cannot read fails the kind list" "$(operator_kinds "$scratch/no-such.yaml")" "^FAIL yq could not read $scratch/no-such.yaml"

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
        $1 == "UNREADABLE" { print; next }
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

if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all PR-install naming checks passed"
