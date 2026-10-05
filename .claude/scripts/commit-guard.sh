#!/usr/bin/env bash
# Refuse a commit that carries a breaking-release signal while any crate it
# touches is still below 1.0.0.
#
# anodizer reads three signals out of a commit and mints a MAJOR bump from any
# of them (see `.anodizer.yaml`): a `<type>!:` / `<type>(scope)!:` subject
# marker, a `BREAKING CHANGE:` / `BREAKING-CHANGE:` footer, and a `#major`
# token. On a 0.x crate that bump is 1.0.0, which is a release decision only
# the user makes. `BREAKING_CHANGE_APPROVED=1` is the user's override.
#
# It also refuses a message whose wording the changelog cannot carry: a
# subject over 100 characters or without a conventional `type(scope): ` prefix,
# or any non-trailer line matching `commit-wording.txt` beside this script
# (contrast frames, prose dashes, deferral excuses, review and session
# narrative). Those have no override: the message is reworded.
#
# Usage: commit-guard.sh [--dry-run] <the exact `git commit` argv>
#        commit-guard.sh --self-test
set -euo pipefail

GUARD_ROOT="${GUARD_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"
WORDING_LIST="${WORDING_LIST:-$(cd "$(dirname "$0")" && pwd)/commit-wording.txt}"
SUBJECT_MAX=100

# The first `version = "..."` under `[package]`, empty when the file names none.
crate_version() {
    [ -f "$1" ] || return 0
    awk '
        /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*$/) }
        in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
            if (match($0, /"[^"]*"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                exit
            }
        }
    ' "$1"
}

below_one_zero() {
    case "$1" in
        0.* | 0) return 0 ;;
        *) return 1 ;;
    esac
}

# Name the breaking signal the message carries, or print nothing.
#
# A here-string feeds grep from a temp fd rather than a pipe, so an early-exit
# consumer cannot SIGPIPE its writer under `pipefail`.
breaking_signal() {
    local msg="$1" subject bang_marker
    subject="${msg%%$'\n'*}"
    bang_marker='^[a-zA-Z]+(\([^)]*\))?!:'
    if [[ $subject =~ $bang_marker ]]; then
        # shellcheck disable=SC2016  # literal backticks in a message, nothing to expand
        printf '%s' 'a `!` marker in the subject'
        return 0
    fi
    if grep -Eq '^(BREAKING CHANGE|BREAKING-CHANGE):' <<<"$msg"; then
        # shellcheck disable=SC2016  # literal backticks in a message, nothing to expand
        printf '%s' 'a `BREAKING CHANGE:` footer'
        return 0
    fi
    if grep -Eq '(^|[[:space:]])#major([[:space:]]|$)' <<<"$msg"; then
        # shellcheck disable=SC2016  # literal backticks in a message, nothing to expand
        printf '%s' 'a `#major` token'
        return 0
    fi
    return 0
}

