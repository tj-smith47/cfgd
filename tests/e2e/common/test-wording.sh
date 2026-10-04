#!/usr/bin/env bash
# Checks the prose of every e2e script: no em dash anywhere on a line, no
# first-person pronoun in a comment or in a quoted string of a begin_test,
# pass_test, fail_test, skip_test, echo, printf or log line, and no contrast
# frame in a comment. The frames are the Contrast frames section of
# .claude/scripts/commit-wording.txt, the list commit messages are held to, and
# a run of whole-line comments is read as one text, so a frame broken across
# lines is found. The quoted
# arguments of an assert_* call are exempt, because they are CLI output the
# case asserts verbatim; a comment after them is still read. Any other line
# that has to hold such text carries `# wording-ok: <why>`, and the scan prints
# each hatch with its why. The fixtures under common/fixtures/wording/ prove
# each arm, and no cluster is needed.
#
# Usage: tests/e2e/common/test-wording.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/census.sh
source "$here/census.sh"
e2e_root="$(dirname "$here")"
fixtures="$here/fixtures/wording"
wording_list="$(dirname "$(dirname "$e2e_root")")/.claude/scripts/commit-wording.txt"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0
frames="$scratch/contrast-frames"
awk '/^# Contrast frames/ { on = 1; next } on && /^[[:space:]]*$/ { exit } on && !/^#/' "$wording_list" > "$frames"

pass() { echo "PASS  $1"; }
fail() {
    echo "FAIL  $1"
    failures=$((failures + 1))
}

