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
# are skipped, since a `}` in a manifest ends no shell block. They are read the
# way bash reads them: every opener on a line (`<<W`, `<<'W'`, `<<"W"`, `<<\W`,
# `<<-W`) queues a body, the bodies follow in order, and a terminator may be
# indented with tabs only after `<<-`. A terminator may be followed by the
# quote that closes a `bash -c '...'` or the `)` that closes a `$(...)` holding
# the heredoc. Openers are looked for in the line as one pass over its
# characters reduces it: a `'...'` span is kept as written, `\x` is one unit, a
# `#` after whitespace ends the line, a `((...))` span is dropped, and a
# `"..."` span is dropped unless it follows `<<` or is still open at the end of
# the line. Inside a `"..."` span, a `$(...)` is read like the top level, so
# the `"<<W"` in `"$(echo "<<W")"` opens nothing. Its `(` and `)` are counted
# without shell grammar, so a `case` pattern `x)` or a `((cmd) )` subshell on
# one line inside `$(...)` closes the span early and hides a later opener. A
# heredoc still open at the end of a file is reported, so a misread terminator
# cannot hide the rest of the file. Exits 1 when no file matched.
scan_excuses() {
    local files="$scratch/scan-files"
    find "$@" -name '*.sh' ! -name test-verdicts.sh -type f > "$files"
    if [ ! -s "$files" ]; then
        echo "scan_excuses: no .sh file under $*" >&2
        return 1
    fi
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    tr '\n' '\0' < "$files" | xargs -0 awk -v excuse="$excuse" '
        function skip_arith(   d, c) {
            for (d = 0; pos <= N; pos++) {
                c = substr(S, pos, 1)
                if (c == "(") d++
                else if (c == ")" && --d == 0) { pos++; return }
            }
        }
        function cmd(nested,   out, d, c, j) {
            out = ""; d = 0
            while (pos <= N) {
                c = substr(S, pos, 1)
                if (c == "\\") { out = out substr(S, pos, 2); pos += 2 }
                else if (c == "\047") {
                    j = index(substr(S, pos + 1), "\047")
                    if (j == 0) j = N - pos
                    out = out substr(S, pos, j + 1); pos += j + 1
                }
                else if (c == "#" && (pos == 1 || substr(S, pos - 1, 1) ~ /[[:space:]]/)) pos = N + 1
                else if (c == "\"") out = out dq(out)
                else if (substr(S, pos, 2) == "((") skip_arith()
                else if (nested && c == ")" && d-- == 0) { pos++; return out }
                else { if (c == "(") d++; out = out c; pos++ }
            }
            return out
        }
        function dq(before,   start, red, c, keep) {
            start = pos++; red = "\""; keep = 0
            while (pos <= N) {
                c = substr(S, pos, 1)
                if (c == "\\") { red = red substr(S, pos, 2); pos += 2 }
                else if (c == "\"") {
                    pos++
                    if (before ~ /<<-?[[:space:]]*$/) return substr(S, start, pos - start)
                    return keep ? red "\"" : ""
                }
                else if (substr(S, pos, 3) == "$((") { pos++; skip_arith() }
                else if (substr(S, pos, 2) == "$(") { pos += 2; keep = 1; red = red "$(" cmd(1) ")" }
                else { red = red c; pos++ }
            }
            return substr(S, start, N)
        }
        BEGIN { qh = 1 }
        FNR == 1 {
            if (qn >= qh) print prev ": heredoc " q[qh] " never closes"
            held = ""; qn = 0; qh = 1; prev = FILENAME; files++
        }
        /(^|[;&|[:space:]])pass_test[[:space:]]/ { calls++ }
        /(^|[;&|[:space:]])pass_test[[:space:]].*# verdict-ok: [^[:space:]]/ { held = ""; next }
        qn >= qh {
            if ($0 ~ ((qdash[qh] ? "^\t*" : "^") q[qh] "[\047\")]*$")) qh++
            next
        }
        $0 !~ /^[[:space:]]*#/ {
            S = $0; N = length(S); pos = 1
            rest = cmd(0)
            while (match(rest, /(^|[^<])<<-?[[:space:]]*\\?[\047"]?[A-Za-z_0-9][A-Za-z_0-9]*/)) {
                word = substr(rest, RSTART, RLENGTH)
                rest = substr(rest, RSTART + RLENGTH)
                qn++
                qdash[qn] = (word ~ /<<-/)
                gsub(/^[^<]?<<-?[[:space:]]*\\?[\047"]?/, "", word)
                q[qn] = word
            }
        }
        $0 ~ excuse {
            if ($0 ~ /(^|[;&|[:space:]])pass_test[[:space:]]/) { print FILENAME ":" FNR; held = "" }
            else { held = FILENAME ":" FNR }
            next
        }
        /^[[:space:]]*(if|elif|else|fi|esac|done)([[:space:];]|$)/ || /^[[:space:]]*\}/ || /;;[[:space:]]*$/ { held = ""; next }
        /(^|[;&|[:space:]])pass_test[[:space:]]/ && held != "" { print held; held = "" }
        END {
            if (qn >= qh) print prev ": heredoc " q[qh] " never closes"
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
cat > "$probe/backslash-heredoc.sh" <<'PROBE'
if a; then
    echo "  Note: applying anyway"
    kubectl apply -f - <<\EOF
{
}
EOF
    pass_test "X-20"
fi
PROBE
cat > "$probe/paired-heredoc.sh" <<'PROBE'
if a; then
    echo "  Note: two bodies"
    paste /dev/fd/3 3<<A <<B
{
A
}
B
    pass_test "X-21"
fi
PROBE
printf 'if a; then\n    echo "  Note: tab-indented EOF inside"\n    cat <<EOF\n\tEOF\n}\nEOF\n    pass_test "X-22"\nfi\n' > "$probe/indented-terminator.sh"
printf 'if a; then\n    echo "  Note: tab-stripped"\n    cat <<-EOF\n\t{\n\t}\n\tEOF\n    pass_test "X-23"\nfi\n' > "$probe/dash-heredoc.sh"
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
cat > "$probe/strfalse.sh" <<'PROBE'
echo "usage: cat <<EOF"
if a; then
    echo "  Note: x"
    cat <<EOF
}
EOF
    pass_test "X-24"
fi
PROBE
cat > "$probe/trailcomment.sh" <<'PROBE'
f # feed <<EOF
if a; then
    echo "  Note: x"
    cat <<EOF
}
EOF
    pass_test "X-25"
fi
PROBE
cat > "$probe/digit-heredoc.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    cat <<1
}
1
    pass_test "X-26"
fi
PROBE
cat > "$probe/arithmetic.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    n=$(( x << y )) m=$((1<<20))
    pass_test "X-27"
fi
PROBE
cat > "$probe/bash-c-word.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    kubectl exec p -- bash -c 'cat > f << "INNEREOF"
}
INNEREOF'
    pass_test "X-28"
