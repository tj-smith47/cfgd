#!/usr/bin/env bash
# Checks that every wait in an e2e script is a bounded poll on the state the
# next step reads. A `sleep` command may run only:
#   - inside a function of common/helpers.sh whose body tests a deadline: a
#     `[`, `[[` or `((` test naming SECONDS, $deadline or $tries;
#   - inside run_every, the one background cadence in common/helpers.sh, which
#     is printed under Cadences;
#   - on a line carrying `# sleep-ok: <why>`, kept for a wait on wall-clock
#     behaviour under test and printed under Hatches with its why.
# The fixtures under common/fixtures/waits/ prove each arm, and no cluster is
# needed.
#
# Usage: tests/e2e/common/test-waits.sh               check tests/e2e
#        tests/e2e/common/test-waits.sh --census DIR  print every sleep under DIR
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

# There are about 90 scripts under tests/e2e/, so a list shorter than this
# means find lost files.
min_listed_files=60

# scan_sleeps <dir>: read every *.sh under <dir> outside common/fixtures/ and
# print, one per line:
#   SITE <file>:<line>          every sleep the walk finds
#   SLEEP <file>:<line> <why>   each one that breaks the rule
#   HATCH <file>:<line> <why>   each one a sleep-ok hatch keeps
#   CADENCE <file>:<line> <fn>  the run_every function of helpers.sh
#   listed <n>, scanned <n>     the scripts find listed and awk read
# Exits 1 when find fails, when there is no file to read, when a file cannot
# be read, when awk read fewer non-empty files than find listed, or when a
# SLEEP line was printed.
#
# Commands are read through heredocs.awk, so a heredoc body (a pod's
# `command: ["sleep", "3600"]`, a stub script) and a quoted word are not
# commands. A sleep counts where bash runs it as a command: first on the line,
# or after `;`, `&`, `|`, `(`, `{`, `!`, `` ` ``, `)` (a case arm), ` -- `
# (`kubectl exec <pod> -- sleep`), or a run of `if`, `while`, `until`, `then`,
# `do`, `else`, `elif`, `exec`, `time`, `command`, `builtin`, `nohup`, `env`,
# `nice`, `xargs`, `exec_in_pod` and `timeout <duration>` words; spelled
# `sleep`, `\sleep`, `/bin/sleep` or `/usr/bin/sleep`. The script of a
# `bash -c '...'` or `sh -c "..."` is read the same way. A function of
# helpers.sh runs from its `name() {` line in column 0 to the next `}` in
# column 0, the layout every function there keeps; a one-line function ends on
# its own line, and one left open is reported.
scan_sleeps() {
    local list="$scratch/scan-files" out="$scratch/scan-out" rc=0 file listed empty=0 scanned
    if ! find "$1" -name '*.sh' ! -path '*/common/fixtures/*' > "$list.raw"; then
        echo "scan_sleeps: find failed under $1" >&2
        return 1
    fi
    LC_ALL=C sort "$list.raw" > "$list"
    listed="$(wc -l < "$list" | tr -d ' ')"
    if [ "$listed" -eq 0 ]; then
        echo "scan_sleeps: no .sh file under $1" >&2
        return 1
    fi
    while IFS= read -r file; do
        if [ ! -f "$file" ] || [ ! -r "$file" ]; then
            echo "scan_sleeps: cannot read $file" >&2
            rc=1
        elif [ ! -s "$file" ]; then
            empty=$((empty + 1))
        fi
    done < "$list"
    [ "$rc" -eq 0 ] || return 1
    # shellcheck disable=SC2016 # the single-quoted text is an awk program
    tr '\n' '\0' < "$list" | xargs -0 awk -f "$here/heredocs.awk" | awk -F '\t' -v helpers_path="$1/common/helpers.sh" '
        BEGIN {
            # A sleep at a command position, as the comment above lists them.
            SLEEP_RE = "(^|[;&|({!`)]|[[:space:]]--[[:space:]])[[:space:]]*" \
                "((if|while|until|then|do|else|elif|exec|time|command|builtin|nohup|env|nice|xargs|exec_in_pod" \
                "|timeout([[:space:]]+-[^[:space:]]+)*[[:space:]]+[0-9.]+[smhd]?)[[:space:]]+)*" \
                "(\\\\|/(usr/)?bin/)?sleep([[:space:];)&|`]|$)"
            # The opening quote of a shell -c script.
            DASH_C_RE = "(^|[^A-Za-z0-9_])(ba|da|k|z)?sh([[:space:]]+-[A-Za-z]+)*[[:space:]]+-[A-Za-z]*c[[:space:]]+[\047\"]"
            # A deadline test: a [, [[ or (( (an arithmetic command, not $(( ))
            # whose text names SECONDS, $deadline or $tries as whole words.
            TEST_RE = "(\\[|(^|[^$])\\(\\()[^]]*([^A-Za-z0-9_]SECONDS|\\$\\{?(deadline|tries))([^A-Za-z0-9_]|$)"
        }
        function report(l, why) {
            if (hatch[l] != "") { print "HATCH " file ":" l " " hatch[l]; return }
            print "SLEEP " file ":" l " " why
            bad = 1
        }
        function site(l) {
            if (l in seen) return
            seen[l] = 1
            print "SITE " file ":" l
            if (fn != "") pline[++np] = l
            else report(l, helpers ? "runs outside a function of helpers.sh" : "runs outside common/helpers.sh; wait on the state the next step reads")
        }
        function close_fn(   i) {
            if (fn == "run_every") { if (np) print "CADENCE " file ":" fn_line " run_every" }
            else if (!deadline)
                for (i = 1; i <= np; i++)
                    report(pline[i], "runs in helpers.sh function " fn " whose body tests no deadline (a [, [[ or (( test naming SECONDS, $deadline or $tries)")
            np = 0; fn = ""; deadline = 0
        }
        function open_at_end() {
            if (fn == "") return
            print "SLEEP " file ":" fn_line " function " fn " has no closing } in column 0, so the walk cannot tell which sleeps it holds"
            bad = 1; np = 0; fn = ""
        }
        # dash_c(code, l): a sleep inside the script of a sh -c on line l.
        function dash_c(code, l,   q, j) {
            while (match(code, DASH_C_RE)) {
                q = substr(code, RSTART + RLENGTH - 1, 1)
                code = substr(code, RSTART + RLENGTH)
                j = index(code, q)
                if ((j ? substr(code, 1, j - 1) : code) ~ SLEEP_RE) site(l)
                if (!j) return
                code = substr(code, j + 1)
            }
        }
        $1 == "FILE" {
            open_at_end()
            files++; file = $2; helpers = (file == helpers_path)
            fn = ""; np = 0; deadline = 0
            split("", hatch); split("", rawcode); split("", seen)
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
            if (fn != "" && code ~ TEST_RE) deadline = 1
            dash_c(code, $3)
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
                if (rawcode[$3] ~ TEST_RE) deadline = 1
            }
            if (cmd ~ SLEEP_RE) site($3)
            if (fn != "" && (cmd ~ /^\}/ || (header && cmd ~ /[[:space:];]\}[[:space:]]*$/))) close_fn()
        }
        END { open_at_end(); print "scanned " files + 0; exit bad }
    ' > "$out" || rc=1
    cat "$out"
    echo "listed $listed"
    scanned="$(sed -n 's/^scanned //p' "$out")"
    if [ "${scanned:-0}" -ne $((listed - empty)) ]; then
        echo "scan_sleeps: awk read ${scanned:-0} of the $((listed - empty)) non-empty scripts find listed under $1" >&2
        return 1
    fi
    return "$rc"
}

