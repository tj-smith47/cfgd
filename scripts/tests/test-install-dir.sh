#!/bin/sh
# Checks the installer's install-directory choice without downloading anything:
# resolve_install_dir is lifted out of scripts/install.sh.tpl and run against
# scratch directories, one case per way the directory can be chosen.
#
# Usage: scripts/tests/test-install-dir.sh
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
template="$here/../install.sh.tpl"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

failures=0
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

# Print one top-level shell function of the template, from its opening line
# through the first line that is a lone closing brace.
function_body() {
    awk -v open="$1() {" '$0 == open { on = 1 } on { print } on && $0 == "}" { exit }' "$template"
}

# The template names the system directory literally. Each case swaps it for a
# scratch path so the writable and the unwritable arm can both be reached
# without touching the host's /usr/local/bin.
system_dir_literal="/usr/local/bin"
body="$(function_body error)
$(function_body resolve_install_dir)"
case "$body" in
    *"resolve_install_dir() {"*"$system_dir_literal"*) ;;
    *)
        printf 'FAIL: %s no longer defines resolve_install_dir around %s\n' "$template" "$system_dir_literal" >&2
        exit 1
        ;;
esac

# Run resolve_install_dir in a child shell and print what it echoed. $1 is the
# CFGD_INSTALL_DIR value, $2 the scratch system directory, $3 DRY_RUN, and $4
# a directory put first on PATH (for a sudo stand-in), or empty.
resolve() {
    printf '%s\n' "$body" | sed "s#$system_dir_literal#$2#g" > "$scratch/fn.sh"
    INSTALL_DIR="$1" DRY_RUN="$3" HOME="$scratch/home" PATH="${4:+$4:}$PATH" \
        sh -c '. "$1"; resolve_install_dir' sh "$scratch/fn.sh"
}

missing_system="$scratch/no-system-bin"
writable_system="$scratch/system-bin"
mkdir -p "$writable_system" "$scratch/home"

# An override naming a directory that does not exist yet is created and used.
override="$scratch/new/nested/bin"
got="$(resolve "$override" "$missing_system" false "")" || fail "a new override directory: resolve_install_dir failed"
[ "$got" = "$override" ] || fail "a new override directory: echoed '$got', want '$override'"
[ -d "$override" ] || fail "a new override directory: $override was not created"

# An override naming an existing directory is used as it is.
existing="$scratch/existing"
mkdir -p "$existing"
printf 'keep\n' > "$existing/marker"
chmod 750 "$existing"
before="$(ls -ld "$existing" | awk '{print $1}')"
got="$(resolve "$existing" "$missing_system" false "")" || fail "an existing override directory: resolve_install_dir failed"
[ "$got" = "$existing" ] || fail "an existing override directory: echoed '$got', want '$existing'"
after="$(ls -ld "$existing" | awk '{print $1}')"
[ "$before" = "$after" ] || fail "an existing override directory: mode changed from $before to $after"
[ "$(cat "$existing/marker")" = keep ] || fail "an existing override directory: its contents changed"

# No override and no writable system directory: ~/.local/bin is created and used.
got="$(resolve "" "$missing_system" false "")" || fail "the home fallback: resolve_install_dir failed"
[ "$got" = "$scratch/home/.local/bin" ] || fail "the home fallback: echoed '$got', want '$scratch/home/.local/bin'"
[ -d "$scratch/home/.local/bin" ] || fail "the home fallback: $scratch/home/.local/bin was not created"

# No override and a writable system directory: the system directory is used.
got="$(resolve "" "$writable_system" false "")" || fail "a writable system directory: resolve_install_dir failed"
[ "$got" = "$writable_system" ] || fail "a writable system directory: echoed '$got', want '$writable_system'"

# A dry run creates nothing.
dry="$scratch/dry/bin"
got="$(resolve "$dry" "$missing_system" true "")" || fail "a dry run: resolve_install_dir failed"
[ "$got" = "$dry" ] || fail "a dry run: echoed '$got', want '$dry'"
[ ! -e "$scratch/dry" ] || fail "a dry run: $scratch/dry was created"

# A directory mkdir cannot create (its parent is a file) goes through sudo.
# The stand-in records its arguments and succeeds or fails on request.
shim="$scratch/shim"
mkdir -p "$shim"
cat > "$shim/sudo" <<'SHIM'
#!/bin/sh
printf '%s\n' "$*" >> "$SUDO_LOG"
exit "${SUDO_EXIT:-0}"
SHIM
chmod +x "$shim/sudo"
printf 'not a directory\n' > "$scratch/blocker"
blocked="$scratch/blocker/bin"
export SUDO_LOG="$scratch/sudo.log"
got="$(SUDO_EXIT=0 resolve "$blocked" "$missing_system" false "$shim")" || fail "a directory needing sudo: resolve_install_dir failed"
[ "$got" = "$blocked" ] || fail "a directory needing sudo: echoed '$got', want '$blocked'"
[ "$(cat "$SUDO_LOG" 2>/dev/null)" = "mkdir -p $blocked" ] || fail "a directory needing sudo: sudo ran '$(cat "$SUDO_LOG" 2>/dev/null)', want 'mkdir -p $blocked'"

# When sudo cannot create it either, the installer stops with an error.
rm -f "$SUDO_LOG"
if got="$(SUDO_EXIT=1 resolve "$blocked" "$missing_system" false "$shim" 2> "$scratch/stderr")"; then
    fail "a directory nothing can create: resolve_install_dir succeeded, echoing '$got'"
fi
grep -q "Cannot create install directory $blocked" "$scratch/stderr" \
    || fail "a directory nothing can create: stderr does not name it: $(cat "$scratch/stderr")"

if [ "$failures" -ne 0 ]; then
    printf '%d install directory check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'install directory checks passed\n'