fi
PROBE
cat > "$probe/double-quoted-word.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    cat <<"EOF"
}
EOF
    pass_test "X-29"
fi
PROBE
cat > "$probe/inner-paren.sh" <<'PROBE'
v="$( (echo x); echo "<<X")"
if a; then
    echo "  Note: x"
    cat <<EOF
}
EOF
    pass_test "X-35"
fi
PROBE
cat > "$probe/sq-open-hash.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    bash -c 'true # x; cat <<EOF
}
EOF'
    pass_test "X-36"
fi
PROBE
cat > "$probe/escaped-quote.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    echo "a\"b" x; cat <<EOF; echo "c"
}
EOF
    pass_test "X-30"
fi
PROBE
cat > "$probe/quote-in-single.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    echo 'a"b' ; cat <<EOF ; echo "c"
}
EOF
    pass_test "X-31"
fi
PROBE
cat > "$probe/open-string.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    bash -c "cat <<\"EOF\"
}
EOF"
    pass_test "X-32"
fi
PROBE
cat > "$probe/hash-in-single.sh" <<'PROBE'
if a; then
    echo "  Note: x"
    sed 's/ #.*//' <<EOF
}
EOF
    pass_test "X-33"
fi
PROBE
cat > "$probe/nested-string.sh" <<'PROBE'
v="$(echo "<<EOF")"
if a; then
    echo "  Note: x"
    cat <<EOF
}
EOF
    pass_test "X-34"
fi
PROBE
want="$(printf '%s\n' \
    "$probe/bash-c-word.sh:2" \
    "$probe/double-quoted-word.sh:2" \
    "$probe/escaped-quote.sh:2" \
    "$probe/inner-paren.sh:3" \
    "$probe/sq-open-hash.sh:2" \
    "$probe/quote-in-single.sh:2" \
    "$probe/open-string.sh:2" \
    "$probe/hash-in-single.sh:2" \
    "$probe/nested-string.sh:3" \
    "$probe/strfalse.sh:3" \
    "$probe/trailcomment.sh:3" \
    "$probe/digit-heredoc.sh:2" \
    "$probe/arithmetic.sh:2" \
    "$probe/comment.sh:2" \
    "$probe/elif.sh:4" \
    "$probe/else.sh:4" \
    "$probe/function.sh:2" \
    "$probe/same-line.sh:1" \
    "$probe/printf.sh:2" \
    "$probe/heredoc-brace.sh:2" \
    "$probe/herestring.sh:3" \
    "$probe/quoted-heredoc.sh:5" \
    "$probe/backslash-heredoc.sh:2" \
    "$probe/paired-heredoc.sh:2" \
    "$probe/indented-terminator.sh:2" \
    "$probe/dash-heredoc.sh:2" \
    "$probe/unclosed.sh: heredoc DOC never closes" \
    "$probe/while-body.sh:2" \
    "$probe/stderr.sh:2" \
    "$probe/inner-loop.sh:2" \
    "$probe/hatch-elsewhere.sh:2" \
    "$probe/hatch-on-excuse.sh:2" \
    "scanned 36 37" | sort)"
got="$(scan_excuses "$probe" | sort)"
if [ "$got" = "$want" ]; then
    pass "the scan flags else, elif, function, comment, same-line, printf, heredoc, after-here-string, after-quoted-heredoc, backslash, paired, indented-terminator, tab-stripped and digit-word heredoc, after-string, after-comment, arithmetic-shift, after-escaped-quote, after-single-quote, open-string, bash -c word, double-quoted word and nested-string, inner-paren, open-single-quote-hash, loop-body, stderr, inner-loop and misplaced-hatch excuses and an unclosed heredoc, clears case, loop, ended-branch and hatched ones, and counts 37 calls in 36 files"
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
