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
# brace), then a last line `scanned <files> <pass_test calls>`. Lines come from
# heredocs.awk, so heredoc bodies are skipped: a `}` in a manifest ends no shell
# block. A heredoc still open at the end of a file is reported, so a misread
# terminator cannot hide the rest of the file. A file that cannot be read is
# reported too. Exits 1 when no file matched.
scan_excuses() {
    local files="$scratch/scan-files" readable="$scratch/scan-readable" f
    find "$@" -name '*.sh' ! -name test-verdicts.sh \( -type f -o -type l \) > "$files"
    if [ ! -s "$files" ]; then
        echo "scan_excuses: no .sh file under $*" >&2
        return 1
    fi
    : > "$readable"
    while IFS= read -r f; do
        if [ -f "$f" ] && [ -r "$f" ]; then printf '%s\0' "$f" >> "$readable"; else echo "$f: unreadable"; fi
    done < "$files"
    [ -s "$readable" ] || { echo "scanned 0 0"; return 0; }
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    { xargs -0 awk -f "$here/heredocs.awk" < "$readable" || echo "UNREADABLE"; } | awk -F '\t' -v excuse="$excuse" '
        function rest(n,   i, p) { p = 0; for (i = 1; i <= n; i++) p += length($i) + 1; return substr($0, p + 1) }
        $1 == "FILE" { files++; held = ""; next }
        $1 == "UNREADABLE" { print "heredocs.awk could not read the scripts"; next }
        $1 == "UNCLOSED" { print $2 ": heredoc " $4 " never closes"; next }
        $1 != "SH" && $1 != "BODY" { next }
        {
            where = $2 ":" $3
            line = ($1 == "SH") ? rest(3) : rest(4)
        }
        line ~ /(^|[;&|[:space:]])pass_test[[:space:]]/ { calls++ }
        line ~ /(^|[;&|[:space:]])pass_test[[:space:]].*# verdict-ok: [^[:space:]]/ { held = ""; next }
        $1 == "BODY" { next }
        line ~ excuse {
            if (line ~ /(^|[;&|[:space:]])pass_test[[:space:]]/) { print where; held = "" }
            else { held = where }
            next
        }
        line ~ /^[[:space:]]*(if|elif|else|fi|esac|done)([[:space:];]|$)/ || line ~ /^[[:space:]]*\}/ || line ~ /;;[[:space:]]*$/ { held = ""; next }
        line ~ /(^|[;&|[:space:]])pass_test[[:space:]]/ && held != "" { print held; held = "" }
        END { print "scanned " files + 0 " " calls + 0 }
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

mkdir -p "$scratch/dangling"
ln -s "$scratch/nowhere.sh" "$scratch/dangling/gone.sh"
dangling="$(scan_excuses "$scratch/dangling" 2>&1 || true)"
if grep -qxF "$scratch/dangling/gone.sh: unreadable" <<<"$dangling"; then
    pass "a script the scan cannot read is reported"
else
    fail "a dangling symlink was not reported unreadable: $dangling"
fi

