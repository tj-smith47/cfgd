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
# Usage: commit-guard.sh [--dry-run] <the exact `git commit` argv>
#        commit-guard.sh --self-test
set -euo pipefail

GUARD_ROOT="${GUARD_ROOT:-$(cd "$(dirname "$0")/../.." && pwd)}"

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
        printf '%s' 'a `!` marker in the subject'
        return 0
    fi
    if grep -Eq '^(BREAKING CHANGE|BREAKING-CHANGE):' <<<"$msg"; then
        printf '%s' 'a `BREAKING CHANGE:` footer'
        return 0
    fi
    if grep -Eq '(^|[[:space:]])#major([[:space:]]|$)' <<<"$msg"; then
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
    printf 'A 1.0.0 release is the user'"'"'s call. Reword the subject (a `#minor` token\n'
    printf 'carries the bump anodizer should take instead), or have the user set\n'
    printf 'BREAKING_CHANGE_APPROVED=1 for this commit.\n'
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

STAGED="$(git diff --cached --name-only)"
REFUSAL="$(refusal_for "$MESSAGE" "$STAGED")"

if [ -n "$REFUSAL" ] && [ "${BREAKING_CHANGE_APPROVED:-}" != "1" ]; then
    printf '%s' "$REFUSAL" >&2
    exit 1
fi

if [ -n "$REFUSAL" ]; then
    printf 'commit-guard: BREAKING_CHANGE_APPROVED=1 is set, allowing the breaking signal.\n' >&2
fi

if [ "$DRY_RUN" -eq 1 ]; then
    printf 'commit-guard: no breaking signal blocks this commit.\n'
    exit 0
fi

exec git commit "$@"
