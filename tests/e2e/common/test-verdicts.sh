#!/usr/bin/env bash
# Checks that no e2e case passes on an excuse: a branch that prints why the
# thing under test did not happen ("Note: ...", "not yet ...", "acceptable")
# and then calls pass_test has asserted nothing. Such a branch either asserts
# what the note excuses or fails.
#
# Usage: tests/e2e/common/test-verdicts.sh
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

excuse='echo .*(Note:|[Nn]ot yet|[Aa]ccepting|[Aa]cceptable|may still|which is fine|normal for)'

# Print file:line for each excuse echo followed by pass_test before the branch
# ends at the next if/elif/else/fi keyword.
scan_excuses() {
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    find "$@" -name '*.sh' ! -name test-verdicts.sh -type f -print0 | xargs -0 awk -v excuse="$excuse" '
        FNR == 1 { held = "" }
        $0 ~ excuse { held = FILENAME ":" FNR; next }
        /^[[:space:]]*(if|elif|else|fi)([[:space:]]|$)/ { held = ""; next }
        /^[[:space:]]*pass_test[[:space:]]/ && held != "" { print held; held = "" }
    '
}

strays="$(scan_excuses "$e2e_root")"
if [ -z "$strays" ]; then
    pass "no e2e case calls pass_test after printing an excuse"
else
    fail "these excuse lines are followed by pass_test in the same branch:"
    printf '%s\n' "$strays" | sed 's/^/    /'
fi

# The scan must see a planted excuse, and must not flag one whose branch ended.
mkdir -p "$scratch/probe"
cat > "$scratch/probe/p.sh" <<'PROBE'
if a; then
    pass_test "X-01"
else
    echo "  Note: not emitted yet"
    pass_test "X-01"
fi
if b; then
    echo "  Accepting: it may be fine"
else
    pass_test "X-02"
fi
PROBE
got="$(scan_excuses "$scratch/probe")"
if [ "$got" = "$scratch/probe/p.sh:4" ]; then
    pass "the scan flags an excuse followed by pass_test and nothing across a branch end"
else
    fail "the scan on the planted probe printed '$got', want '$scratch/probe/p.sh:4'"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
