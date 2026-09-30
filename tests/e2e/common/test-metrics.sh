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

# Every counter read goes through the helpers, so a release that renders a
# different spelling is handled in one place.
strays="$(grep -rnE --include='*.sh' '_total(\(\\\{|\\?\{)' "$e2e_root" \
    | grep -vE "^$e2e_root/common/(helpers|test-metrics)\.sh:" || true)"
if [ -z "$strays" ]; then
    pass "no e2e script matches a counter sample by hand"
else
    fail "counter samples matched outside metric_sample_lines/metric_sample_value:"
    printf '%s\n' "$strays" | sed 's/^/    /'
fi

# The scan must be able to see a hand match, or the check above proves nothing.
mkdir -p "$scratch/probe"
printf '%s\n' "grep -qE '^cfgd_x_hits_total(\\{| )' body" "awk -v s='cfgd_x_hits_total{module=\"a\"}'" > "$scratch/probe/p.sh"
if [ "$(grep -rcE --include='*.sh' '_total(\(\\\{|\\?\{)' "$scratch/probe" | cut -d: -f2)" = 2 ]; then
    pass "the hand-match scan finds both a grep and an awk match"
else
    fail "the hand-match scan missed a planted grep or awk match"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
