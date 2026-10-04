#!/usr/bin/env bash
# Checks the prose of every e2e script: no em dash anywhere on a line, and no
# first-person pronoun in a comment or in a quoted string of a begin_test,
# pass_test, fail_test, skip_test, echo, printf or log line. The arguments of
# an assert_* call are exempt, because they are CLI output the case asserts
# verbatim. The fixtures under common/fixtures/wording/ prove each arm, and no
# cluster is needed.
#
# Usage: tests/e2e/common/test-wording.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
e2e_root="$(dirname "$here")"
fixtures="$here/fixtures/wording"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

pass() { echo "PASS  $1"; }
fail() {
    echo "FAIL  $1"
    failures=$((failures + 1))
}

# scan_wording <dir>: print `WORDING <file>:<line> <what>` for each breach in
# a *.sh under <dir> outside common/fixtures/, then `scanned <n>`. Exits 1 when
# find fails, when there is no file, or when a WORDING line was printed.
scan_wording() {
    local list
    list="$(find "$1" -name '*.sh' ! -path '*/common/fixtures/*' | LC_ALL=C sort)" || {
        echo "scan_wording: find failed under $1" >&2
        return 1
    }
    if [ -z "$list" ]; then
        echo "scan_wording: no .sh file under $1" >&2
        return 1
    fi
    # shellcheck disable=SC2016  # the single-quoted text is an awk program
    tr '\n' '\0' <<<"$list" | xargs -0 awk '
        FNR == 1 { files++ }
        function flag(what) { print "WORDING " FILENAME ":" FNR " " what; bad = 1 }
        function first_person(text) {
            return text ~ /(^|[^A-Za-z0-9_-])([Ww]e|I)([^A-Za-z0-9_\/]|$)/
        }
        {
            line = $0
            sub(/assert_[a-z_]+[[:space:]].*$/, "", line)
            # U+2014 spelled as its UTF-8 bytes, so this line holds none.
            if (index(line, "\342\200\224")) flag("prose em dash")
            if (line ~ /^#!/) next
            comment = ""
            if (match(line, /(^|[[:space:]])#/)) comment = substr(line, RSTART + RLENGTH)
            if (first_person(comment)) { flag("first-person word in a comment"); next }
            code = substr(line, 1, length(line) - length(comment))
            if (code !~ /(^|[;&|(])[[:space:]]*((if|then|else|do|!)[[:space:]]+)*(begin_test|pass_test|fail_test|skip_test|echo|printf|log)([[:space:]]|$)/) next
            while (match(code, /"([^"\\]|\\.)*"/)) {
                if (first_person(substr(code, RSTART + 1, RLENGTH - 2))) { flag("first-person word in a message"); next }
                code = substr(code, RSTART + RLENGTH)
            }
        }
        END { print "scanned " files + 0; exit bad }
    '
}

# probe <fixture> <want: pass|fail> <description> [line regex]: run the scan on
# one fixture; a failure also has to print a line matching the regex.
probe() {
    local out rc=0
    rm -rf "${scratch:?}/tree"
    mkdir -p "$scratch/tree"
    cp -R "$fixtures/$1/." "$scratch/tree/"
    out="$(scan_wording "$scratch/tree" 2>&1)" || rc=$?
    case "$2:$((rc != 0))" in
        pass:0) pass "$3" ;;
        fail:1)
            if [ -z "${4:-}" ] || grep -Eq -- "$4" <<<"$out"; then
                pass "$3"
            else
                fail "$3 (the scan failed without printing /$4/):"
                printf '%s\n' "$out" | sed 's/^/    /'
            fi
            ;;
        *)
            fail "$3 (scan exited $rc):"
            printf '%s\n' "$out" | sed 's/^/    /'
            ;;
    esac
}

# The fixtures sit under common/fixtures/, which the scan skips, so each one
# is copied to a scratch tree first.
probe dash-comment fail "an em dash in a comment fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe dash-title fail "an em dash in a begin_test title fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe dash-message fail "an em dash in a fail_test message fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe we-comment fail "\"we\" in a comment fails" '^WORDING .*/suite\.sh:2 first-person word in a comment$'
probe i-message fail "\"I\" in an echo message fails" '^WORDING .*/suite\.sh:2 first-person word in a message$'
probe clean pass "assert_* arguments, -I flags, I/O, words holding we or I and a non-message command pass"

real="$(scan_wording "$e2e_root" 2>&1)" && rc=0 || rc=$?
scanned="$(sed -n 's/^scanned //p' <<<"$real")"
if [ "$rc" -eq 0 ] && [ "${scanned:-0}" -ge 60 ]; then
    pass "no e2e script has a prose em dash or a first-person word ($scanned scripts)"
else
    fail "e2e prose (rc=$rc, ${scanned:-0} scripts scanned):"
    grep -v '^scanned ' <<<"$real" | sed 's/^/    /'
fi

if [ "$failures" -eq 0 ]; then
    echo "all checks passed"
else
    echo "$failures check(s) failed"
    exit 1
fi