# heredocs.awk on one fixture per way a script opens, fills and closes a
# heredoc, and per text that only looks like an opener. Every fixture is valid
# shell, so the reader is judged on what bash would read. Tabs in the record
# stream show as " | ".
reader="$scratch/reader"
mkdir -p "$reader"
printf 'paste /dev/fd/3 3<<A <<B\na\nA\nb\nB\n' > "$reader/paired.sh"
printf 'cat <<-EOF\n\tx\n\tEOF\n' > "$reader/dash.sh"
cat > "$reader/backslash.sh" <<'FIXTURE'
cat <<\EOF
$x
EOF
FIXTURE
printf "cat <<'EOF'\n\$x\nEOF\ncat << \"END\"\n\$y\nEND\n" > "$reader/quoted.sh"
printf 'cat <<DOC\nnever closed\n' > "$reader/unclosed.sh"
printf 'grep -q x <<<abc\n' > "$reader/herestring.sh"
printf 'cat <<EOF\n\tEOF\nEOF\n' > "$reader/indented-terminator.sh"
printf 'kubectl apply -n ns \\\n    -f - <<EOF\nx: 1\nEOF\n' > "$reader/continued.sh"
cat > "$reader/captured.sh" <<'FIXTURE'
r=$(kubectl apply -f - 2>&1 <<EOF || true
x: 1
EOF
)
FIXTURE
printf '# usage: f <<EOF\ntrue # feed <<EOF\n' > "$reader/comment.sh"
printf 'echo "usage: cat <<EOF"\n' > "$reader/double-quoted.sh"
cat > "$reader/arithmetic.sh" <<'FIXTURE'
n=$((a<<b))
FIXTURE
printf "bash -c 'cat <<A\nx\nA'\nbash -c \"cat <<B\nx\nB\"\nv=\$(cat <<C\nx\nC)\n" > "$reader/terminator-suffix.sh"
printf 'cat <<1\nx\n1\n' > "$reader/digit.sh"
for f in "$reader"/*.sh; do
    bash -n "$f" 2>/dev/null || fail "reader fixture $(basename "$f") is not valid shell"
done
want_records="$(cat <<'WANT'
FILE | arithmetic.sh
SH | arithmetic.sh | 1 | n=$((a<<b))
FILE | backslash.sh
SH | backslash.sh | 1 | cat <<\EOF
OPEN | backslash.sh | 1 | 1 | EOF | 1 | 0 | cat <<\EOF
BODY | backslash.sh | 2 | 1 | $x
CLOSE | backslash.sh | 3 | 1
FILE | captured.sh
SH | captured.sh | 1 | r=$(kubectl apply -f - 2>&1 <<EOF || true
OPEN | captured.sh | 1 | 1 | EOF | 0 | 0 | r=$(kubectl apply -f - 2>&1 <<EOF || true
BODY | captured.sh | 2 | 1 | x: 1
CLOSE | captured.sh | 3 | 1
SH | captured.sh | 4 | )
FILE | comment.sh
SH | comment.sh | 1 | # usage: f <<EOF
SH | comment.sh | 2 | true # feed <<EOF
FILE | continued.sh
SH | continued.sh | 1 | kubectl apply -n ns \
SH | continued.sh | 2 |     -f - <<EOF
OPEN | continued.sh | 2 | 1 | EOF | 0 | 0 | kubectl apply -n ns      -f - <<EOF
BODY | continued.sh | 3 | 1 | x: 1
CLOSE | continued.sh | 4 | 1
FILE | dash.sh
SH | dash.sh | 1 | cat <<-EOF
OPEN | dash.sh | 1 | 1 | EOF | 0 | 1 | cat <<-EOF
BODY | dash.sh | 2 | 1 |  | x
CLOSE | dash.sh | 3 | 1
FILE | digit.sh
SH | digit.sh | 1 | cat <<1
OPEN | digit.sh | 1 | 1 | 1 | 0 | 0 | cat <<1
BODY | digit.sh | 2 | 1 | x
CLOSE | digit.sh | 3 | 1
FILE | double-quoted.sh
SH | double-quoted.sh | 1 | echo "usage: cat <<EOF"
FILE | herestring.sh
SH | herestring.sh | 1 | grep -q x <<<abc
FILE | indented-terminator.sh
SH | indented-terminator.sh | 1 | cat <<EOF
OPEN | indented-terminator.sh | 1 | 1 | EOF | 0 | 0 | cat <<EOF
BODY | indented-terminator.sh | 2 | 1 |  | EOF
CLOSE | indented-terminator.sh | 3 | 1
FILE | paired.sh
SH | paired.sh | 1 | paste /dev/fd/3 3<<A <<B
OPEN | paired.sh | 1 | 1 | A | 0 | 0 | paste /dev/fd/3 3<<A <<B
OPEN | paired.sh | 1 | 2 | B | 0 | 0 | paste /dev/fd/3 3<<A <<B
BODY | paired.sh | 2 | 1 | a
CLOSE | paired.sh | 3 | 1
BODY | paired.sh | 4 | 2 | b
CLOSE | paired.sh | 5 | 2
FILE | quoted.sh
SH | quoted.sh | 1 | cat <<'EOF'
OPEN | quoted.sh | 1 | 1 | EOF | 1 | 0 | cat <<'EOF'
BODY | quoted.sh | 2 | 1 | $x
CLOSE | quoted.sh | 3 | 1
SH | quoted.sh | 4 | cat << "END"
OPEN | quoted.sh | 4 | 2 | END | 1 | 0 | cat << "END"
BODY | quoted.sh | 5 | 2 | $y
CLOSE | quoted.sh | 6 | 2
FILE | terminator-suffix.sh
SH | terminator-suffix.sh | 1 | bash -c 'cat <<A
OPEN | terminator-suffix.sh | 1 | 1 | A | 0 | 0 | bash -c 'cat <<A
BODY | terminator-suffix.sh | 2 | 1 | x
CLOSE | terminator-suffix.sh | 3 | 1
SH | terminator-suffix.sh | 4 | bash -c "cat <<B
OPEN | terminator-suffix.sh | 4 | 2 | B | 0 | 0 | bash -c "cat <<B
BODY | terminator-suffix.sh | 5 | 2 | x
CLOSE | terminator-suffix.sh | 6 | 2
SH | terminator-suffix.sh | 7 | v=$(cat <<C
OPEN | terminator-suffix.sh | 7 | 3 | C | 0 | 0 | v=$(cat <<C
BODY | terminator-suffix.sh | 8 | 3 | x
CLOSE | terminator-suffix.sh | 9 | 3
FILE | unclosed.sh
SH | unclosed.sh | 1 | cat <<DOC
OPEN | unclosed.sh | 1 | 1 | DOC | 0 | 0 | cat <<DOC
BODY | unclosed.sh | 2 | 1 | never closed
UNCLOSED | unclosed.sh | 1 | DOC
WANT
)"
got_records="$(cd "$reader" && awk -f "$here/heredocs.awk" ./*.sh 2>&1 | sed 's|\./||; s|\t| \| |g')"
if [ "$got_records" = "$want_records" ]; then
    pass "heredocs.awk reads paired, tab-stripped, backslash, quoted, unclosed, indented-terminator, continued, captured, digit and quote- or paren-closed heredocs, and opens none for a here-string, a comment, a double-quoted string or an arithmetic shift"
else
    fail "heredocs.awk printed records that differ (< want, > got):"
    diff <(printf '%s\n' "$want_records") <(printf '%s\n' "$got_records") | grep '^[<>]' | sed 's/^/    /' || true
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
