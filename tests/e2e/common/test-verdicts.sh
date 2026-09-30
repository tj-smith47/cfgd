#!/usr/bin/env bash
# Checks that no e2e case passes on an excuse: a branch that prints why the
# thing under test did not happen ("Note: ...", "not yet ...", "acceptable")
# and then calls pass_test has asserted nothing. Such a branch either asserts
# what the note excuses or fails. A pass_test line carrying
# `# verdict-ok: <why>` is exempt.
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

excuse='(echo|printf) .*(Note:|[Nn]ot yet|[Aa]ccepting|[Aa]cceptable|may still|which is fine|normal for)'

# There are about twice this many pass_test calls under tests/e2e/, so a
# count below it means the scan lost its files.
min_pass_tests=330

# Print file:line for each excuse echo or printf followed by pass_test before
# its block ends (if/elif/else/fi, a case arm or esac, done, or a closing
# brace), then a last line `scanned <files> <pass_test calls>`. Heredoc bodies
# are skipped, since a `}` in a manifest ends no shell block; a terminator may
# be followed by the quote that closes a `bash -c '...'` holding the heredoc.
# A heredoc still open at the end of a file is reported, so a misread
# terminator cannot hide the rest of the file. Exits 1 when no file matched.
scan_excuses() {
    local files="$scratch/scan-files"
    find "$@" -name '*.sh' ! -name test-verdicts.sh -type f > "$files"
    if [ ! -s "$files" ]; then
        echo "scan_excuses: no .sh file under $*" >&2
        return 1
    fi
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    tr '\n' '\0' < "$files" | xargs -0 awk -v excuse="$excuse" '
        FNR == 1 {
            if (doc != "") print prev ": heredoc " doc " never closes"
            held = ""; doc = ""; prev = FILENAME; files++
        }
        /(^|[;&|[:space:]])pass_test[[:space:]]/ { calls++ }
        /(^|[;&|[:space:]])pass_test[[:space:]].*# verdict-ok: [^[:space:]]/ { held = ""; next }
        doc != "" { if ($0 ~ "^[[:space:]]*" doc "[\047\")]*$") doc = ""; next }
        $0 !~ /^[[:space:]]*#/ && match($0, /(^|[^<])<<-?[[:space:]]*[\047"]?[A-Za-z_]+/) {
            doc = substr($0, RSTART, RLENGTH)
            gsub(/^[^<]?<<-?[[:space:]]*[\047"]?/, "", doc)
        }
        $0 ~ excuse {
            if ($0 ~ /(^|[;&|[:space:]])pass_test[[:space:]]/) { print FILENAME ":" FNR; held = "" }
            else { held = FILENAME ":" FNR }
            next
        }
        /^[[:space:]]*(if|elif|else|fi|esac|done)([[:space:];]|$)/ || /^[[:space:]]*\}/ || /;;[[:space:]]*$/ { held = ""; next }
        /(^|[;&|[:space:]])pass_test[[:space:]]/ && held != "" { print held; held = "" }
        END {
            if (doc != "") print prev ": heredoc " doc " never closes"
            print "scanned " files + 0 " " calls + 0
        }
    '
}

report="$(scan_excuses "$e2e_root")"
strays="$(grep -v '^scanned ' <<<"$report" || true)"
read -r _ scanned_files scanned_calls < <(grep '^scanned ' <<<"$report")
if [ "$scanned_calls" -lt "$min_pass_tests" ]; then
    fail "the scan read $scanned_calls pass_test calls in $scanned_files files, fewer than $min_pass_tests"
elif [ -z "$strays" ]; then
    pass "no e2e case calls pass_test after printing an excuse ($scanned_calls calls in $scanned_files files)"
else
    fail "the scan flagged these lines (an excuse followed by pass_test, or a heredoc that never closes):"
    printf '%s\n' "$strays" | sed 's/^/    /'
fi

if scan_excuses "$scratch/no-such-dir" > /dev/null 2>&1; then
    fail "a scan of a path with no .sh file passed"
else
    pass "a scan of a path with no .sh file fails"
fi

# One probe file per placement. Each flagged excuse is listed with its line;
# every other excuse in the probes must not be flagged.
probe="$scratch/probe"
mkdir -p "$probe"
cat > "$probe/else.sh" <<'PROBE'
if a; then
    pass_test "X-01"
else
    echo "  Note: not emitted yet"
    pass_test "X-01"
fi
PROBE
cat > "$probe/elif.sh" <<'PROBE'
if a; then
    fail_test "X-02" "a"
elif b; then
    echo "  Accepting: close enough"
    pass_test "X-02"