if [ "${1:-}" = "--census" ]; then
    [ -n "${2:-}" ] || { echo "usage: $0 --census DIR" >&2; exit 2; }
    { scan_sleeps "$2" || true; } | sed -n 's/^SITE //p'
    exit 0
fi

# The real tree: every sleep is a deadline loop's interval, the cadence or a
# hatch.
if report="$(scan_sleeps "$e2e_root")"; then
    listed="$(sed -n 's/^listed //p' <<<"$report")"
    if [ "$listed" -lt "$min_listed_files" ]; then
        fail "the sleep walk listed $listed files, fewer than $min_listed_files"
    else
        pass "every sleep under tests/e2e is a helpers.sh deadline loop's interval, the cadence or a hatch ($listed files)"
    fi
else
    fail "sleeps outside a bounded wait:"
    { grep '^SLEEP ' <<<"$report" || true; } | sed 's/^/    /'
fi
echo "Cadences (background refreshes that outlive the step starting them):"
{ grep '^CADENCE ' <<<"${report:-}" || true; } | sed 's/^CADENCE /    /'
echo "Hatches (each why names the wall-clock behaviour under test the sleep waits on):"
{ grep '^HATCH ' <<<"${report:-}" || true; } | sed 's/^HATCH /    /'

# probe <tree> <want: pass|fail> <description> [why]: run the walk on one
# fixture tree and say whether it passed or failed as it should; a failure
# also has to print a line matching the extended regex <why>.
probe() {
    local out rc=0
    out="$(scan_sleeps "$1" 2>&1)" || rc=$?
    case "$2:$((rc != 0))" in
        pass:0) pass "$3" ;;
        fail:1)
            if [ -z "${4:-}" ] || grep -Eq -- "$4" <<<"$out"; then
                pass "$3"
            else
                fail "$3 (the walk failed without printing /$4/):"
                printf '%s\n' "$out" | sed 's/^/    /'
            fi
            ;;
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
probe "$(fixture_tree mention-only-helper)" fail "a helpers.sh function that names a deadline in a message and tests none fails the walk" \
    '^SLEEP .*/common/helpers.sh:5 .*function settle whose body tests no deadline'
