#!/usr/bin/env bash
# Checks the counter readers in helpers.sh without a cluster: both spellings a
# counter sample renders under are read, a longer name or a comment line is
# not, an absent sample reads as 0, and no e2e script matches a counter sample
# by hand.
#
# Usage: tests/e2e/common/test-metrics.sh
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

# shellcheck source=tests/e2e/common/helpers.sh
REGISTRY=registry.test CLI_SCRATCH="$scratch" source "$here/helpers.sh"

body="$scratch/body.txt"
cat > "$body" <<'BODY'
# HELP cfgd_x_hits Hits.
# TYPE cfgd_x_hits counter
cfgd_x_hits_total{module="a"} 3
cfgd_x_hits_total_total{module="b"} 5
cfgd_x_hits_totalx{module="c"} 7
cfgd_x_hits_total 11
cfgd_x_hits_created{module="a"} 1.7e9
# EOF
BODY

want="$(printf '%s\n' 'cfgd_x_hits_total{module="a"} 3' 'cfgd_x_hits_total_total{module="b"} 5' 'cfgd_x_hits_total 11')"
got="$(metric_sample_lines cfgd_x_hits "$body")"
if [ "$got" = "$want" ]; then
    pass "metric_sample_lines reads both spellings and skips _totalx, _created and comment lines"
else
    fail "metric_sample_lines: got '$got', want '$want'"
fi

if metric_sample_lines cfgd_x_misses "$body" > /dev/null; then
    fail "metric_sample_lines returned 0 for a family with no sample"
else
    pass "metric_sample_lines returns 1 for a family with no sample"
fi

check_value() {
    local labels="$1" want="$2" got
    got="$(metric_sample_value cfgd_x_hits "$labels" "$body")"
    if [ "$got" = "$want" ]; then
        pass "metric_sample_value {$labels} = $want"
    else
        fail "metric_sample_value {$labels}: got '$got', want '$want'"
    fi
}
check_value 'module="a"' 3
check_value 'module="b"' 5
check_value 'module="c"' 0
check_value 'module="absent"' 0
check_value '' 11

# There are far more scripts than this under tests/e2e/ (85 when the floor was
# set), so a count below it means the scan lost its files.
min_scanned_files=40

# Print file:line for each line outside helpers.sh and this file that names a
# counter sample (`cfgd_<name>_total` as a whole word, or any `_total{` / `_total(\{` match),
# skipping comments and begin_test titles, then a last line `scanned <files>`.
# Exits 1 when no file matched.
scan_hand_matches() {
    local files="$scratch/scan-files"
    find "$@" -name '*.sh' ! -name helpers.sh ! -name test-metrics.sh -type f > "$files"
    if [ ! -s "$files" ]; then
        echo "scan_hand_matches: no .sh file under $*" >&2
        return 1
    fi
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    tr '\n' '\0' < "$files" | xargs -0 awk '
        FNR == 1 { files++ }
        /^[[:space:]]*#/ || /^[[:space:]]*begin_test[[:space:]]/ { next }
        /cfgd_[a-z_]+_total([^a-z_]|$)/ || /_total(\(\\\{|\\?\{)/ { print FILENAME ":" FNR }
        END { print "scanned " files + 0 }
    '
}

# Every counter read goes through the helpers, so a release that renders a
# different spelling is handled in one place.
report="$(scan_hand_matches "$e2e_root")"
strays="$(grep -v '^scanned ' <<<"$report" || true)"
scanned_files="$(sed -n 's/^scanned //p' <<<"$report")"
if [ "$scanned_files" -lt "$min_scanned_files" ]; then
    fail "the hand-match scan read $scanned_files files, fewer than $min_scanned_files"
elif [ -z "$strays" ]; then
    pass "no e2e script matches a counter sample by hand ($scanned_files files)"
else
    fail "counter samples matched outside metric_sample_lines/metric_sample_value:"
    printf '%s\n' "$strays" | sed 's/^/    /'
fi

if scan_hand_matches "$scratch/no-such-dir" > /dev/null 2>&1; then
    fail "a hand-match scan of a path with no .sh file passed"
else
    pass "a hand-match scan of a path with no .sh file fails"
fi

# One probe line per placement; the flagged ones are listed with their line.
probe="$scratch/probe"
mkdir -p "$probe"
cat > "$probe/p.sh" <<'PROBE'
grep -qE '^cfgd_x_hits_total(\{| )' body
awk -v s='cfgd_x_hits_total{module="a"}' '$1 == s' body
grep -q cfgd_x_hits_total body
grep -q '^cfgd_x_hits_total ' body
grep -q "^${family}_total{" body
begin_test "X-01: /metrics returns cfgd_x_hits_total"
# cfgd_x_hits_total is read through the helper below
metric_sample_lines cfgd_x_hits body
curl -H "Authorization: Bearer cfgd_dk_totally_invalid_key"
PROBE
want="$(printf '%s\n' "$probe/p.sh:1" "$probe/p.sh:2" "$probe/p.sh:3" "$probe/p.sh:4" "$probe/p.sh:5" "scanned 1")"
got="$(scan_hand_matches "$probe")"
if [ "$got" = "$want" ]; then
    pass "the hand-match scan flags anchored, awk, unanchored, space and templated matches, and skips titles, comments, helper calls and a longer word such as totally"
else
    fail "the hand-match scan on the probe printed:"
    printf '%s\n' "$got" | sed 's/^/    /'
    echo "    want:"
    printf '%s\n' "$want" | sed 's/^/    /'
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