# scan_wording <dir>: print `WORDING <file>:<line> <what>` for each breach in
# a *.sh under <dir> outside common/fixtures/, `HATCH <file>:<line> <why>` for
# each wording-ok hatch, then `scanned <n>`. Exits 1 when
# find fails, when there is no file, when a non-empty file find listed never
# reached awk (each one is named), or when a WORDING line was printed.
scan_wording() {
    local list="$scratch/wording-files" read="$scratch/wording-read" rc=0
    if ! find "$1" -name '*.sh' ! -path '*/common/fixtures/*' > "$list.raw"; then
        echo "scan_wording: find failed under $1" >&2
        return 1
    fi
    LC_ALL=C sort "$list.raw" > "$list"
    if [ ! -s "$list" ]; then
        echo "scan_wording: no .sh file under $1" >&2
        return 1
    fi
    : > "$read"
    # shellcheck disable=SC2016  # the single-quoted text is an awk program
    tr '\n' '\0' < "$list" | xargs -0 awk -v readlog="$read" -v framelist="$frames" '
        BEGIN { while ((getline f < framelist) > 0) frame[++nframes] = f }
        FNR == 1 { frame_flush(); files++; print FILENAME > readlog }
        function flag(what) { print "WORDING " FILENAME ":" FNR " " what; bad = 1 }
        # A contrast frame in the comment text gathered since the last flush,
        # reported at the line its match starts on. fo[n] is the offset in
        # fbuf where the text of line fl[n] starts.
        function frame_flush(   t, k, n) {
            t = tolower(fbuf)
            for (k = 1; fbuf != "" && k <= nframes; k++)
                if (match(t, frame[k])) {
                    for (n = nlines; n > 1 && fo[n] > RSTART; n--) ;
                    print "WORDING " ffile ":" fl[n] " contrast frame"; bad = 1
                }
            fbuf = ""; nlines = 0
        }
        function frame_add(text) {
            gsub(/[[:space:]]+/, " ", text); sub(/^ /, "", text); sub(/ $/, "", text)
            if (fbuf == "") ffile = FILENAME
            fl[++nlines] = FNR; fo[nlines] = length(fbuf) + 1
            fbuf = fbuf " " text
        }
        function first_person(text) {
            return text ~ /(^|[^A-Za-z0-9_-])([Ww]e|I)([^A-Za-z0-9_\/]|$)/
        }
        {
            line = $0
            if (match(line, /(^|[[:space:]])#[[:space:]]*wording-ok:[[:space:]]*[^[:space:]].*$/)) {
                why = substr(line, RSTART, RLENGTH); sub(/^[[:space:]]*#[[:space:]]*wording-ok:[[:space:]]*/, "", why)
                print "HATCH " FILENAME ":" FNR " " why
                next
            }
            if (match(line, /assert_[a-z_]+[[:space:]]/)) {
                rest = substr(line, RSTART + RLENGTH)
                gsub(/"([^"\\]|\\.)*"|\047[^\047]*\047/, "\"\"", rest)
                line = substr(line, 1, RSTART + RLENGTH - 1) rest
            }
            # U+2014 spelled as its UTF-8 bytes, so this line holds none.
            if (index(line, "\342\200\224")) flag("prose em dash")
            if (line ~ /^#!/) { frame_flush(); next }
            # A # inside a quoted string starts no comment, so the comment is
            # found on a copy with each quoted string blanked to Qs of the
            # same length, which keeps offsets equal to those in line.
            masked = line
            while (match(masked, /"([^"\\]|\\.)*"|\047[^\047]*\047/)) {
                q = ""; for (k = 0; k < RLENGTH; k++) q = q "Q"
                masked = substr(masked, 1, RSTART - 1) q substr(masked, RSTART + RLENGTH)
            }
            comment = ""
            if (match(masked, /(^|[[:space:]])#/)) comment = substr(line, RSTART + RLENGTH)
            if (line ~ /^[[:space:]]*#/) frame_add(comment)
            else { frame_flush(); if (comment != "") { frame_add(comment); frame_flush() } }
            if (first_person(comment)) { flag("first-person word in a comment"); next }
            code = substr(line, 1, length(line) - length(comment))
            if (code !~ /(^|[;&|(])[[:space:]]*((if|then|else|do|!)[[:space:]]+)*(begin_test|pass_test|fail_test|skip_test|echo|printf|log)([[:space:]]|$)/) next
            while (match(code, /"([^"\\]|\\.)*"/)) {
                if (first_person(substr(code, RSTART + 1, RLENGTH - 2))) { flag("first-person word in a message"); next }
                code = substr(code, RSTART + RLENGTH)
            }
        }
        END { frame_flush(); print "scanned " files + 0; exit bad }
    ' || rc=1
    census_unread scan_wording "$list" "$read" || rc=1
    return "$rc"
}

# probe <fixture or dir> <want: pass|fail> <description> [line regex]: run the
# scan on a copy of one fixture, or of a directory built here; a failure also
# has to print a line matching the regex.
probe() {
    local out rc=0 src="$fixtures/$1"
    [ -d "$1" ] && src="$1"
    rm -rf "${scratch:?}/tree"
    mkdir -p "$scratch/tree"
    cp -R "$src/." "$scratch/tree/"
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

if [ -s "$frames" ]; then
    pass "the contrast frames read from commit-wording.txt ($(wc -l < "$frames" | tr -d ' ') patterns)"
else
    fail "no contrast frame was read from $wording_list"
fi

# The fixtures sit under common/fixtures/, which the scan skips, so each one
# is copied to a scratch tree first.
probe dash-comment fail "an em dash in a comment fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe dash-title fail "an em dash in a begin_test title fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe dash-message fail "an em dash in a fail_test message fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe we-comment fail "\"we\" in a comment fails" '^WORDING .*/suite\.sh:2 first-person word in a comment$'
probe i-message fail "\"I\" in an echo message fails" '^WORDING .*/suite\.sh:2 first-person word in a message$'

probe frame-rather-than fail "\"rather than\" in a comment fails" '^WORDING .*/suite\.sh:2 contrast frame$'
probe frame-instead-of fail "\"instead of\" in a trailing comment fails" '^WORDING .*/suite\.sh:2 contrast frame$'
probe frame-comma-not fail "a \", not\" frame broken across two comment lines fails at the first" '^WORDING .*/suite\.sh:2 contrast frame$'
probe quoted-hash pass "a # inside a quoted string starts no comment"
probe frame-hatched pass "a contrast frame on a line carrying a wording-ok hatch passes"

# The census: each way a listed script can fail to reach awk fails the scan.
# find and awk are stubbed through PATH, so each failure is the one named.
mkdir -p "$scratch/empty-dir" "$scratch/dangling" "$scratch/find-fails" "$scratch/awk-drops"
probe "$scratch/empty-dir" fail "a scan with no .sh file to read fails" '^scan_wording: no \.sh file under'
cp -R "$fixtures/clean/." "$scratch/dangling/"
ln -s "$scratch/no-such-file" "$scratch/dangling/gone.sh"
probe "$scratch/dangling" fail "a script awk cannot open fails the scan by name" '^scan_wording: .*/gone\.sh was listed and never read$'
printf '#!/bin/sh\nexit 1\n' > "$scratch/find-fails/find"
# shellcheck disable=SC2016  # the $ belong to the stub script, expanded when it runs
printf '#!/usr/bin/env bash\nset -- "${@:1:$#-1}"\nexec %q "$@"\n' "$(command -v awk)" > "$scratch/awk-drops/awk"
chmod +x "$scratch/find-fails/find" "$scratch/awk-drops/awk"
PATH="$scratch/find-fails:$PATH" probe clean fail "a find that fails fails the scan" '^scan_wording: find failed under'
PATH="$scratch/awk-drops:$PATH" probe clean fail "a script find listed and awk never read fails the scan by name" '^scan_wording: .*/suite\.sh was listed and never read$'

probe dash-after-assert fail "an em dash in a comment after an assert_* call fails" '^WORDING .*/suite\.sh:2 prose em dash$'
probe dash-hatched pass "a line carrying a wording-ok hatch with its why passes"
rm -rf "${scratch:?}/tree"
mkdir -p "$scratch/tree"
cp -R "$fixtures/dash-hatched/." "$scratch/tree/"
if scan_wording "$scratch/tree" | grep -q '^HATCH .*/suite\.sh:2 cfgd status prints this em dash and the case greps it verbatim$'; then
    pass "the scan prints each hatch with its why"
else
    fail "the scan did not print the hatch's why"
fi
probe clean pass "assert_* arguments, -I flags, I/O, words holding we or I and a non-message command pass"

real="$(scan_wording "$e2e_root" 2>&1)" && rc=0 || rc=$?
scanned="$(sed -n 's/^scanned //p' <<<"$real")"
if [ "$rc" -eq 0 ] && [ "${scanned:-0}" -ge 60 ]; then
    pass "no e2e script has a prose em dash, a first-person word or a contrast frame in a comment ($scanned scripts)"
    grep '^HATCH ' <<<"$real" | sed 's/^/    /' || true
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