probe "$(fixture_tree assigned-only-helper)" fail "a helpers.sh function that sets a deadline and never tests it fails the walk" \
    '^SLEEP .*/common/helpers.sh:5 .*function settle whose body tests no deadline'
probe "$(fixture_tree substring-helper)" fail "a helpers.sh function testing a word that only contains tries fails the walk" \
    '^SLEEP .*/common/helpers.sh:6 .*function settle whose body tests no deadline'
probe "$(fixture_tree nested-helpers-path)" fail "a common/helpers.sh below the walked tree's own is a suite script" \
    '^SLEEP .*/foo/common/helpers.sh:8 runs outside common/helpers.sh'
probe "$(fixture_tree cadence-helper)" pass "run_every in helpers.sh passes"
probe "$(fixture_tree run-every-outside-helpers)" fail "a run_every-shaped loop outside helpers.sh fails the walk" \
    '^SLEEP .*/suite.sh:9 runs outside common/helpers.sh'

# Every spelling on a line below the marker is a sleep bash runs, and each
# has to come out as one SLEEP line.
spelled="$(scan_sleeps "$(fixture_tree spellings)" 2>&1 || true)"
spellings_want="$(sed -n '/^# spellings:$/,$p' "$fixtures/spellings/suite.sh" | sed 1d | grep -c .)"
spellings_got="$(grep -c '^SLEEP .*/suite.sh:' <<<"$spelled" || true)"
if [ "$spellings_got" -eq "$spellings_want" ]; then
    pass "the walk names each of the $spellings_want sleep spellings"
else
    fail "the walk named $spellings_got of the $spellings_want sleep spellings:"
    printf '%s\n' "$spelled" | sed 's/^/    /'
fi

cadenced="$(scan_sleeps "$(fixture_tree cadence-helper)" 2>&1 || true)"
if grep -q '^CADENCE .*/common/helpers.sh:3 run_every$' <<<"$cadenced"; then
    pass "the walk lists run_every under its cadence line"
else
    fail "the walk did not list run_every as a cadence:"
    printf '%s\n' "$cadenced" | sed 's/^/    /'
fi

tree="$(fixture_tree deadline-helper)"
ln -s "$scratch/no-such-file" "$tree/gone.sh"
probe "$tree" fail "a script the walk cannot read fails it" '^scan_sleeps: cannot read .*/gone\.sh$'

mkdir -p "$scratch/empty-dir"
probe "$scratch/empty-dir" fail "a walk with no .sh file to read fails" '^scan_sleeps: no \.sh file under'

tree="$(fixture_tree deadline-helper)"
: > "$tree/empty.sh"
probe "$tree" pass "an empty script counts as read"

# find and awk are stubbed through PATH, so each failure below is the one
# named and no other.
real_awk="$(command -v awk)"
mkdir -p "$scratch/find-fails" "$scratch/awk-drops"
printf '#!/bin/sh\nexit 1\n' > "$scratch/find-fails/find"
# shellcheck disable=SC2016  # the $ belong to the stub script, expanded when it runs
printf '#!/usr/bin/env bash\nif [ "$1" = -f ] && [[ "$2" == */heredocs.awk ]]; then set -- "${@:1:$#-1}"; fi\nexec %q "$@"\n' \
    "$real_awk" > "$scratch/awk-drops/awk"
chmod +x "$scratch/find-fails/find" "$scratch/awk-drops/awk"
PATH="$scratch/find-fails:$PATH" probe "$(fixture_tree deadline-helper)" fail "a find that fails fails the walk" \
    '^scan_sleeps: find failed under'
PATH="$scratch/awk-drops:$PATH" probe "$(fixture_tree deadline-helper)" fail "a script find listed and awk never read fails the walk" \
    '^scan_sleeps: awk read 1 of the 2 non-empty scripts'

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
