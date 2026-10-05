#!/usr/bin/env bash
# Checks the counter readers in helpers.sh without a cluster: a `<family>_total`
# sample is read, a `<family>_total_total` sample, a longer name or a comment
# line is not, an absent sample reads as 0, and no e2e script matches a counter
# sample by hand. Checks has_all_words on a space-joined list too, and that no
# e2e script chains two quoted words in one case pattern (`*" a "*" b "*`).
#
# Usage: tests/e2e/common/test-metrics.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/census.sh
source "$here/census.sh"
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
printf 'cfgd_x_tabs_total\t13\n' >> "$body"

want="$(printf '%s\n' 'cfgd_x_hits_total{module="a"} 3' 'cfgd_x_hits_total 11')"
got="$(metric_sample_lines cfgd_x_hits "$body")"
if [ "$got" = "$want" ]; then
    pass "metric_sample_lines reads _total and skips _total_total, _totalx, _created and comment lines"
else
    fail "metric_sample_lines: got '$got', want '$want'"
fi

if [ "$(metric_sample_lines cfgd_x_tabs "$body")" = $'cfgd_x_tabs_total\t13' ]; then
    pass "metric_sample_lines reads a sample whose value follows a tab"
else
    fail "metric_sample_lines did not read the tab-separated cfgd_x_tabs_total sample"
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
check_value 'module="b"' 0
check_value 'module="c"' 0
check_value 'module="absent"' 0
check_value '' 11
got="$(metric_sample_value cfgd_x_tabs '' "$body")"
if [ "$got" = 13 ]; then
    pass "metric_sample_value reads 13 from the tab-separated cfgd_x_tabs_total sample"
else
    fail "metric_sample_value cfgd_x_tabs: got '$got', want 13"
fi

if got="$(metric_sample_value cfgd_x_misses '' "$body")" && [ "$got" = 0 ]; then
    pass "metric_sample_value reads 0 and exits 0 for a family with no sample"
else
    fail "metric_sample_value for a family with no sample: got '$got'"
fi

# There are about twice this many scripts under tests/e2e/, so a count below
# it means the scan lost its files.
min_scanned_files=40

# scan_lines <scan> <ere> <skip> <path>...: print file:line for each line
# matching <ere> in a *.sh under <path>, skipping files named in the
# space-separated <skip> list, comments and begin_test titles, then a last line
# `scanned <files>`. Exits 1 when find fails or matched no file, or when a
# non-empty file find listed never reached awk (each one is named, under
# <scan>). The pattern reaches awk through the environment, so its backslashes
# are read as written.
scan_lines() {
    local scan="$1" ere="$2" files="$scratch/scan-files" read="$scratch/scan-read"
    local -a skip=()
    local name
    for name in $3; do
        skip+=(! -name "$name")
    done
    shift 3
    if ! find "$@" -name '*.sh' "${skip[@]}" > "$files"; then
        echo "$scan: find failed under $*" >&2
        return 1
    fi
    if [ ! -s "$files" ]; then
        echo "$scan: no .sh file under $*" >&2
        return 1
    fi
    : > "$read"
    # shellcheck disable=SC2016  # an awk program; the $0 field belongs to awk
    tr '\n' '\0' < "$files" | SCAN_LINES_ERE="$ere" xargs -0 awk -v readlog="$read" '
        FNR == 1 { files++; print FILENAME > readlog }
        /^[[:space:]]*#/ || /^[[:space:]]*begin_test[[:space:]]/ { next }
        $0 ~ ENVIRON["SCAN_LINES_ERE"] { print FILENAME ":" FNR }
        END { print "scanned " files + 0 }
    '
    census_unread "$scan" "$files" "$read"
}

# A counter sample is `cfgd_<name>_total` as a whole word, or any `_total{` /
# `_total(\{` match. helpers.sh is where the one matching spelling lives, and
# this file holds the probes.
scan_hand_matches() {
    scan_lines scan_hand_matches 'cfgd_[a-z_]+_total([^a-z_]|$)|_total(\(\\\{|\\?\{)' \
        "helpers.sh test-metrics.sh" "$@"
}

# A case pattern chaining two quoted words (`*" a "*" b "*`) cannot match a
# space-joined list whose members share one space; has_all_words reads it.
# helpers.sh, where list predicates live, is read too; this file is skipped for
# its probe heredoc.
scan_chained_cases() {
    scan_lines scan_chained_cases ' "\*" ' test-metrics.sh "$@"
}

# Every counter read goes through the helpers, so the one sample spelling read
# is stated in one place.
report="$(scan_hand_matches "$e2e_root")" || fail "the hand-match scan lost the scripts named above"
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

