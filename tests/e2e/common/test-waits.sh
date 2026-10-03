#!/usr/bin/env bash
# Checks that every wait in an e2e script is a bounded poll on the state the
# next step reads. A `sleep` command may run only inside a function of
# common/helpers.sh whose body checks a deadline (`SECONDS`, `deadline` or
# `tries`), or on a line carrying `# sleep-ok: <why>`, which is kept for a wait
# on wall-clock behaviour under test. Every such line is printed with its why.
# The fixtures under common/fixtures/waits/ prove each arm, and no cluster is
# needed.
#
# Usage: tests/e2e/common/test-waits.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
e2e_root="$(dirname "$here")"
fixtures="$here/fixtures/waits"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

pass() { echo "PASS  $1"; }
fail() {
    echo "FAIL  $1"
    failures=$((failures + 1))
}

# There are about 90 scripts under tests/e2e/, so a count below this means the
# walk lost its files.
min_scanned_files=60

# scan_sleeps <dir>: read every *.sh under <dir> outside common/fixtures/ and
# print, one per line, `SLEEP <file>:<line> <reason>` for each sleep that breaks
# the rule, `HATCH <file>:<line> <why>` for each one a hatch keeps, then
# `scanned <files>`. Exits 1 when a file cannot be read, when there is no file
# to read, or when a SLEEP line was printed.
#
# Commands are read through heredocs.awk, so a heredoc body (a pod's
# `command: ["sleep", "3600"]`, a stub script) and a quoted word are not
# commands. A sleep counts where bash runs it as a command: first on the line,
# or after `;`, `&`, `|`, `(`, `{`, `!` or a `then`, `do`, `else`, `elif`,
# `exec`, `time`, `command`, `builtin` or `nohup` word. A function of
# helpers.sh runs from its `name() {` line in column 0 to the next `}` in
# column 0, the layout every function there keeps; a one-line function ends on
# its own line, and one left open is reported.
scan_sleeps() {
    local list="$scratch/scan-files" rc=0 file
    find "$1" -name '*.sh' ! -path '*/common/fixtures/*' | LC_ALL=C sort > "$list"
    if [ ! -s "$list" ]; then
        echo "scan_sleeps: no .sh file under $1" >&2
        return 1
    fi
    while IFS= read -r file; do
        if [ ! -f "$file" ] || [ ! -r "$file" ]; then
            echo "scan_sleeps: cannot read $file" >&2
            rc=1
        fi
    done < "$list"
    [ "$rc" -eq 0 ] || return 1
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    tr '\n' '\0' < "$list" | xargs -0 awk -f "$here/heredocs.awk" | awk -F '\t' '
        function report(f, l, why) {
            if (hatch[l] != "") { print "HATCH " f ":" l " " hatch[l]; return }
            print "SLEEP " f ":" l " " why
            bad = 1
        }
        function close_fn(   i) {
            for (i = 1; i <= np; i++)
                if (!deadline) report(file, pline[i], "runs in helpers.sh function " fn " whose body checks no deadline (SECONDS, deadline or tries)")
            np = 0; fn = ""; deadline = 0
        }
        function open_at_end() {
            if (fn == "") return
            print "SLEEP " file ":" fn_line " function " fn " has no closing } in column 0, so the walk cannot tell which sleeps it holds"
            bad = 1; np = 0; fn = ""
        }
        $1 == "FILE" {
            open_at_end()
            files++; file = $2; helpers = (file ~ /(^|\/)common\/helpers\.sh$/)
            fn = ""; np = 0; deadline = 0
            split("", hatch); split("", rawcode)
            next
        }
        $1 == "SH" {
            raw = $4
            for (i = 5; i <= NF; i++) raw = raw "\t" $i
            if (match(raw, /#[[:space:]]*sleep-ok:[[:space:]]*[^[:space:]].*$/)) {
                h = substr(raw, RSTART, RLENGTH); sub(/^#[[:space:]]*sleep-ok:[[:space:]]*/, "", h)
                hatch[$3] = h
            }
            code = raw; sub(/(^|[[:space:]])#.*$/, "", code)
            rawcode[$3] = code
            if (fn != "" && code ~ /(^|[^A-Za-z0-9_])SECONDS([^A-Za-z0-9_]|$)|deadline|tries/) deadline = 1
            next
        }
        $1 != "CMD" { next }
        {
            cmd = $4
            for (i = 5; i <= NF; i++) cmd = cmd "\t" $i
            header = helpers && cmd ~ /^(function[[:space:]]+)?[A-Za-z_][A-Za-z0-9_:.-]*[[:space:]]*\(\)/
            if (header) {
                open_at_end()
                fn = cmd; sub(/^(function[[:space:]]+)?/, "", fn); sub(/[[:space:]]*\(.*/, "", fn)
                fn_line = $3; np = 0; deadline = 0
                # The SH record of the header line came first, before fn was set.
                if (rawcode[$3] ~ /(^|[^A-Za-z0-9_])SECONDS([^A-Za-z0-9_]|$)|deadline|tries/) deadline = 1
            }
            if (cmd ~ /(^|[;&|({!])[[:space:]]*((then|do|else|elif|exec|time|command|builtin|nohup)[[:space:]]+)*sleep([[:space:];)&|]|$)/) {
                if (fn != "") pline[++np] = $3
                else report(file, $3, helpers ? "runs outside a function of helpers.sh" : "runs outside common/helpers.sh; wait on the state the next step reads")
            }
            if (fn != "" && (cmd ~ /^\}/ || (header && cmd ~ /[[:space:];]\}[[:space:]]*$/))) close_fn()
        }
        END { open_at_end(); print "scanned " files + 0; exit bad }
    ' || return 1
}

# The real tree: every sleep is a deadline loop's interval or a hatch.
if report="$(scan_sleeps "$e2e_root")"; then
    scanned="$(sed -n 's/^scanned //p' <<<"$report")"
    if [ "$scanned" -lt "$min_scanned_files" ]; then
        fail "the sleep walk read $scanned files, fewer than $min_scanned_files"
    else
        pass "every sleep under tests/e2e is a helpers.sh deadline loop's interval or a hatch ($scanned files)"
    fi
else
    fail "sleeps outside a bounded wait:"
    { grep '^SLEEP ' <<<"$report" || true; } | sed 's/^/    /'
fi
echo "Hatches (each why names the wall-clock behaviour the sleep waits on):"
{ grep '^HATCH ' <<<"${report:-}" || true; } | sed 's/^HATCH /    /'

# probe <name> <want: pass|fail> <description>: run the walk on one fixture
# tree and say whether it passed or failed as it should.
probe() {
    local out rc=0
    out="$(scan_sleeps "$1" 2>&1)" || rc=$?
    case "$2:$((rc != 0))" in
        pass:0 | fail:1) pass "$3" ;;
        *)
            fail "$3 (walk exited $rc):"
            printf '%s\n' "$out" | sed 's/^/    /'
            ;;
    esac
}

# The fixtures live under common/fixtures/, which the walk skips, so each is
# copied to a scratch tree of its own first.
fixture_tree() {
    rm -rf "${scratch:?}/tree"
    mkdir -p "$scratch/tree"
    cp -R "$fixtures/$1/." "$scratch/tree/"
    echo "$scratch/tree"
}

probe "$(fixture_tree bare-sleep)" fail "a bare sleep in a suite script fails the walk"
probe "$(fixture_tree deadline-helper)" pass "a sleep in a helpers.sh function with a deadline loop passes"
probe "$(fixture_tree no-deadline-helper)" fail "a sleep in a helpers.sh function with no deadline check fails the walk"
probe "$(fixture_tree top-level-helper)" fail "a sleep in helpers.sh outside any function fails the walk"
probe "$(fixture_tree unclosed-helper)" fail "a helpers.sh function with no closing } in column 0 fails the walk"
probe "$(fixture_tree not-a-command)" pass "a sleep in a heredoc body, a quoted word or a comment is no command"
probe "$(fixture_tree hatch)" pass "a sleep carrying '# sleep-ok: <why>' passes"
probe "$(fixture_tree empty-hatch)" fail "a '# sleep-ok:' with no why fails the walk"

tree="$(fixture_tree deadline-helper)"
ln -s "$scratch/no-such-file" "$tree/gone.sh"
probe "$tree" fail "a script the walk cannot read fails it"
probe "$scratch/no-such-dir" fail "a walk with no .sh file to read fails"

hatched="$(scan_sleeps "$(fixture_tree hatch)")"
if grep -q '^HATCH .*/suite.sh:[0-9]* the daemon under test reconciles every 5s$' <<<"$hatched"; then
    pass "the walk prints each hatch with its why"
else
    fail "the walk did not print the hatch's why:"
    printf '%s\n' "$hatched" | sed 's/^/    /'
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