# Each staged path's crate and that crate's version, one `<name> <version>` per
# line, deduplicated, for the crates below 1.0.0.
touched_crates_below_one() {
    local staged="$1" path crate version seen=" "
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        case "$path" in
            crates/*/*) ;;
            *) continue ;;
        esac
        crate="${path#crates/}"
        crate="${crate%%/*}"
        case "$seen" in
            *" $crate "*) continue ;;
        esac
        seen="$seen$crate "
        version="$(crate_version "$GUARD_ROOT/crates/$crate/Cargo.toml")"
        [ -n "$version" ] || continue
        if below_one_zero "$version"; then
            printf '%s %s\n' "$crate" "$version"
        fi
    done <<<"$staged"
}

# The refusal text, or nothing when the commit may proceed.
refusal_for() {
    local msg="$1" staged="$2" signal below crate version
    signal="$(breaking_signal "$msg")"
    [ -n "$signal" ] || return 0
    below="$(touched_crates_below_one "$staged")"
    [ -n "$below" ] || return 0
    printf 'commit-guard: this message carries %s, and anodizer would mint 1.0.0 for:\n' "$signal"
    while IFS=' ' read -r crate version; do
        [ -n "$crate" ] && printf '  %s is at %s\n' "$crate" "$version"
    done <<<"$below"
    # shellcheck disable=SC2016  # literal backticks in a message, nothing to expand
    printf 'A 1.0.0 release is the user'"'"'s call. Reword the subject (a `#minor` token\n'
    printf 'carries the bump anodizer should take instead), or have the user set\n'
    printf 'BREAKING_CHANGE_APPROVED=1 for this commit.\n'
}

# The wording refusal, or nothing when every line reads as the changelog and
# its readers need. Trailer lines (`Key: value`) carry names and are skipped.
wording_refusal() {
    local msg="$1" subject prefix line pat n=0 out=""
    subject="${msg%%$'\n'*}"
    prefix='^[a-z]+(\([^)]*\))?!?: [^ ]'
    if [ "${#subject}" -gt "$SUBJECT_MAX" ]; then
        out="$out  the subject is ${#subject} characters; the limit is $SUBJECT_MAX"$'\n'
    fi
    if ! [[ $subject =~ $prefix ]]; then
        out="$out  the subject does not open with a conventional \`type(scope): \` prefix"$'\n'
    fi
    if [ ! -r "$WORDING_LIST" ]; then
        printf 'commit-guard: cannot read the wording list at %s\n' "$WORDING_LIST" >&2
        return 2
    fi
    while IFS= read -r pat || [ -n "$pat" ]; do
        case "$pat" in '' | '#'*) continue ;; esac
        while IFS= read -r line; do
            n=$((n + 1))
            [[ $line =~ ^[A-Za-z-]+:\  ]] && continue
            if grep -Pqi -- "$pat" <<<"$line"; then
                out="$out  line $n matches \`$pat\`: $line"$'\n'
            fi
        done <<<"$msg"
        n=0
    done <"$WORDING_LIST"
    [ -n "$out" ] || return 0
    printf 'commit-guard: this message needs rewording before it can be a changelog line:\n%s' "$out"
    printf 'The list is %s; say the fact once, in plain English.\n' "$WORDING_LIST"
}

# --- Message extraction -----------------------------------------------------

# Join every `-m` occurrence with a blank line, as git does; `-F` / `--file`
# supplies the whole message on its own.
extract_message() {
    local -a argv=("$@")
    local i=0 arg next message="" found=0 file=""
    while [ "$i" -lt "${#argv[@]}" ]; do
        arg="${argv[$i]}"
        next=""
        [ $((i + 1)) -lt "${#argv[@]}" ] && next="${argv[$((i + 1))]}"
        case "$arg" in
            -m | --message)
                [ "$found" -eq 1 ] && message="$message"$'\n\n'
                message="$message$next"
                found=1
                i=$((i + 2))
                continue
                ;;
            -m?*)
                [ "$found" -eq 1 ] && message="$message"$'\n\n'
                message="$message${arg#-m}"
                found=1
                ;;
            --message=*)
                [ "$found" -eq 1 ] && message="$message"$'\n\n'
                message="$message${arg#--message=}"
                found=1
                ;;
            -F | --file)
                file="$next"
                i=$((i + 2))
                continue
                ;;
            -F?*) file="${arg#-F}" ;;
            --file=*) file="${arg#--file=}" ;;
        esac
        i=$((i + 1))
    done
    if [ "$found" -eq 1 ]; then
        printf '%s' "$message"
        return 0
    fi
    if [ -n "$file" ]; then
        if [ ! -f "$file" ]; then
            printf 'commit-guard: no message file at %s\n' "$file" >&2
            return 2
        fi
        cat "$file"
        return 0
    fi
    return 1
}

# --- Self-test --------------------------------------------------------------

self_test() {
    local fixture failures=0
    fixture="$(mktemp -d)"
    # Expanded now: the trap fires after this function's locals are gone.
    # shellcheck disable=SC2064  # expanded now on purpose: the local is gone when the trap fires
    trap "rm -rf '$fixture'" EXIT
    mkdir -p "$fixture/crates/below" "$fixture/crates/stable"
    printf '[package]\nname = "below"\nversion = "0.9.0"\n' >"$fixture/crates/below/Cargo.toml"
    printf '[package]\nname = "stable"\nversion = "1.2.0"\n' >"$fixture/crates/stable/Cargo.toml"
    GUARD_ROOT="$fixture"

    local under_crate="crates/below/src/lib.rs"
    local under_stable="crates/stable/src/lib.rs"
    local under_docs="docs/cli-reference.md"

    walk() { # name, message, staged paths, expected verdict (refuse|allow)
        local got
        got="$(refusal_for "$2" "$3")"
        if [ "$4" = "refuse" ] && [ -z "$got" ]; then
            printf 'MISS: %s should have been refused\n' "$1"
            failures=$((failures + 1))
        elif [ "$4" = "allow" ] && [ -n "$got" ]; then
            printf 'MISS: %s should have been allowed, got:\n%s\n' "$1" "$got"
            failures=$((failures + 1))
        else
            printf 'ok: %s (%s)\n' "$1" "$4"
        fi
    }

    walk 'feat!: x on a 0.x crate' 'feat!: x' "$under_crate" refuse
    walk 'fix(cli)!: x on a 0.x crate' 'fix(cli)!: x' "$under_crate" refuse
    walk 'feat: x on a 0.x crate' 'feat: x' "$under_crate" allow
    walk 'a BREAKING CHANGE footer' $'feat: x\n\nBREAKING CHANGE: the flag is gone' "$under_crate" refuse
    walk 'a #major token' 'feat: x #major' "$under_crate" refuse
    walk 'feat!: x touching no crate' 'feat!: x' "$under_docs" allow
    walk 'feat!: x on a 1.x crate' 'feat!: x' "$under_stable" allow
    walk 'a #minor token' 'fix(core): x #minor' "$under_crate" allow

    WORDING_LIST="$fixture/wording.txt"
    cp "$(dirname "${BASH_SOURCE[0]}")/commit-wording.txt" "$WORDING_LIST"
    word() { # name, message, expected verdict (refuse|allow)
        local got
        got="$(wording_refusal "$2")"
        if [ "$3" = "refuse" ] && [ -z "$got" ]; then
            printf 'MISS: %s should have been refused\n' "$1"
            failures=$((failures + 1))
        elif [ "$3" = "allow" ] && [ -n "$got" ]; then
            printf 'MISS: %s should have been allowed, got:\n%s\n' "$1" "$got"
            failures=$((failures + 1))
        else
            printf 'ok: %s (%s)\n' "$1" "$3"
        fi
    }
    word 'a plain subject and body' $'fix(cli): report the failing question\n\nThe refusal names the file.\n\nCo-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>' allow
    word 'a subject over the limit' "feat(cli): $(printf 'x%.0s' $(seq 1 100))" refuse
    word 'a subject without a prefix' 'Report the failing question' refuse
    word 'a contrast frame in the body' $'fix(cli): x\n\nIt reads the file rather than the env.' refuse
    word 'a comma-not frame' $'fix(cli): x\n\nThe flag, not the env, wins.' refuse
    word 'a comma-never frame before a digit' $'fix(cli): x\n\nReturns 2, never 1, when pgrep cannot answer.' refuse
    word 'an instead-of frame' $'fix(cli): x\n\nIt reads the file instead of the env.' refuse
    word 'a but-not frame' $'fix(cli): x\n\nThe flag is read but not the env.' refuse
    word 'a not-X-but-Y frame' $'fix(cli): x\n\nIt reads not the file but the env.' refuse
    word 'a not with no but' $'fix(cli): x\n\nThe file is not read twice.' allow
    word 'a prose em dash' $'fix(cli): x\n\nThe flag — when set — wins.' refuse
    word 'a prose double dash' $'fix(cli): x\n\nThe flag -- when set -- wins.' refuse
    word 'a deferral excuse' $'fix(cli): x\n\nThe rest is a follow-up.' refuse
    word 'a review tag' $'fix(cli): x\n\nThe B1 fix moves the read.' refuse
    word 'session narrative' $'fix(cli): x\n\nTask 7 left this open.' refuse
    word 'a first-person we' $'fix(cli): x\n\nWe read the file once.' refuse
    word 'a Claude mention outside a trailer' $'fix(cli): x\n\nClaude wrote the first draft.' refuse
    word 'a .claude path' $'docs(rules): x\n\nThe rule in .claude/rules/testing.md names the helper.' allow
    word 'an I/O mention' $'fix(core): x\n\nThe I/O error is typed.' allow
    word 'a round-trip mention' $'test(schema): x\n\nThe round-trip test samples one variant.' allow
    word 'a legacy flat key' $'test(cli): x\n\nThe fixture wrote the legacy flat key.' allow
    word 'a module-id token' $'fix(cli): x\n\nFS-CSI-01 and OP-PR-01 read the pod once.' allow

    if [ "$failures" -gt 0 ]; then
        printf 'commit-guard self-test: %d miss(es)\n' "$failures"
        return 1
    fi
    printf 'commit-guard self-test: all cases pass\n'
}

# --- Entry point ------------------------------------------------------------

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit $?
fi

DRY_RUN=0
if [ "${1:-}" = "--dry-run" ]; then
    DRY_RUN=1
    shift
fi

MESSAGE=""
if ! MESSAGE="$(extract_message "$@")"; then
    status=$?
    [ "$status" -eq 2 ] && exit 1
    printf 'commit-guard: pass the message with -m or -F so it can be checked\n' >&2
    exit 1
fi

WORDING="$(wording_refusal "$MESSAGE")" || exit 1
if [ -n "$WORDING" ]; then
    printf '%s\n' "$WORDING" >&2
    exit 1
fi

STAGED="$(git diff --cached --name-only)"
REFUSAL="$(refusal_for "$MESSAGE" "$STAGED")"

if [ -n "$REFUSAL" ] && [ "${BREAKING_CHANGE_APPROVED:-}" != "1" ]; then
    printf '%s\n' "$REFUSAL" >&2
    exit 1
fi

if [ -n "$REFUSAL" ]; then
    printf 'commit-guard: BREAKING_CHANGE_APPROVED=1 is set, allowing the breaking signal.\n' >&2
fi

if [ "$DRY_RUN" -eq 1 ]; then
    printf 'commit-guard: no breaking signal or wording blocks this commit.\n'
    exit 0
fi

exec git commit "$@"