# find and awk are stubbed through PATH, so each failure is the one named.
mkdir -p "$scratch/census" "$scratch/find-fails" "$scratch/awk-drops"
printf 'x\n' > "$scratch/census/only.sh"
printf '#!/bin/sh\nexit 1\n' > "$scratch/find-fails/find"
# shellcheck disable=SC2016  # the $ belong to the stub script, expanded when it runs
printf '#!/usr/bin/env bash\nset -- "${@:1:$#-1}"\nexec %q "$@"\n' "$(command -v awk)" > "$scratch/awk-drops/awk"
chmod +x "$scratch/find-fails/find" "$scratch/awk-drops/awk"
census="$(PATH="$scratch/find-fails:$PATH" scan_hand_matches "$scratch/census" 2>&1)" && census_rc=0 || census_rc=$?
if [ "$census_rc" -ne 0 ] && grep -q '^scan_hand_matches: find failed under' <<<"$census"; then
    pass "a hand-match scan whose find fails fails"
else
    fail "a hand-match scan whose find fails exited $census_rc: $census"
fi
census="$(PATH="$scratch/awk-drops:$PATH" scan_hand_matches "$scratch/census" 2>&1 < /dev/null)" && census_rc=0 || census_rc=$?
if [ "$census_rc" -ne 0 ] && grep -q '^scan_hand_matches: .*/only\.sh was listed and never read$' <<<"$census"; then
    pass "a hand-match scan names a script find listed and awk never read"
else
    fail "a hand-match scan that lost a script exited $census_rc: $census"
fi

mkdir -p "$scratch/dangling"
printf 'x\n' > "$scratch/dangling/a.sh"
ln -s "$scratch/nowhere.sh" "$scratch/dangling/b.sh"
census="$(scan_hand_matches "$scratch/dangling" 2>&1)" && census_rc=0 || census_rc=$?
if [ "$census_rc" -ne 0 ] && grep -q '^scan_hand_matches: .*/b\.sh was listed and never read$' <<<"$census"; then
    pass "a hand-match scan names a script awk cannot open"
else
    fail "a hand-match scan over a dangling script exited $census_rc: $census"
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

# The CI string OP-MC-01 and FS-FLEET-03 read: one space after each member.
conditions='Reconciled=True/ReconcileSuccess DriftDetected=False/NoDrift ModulesResolved=True/AllResolved Compliant=Unknown/NotEvaluated '
words=(Reconciled=True/ReconcileSuccess DriftDetected=False/NoDrift ModulesResolved=True/AllResolved)
check_words() {
    local want="$1" what="$2" list="$3" rc=0
    shift 3
    has_all_words "$list" "$@" || rc=1
    if [ "$rc" -eq "$want" ]; then
        pass "has_all_words $what"
    else
        fail "has_all_words $what: exited $rc on '$list'"
    fi
}
check_words 0 "matches every member of the settled conditions in any order, other members allowed" "$conditions" "${words[@]}"
check_words 0 "matches members named in a different order" "$conditions" "${words[2]}" "${words[0]}"
check_words 1 "fails when one word is missing" "${conditions/DriftDetected=False\/NoDrift /}" "${words[@]}"
check_words 1 "fails when a word is only a prefix of a member" "$conditions" Reconciled=True
check_words 1 "fails on the empty list" "" "${words[@]}"
check_words 0 "matches the two platforms of an index" "linux/arm64 linux/amd64" linux/amd64 linux/arm64

# Every space-joined list is read through has_all_words, so no case can chain
# two quoted words that share one separator.
report="$(scan_chained_cases "$e2e_root")" || fail "the chained-case scan lost the scripts named above"
strays="$(grep -v '^scanned ' <<<"$report" || true)"
scanned_files="$(sed -n 's/^scanned //p' <<<"$report")"
if [ "${scanned_files:-0}" -lt "$min_scanned_files" ]; then
    fail "the chained-case scan read ${scanned_files:-0} files, fewer than $min_scanned_files"
elif [ -z "$strays" ]; then
    pass "no e2e script chains two quoted words in one case pattern ($scanned_files files)"
else
    fail "case patterns chaining quoted words outside has_all_words:"
    printf '%s\n' "$strays" | sed 's/^/    /'
fi

mkdir -p "$scratch/chained"
cat > "$scratch/chained/p.sh" <<'PROBE'
case " $S" in *" A=1 "*" B=2 "*) ok ;; esac
    *" $a "*" $b "*) return 0 ;;
# *" A "*" B "* in a comment
case " $S " in *" A=1 "*) ok ;; esac
has_all_words "$S" A=1 B=2
PROBE
cat > "$scratch/chained/helpers.sh" <<'PROBE'
zz() { case " $1 " in *" a "*" b "*) return 0 ;; esac; }
PROBE
want="$(printf '%s\n' "$scratch/chained/helpers.sh:1" "$scratch/chained/p.sh:1" "$scratch/chained/p.sh:2" "scanned 2")"
got="$(scan_chained_cases "$scratch/chained" | LC_ALL=C sort)"
if [ "$got" = "$want" ]; then
    pass "the chained-case scan reads helpers.sh, flags literal and variable chains, and skips comments, a one-word case and has_all_words"
else
    fail "the chained-case scan on the probe printed:"
    printf '%s\n' "$got" | sed 's/^/    /'
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