fi
PROBE
cat > "$probe/function.sh" <<'PROBE'
check() {
    echo "  Note: skipped the check"
    pass_test "X-03"
}
PROBE
cat > "$probe/comment.sh" <<'PROBE'
if a; then
    echo "  this is acceptable"
    # the pass below asserts nothing
    pass_test "X-04"
fi
PROBE
cat > "$probe/same-line.sh" <<'PROBE'
echo "  Note: x"; pass_test "X-05"
PROBE
cat > "$probe/branch-ended.sh" <<'PROBE'
if b; then
    echo "  Accepting: it may be fine"
else
    pass_test "X-06"
fi
PROBE
cat > "$probe/case.sh" <<'PROBE'
case "$x" in
    a)
        echo "  Note: a"
        ;;
esac
pass_test "X-07"
case "$y" in
    b) echo "  Note: b" ;;
esac
pass_test "X-08"
PROBE
cat > "$probe/loop.sh" <<'PROBE'
while read -r l; do
    echo "  not yet: $l"
done < f
pass_test "X-09"
PROBE
cat > "$probe/hatch.sh" <<'PROBE'
if a; then
    echo "  Note: the reply is the assertion"
    pass_test "X-10" # verdict-ok: the echoed reply was compared above
fi
PROBE
cat > "$probe/printf.sh" <<'PROBE'
if a; then
    printf '  Note: not emitted yet\n'
    pass_test "X-11"
fi
PROBE
cat > "$probe/heredoc-brace.sh" <<'PROBE'
if a; then
    echo "  Note: applying anyway"
    kubectl apply -f - <<EOF
{
  "kind": "ConfigMap"
}
EOF
    pass_test "X-12"
fi
PROBE
cat > "$probe/herestring.sh" <<'PROBE'
grep -q x <<<abc
if a; then
    echo "  Note: after a here-string"
    pass_test "X-18"
fi
PROBE
cat > "$probe/quoted-heredoc.sh" <<'PROBE'
kubectl exec p -- bash -c 'cat > f << "INNEREOF"
x: 1
INNEREOF'
if a; then
    echo "  Note: after a quoted heredoc"
    pass_test "X-19"
fi
PROBE
cat > "$probe/unclosed.sh" <<'PROBE'
# Usage: f <<'EOF' ... EOF
cat <<DOC
never closed
PROBE
cat > "$probe/while-body.sh" <<'PROBE'
while read -r l; do
    echo "  not yet: $l"
    pass_test "X-13"
done < f
PROBE
cat > "$probe/stderr.sh" <<'PROBE'
if a; then
    echo >&2 "  Note: on stderr"
    pass_test "X-14"
fi
PROBE
cat > "$probe/inner-loop.sh" <<'PROBE'
if a; then
    echo "  Note: looping"
    for i in 1 2; do :; done
    pass_test "X-15"
fi
PROBE
cat > "$probe/hatch-elsewhere.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    # verdict-ok: a hatch on a comment line exempts nothing
    pass_test "X-16"
fi
PROBE
cat > "$probe/hatch-on-excuse.sh" <<'PROBE'
if a; then
    echo "  Note: x" # verdict-ok: a hatch on the excuse line exempts nothing
    pass_test "X-17"
fi
PROBE
want="$(printf '%s\n' \
    "$probe/comment.sh:2" \
    "$probe/elif.sh:4" \
    "$probe/else.sh:4" \
    "$probe/function.sh:2" \
    "$probe/same-line.sh:1" \
    "$probe/printf.sh:2" \
    "$probe/heredoc-brace.sh:2" \
    "$probe/herestring.sh:3" \
    "$probe/quoted-heredoc.sh:5" \
    "$probe/unclosed.sh: heredoc DOC never closes" \
    "$probe/while-body.sh:2" \
    "$probe/stderr.sh:2" \
    "$probe/inner-loop.sh:2" \
    "$probe/hatch-elsewhere.sh:2" \
    "$probe/hatch-on-excuse.sh:2" \
    "scanned 19 20" | sort)"
got="$(scan_excuses "$probe" | sort)"
if [ "$got" = "$want" ]; then
    pass "the scan flags else, elif, function, comment, same-line, printf, heredoc, after-here-string, after-quoted-heredoc, loop-body, stderr, inner-loop and misplaced-hatch excuses and an unclosed heredoc, clears case, loop, ended-branch and hatched ones, and counts 20 calls in 19 files"
else
    fail "the scan on the placement probes printed:"
    printf '%s\n' "$got" | sed 's/^/    /'
    echo "    want:"
    printf '%s\n' "$want" | sed 's/^/    /'
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
