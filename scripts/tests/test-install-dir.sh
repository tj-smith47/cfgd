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

# A dry run into a directory that does not exist yet says it would create it.
# resolve_asset is stubbed: the dry run prints the asset URLs and fetches none.
printf '%s\n%s\n%s\n' "$(function_body info)" "$(function_body download_and_install)" \
    'resolve_asset() { ARCHIVE=cfgd.tar.gz; }' > "$scratch/dry-install.sh"
dry_out="$(DRY_RUN=true REPO=o/r VERSION=v0 sh -c '. "$1"; download_and_install linux x86_64 "$2"' \
    sh "$scratch/dry-install.sh" "$scratch/dry/bin")" || fail "a dry run's plan: download_and_install failed"
case "$dry_out" in
    *"Would create $scratch/dry/bin"*) ;;
    *) fail "a dry run's plan does not say it would create $scratch/dry/bin: $dry_out" ;;
esac
case "$dry_out" in
    *"Would require sudo"*) fail "a dry run's plan claims sudo for a directory that does not exist yet: $dry_out" ;;
esac

# A missing directory whose nearest existing parent the user cannot write is
# created through sudo, and the copy into it needs sudo too; the plan says both.
# Root ignores mode bits (the FreeBSD guest runs this as root), so the case only
# means something for an ordinary user.
if [ "$(id -u)" -ne 0 ]; then
    locked="$scratch/locked"
    mkdir -p "$locked"
    chmod 555 "$locked"
    dry_out="$(DRY_RUN=true REPO=o/r VERSION=v0 sh -c '. "$1"; download_and_install linux x86_64 "$2"' \
        sh "$scratch/dry-install.sh" "$locked/bin")" || fail "a dry run's sudo plan: download_and_install failed"
    chmod 755 "$locked"
    case "$dry_out" in
        *"Would create $locked/bin (requires sudo)"*"Would require sudo for $locked/bin"*) ;;
        *) fail "a dry run's plan does not name sudo for creating and filling $locked/bin: $dry_out" ;;
    esac
else
    printf 'skip: the sudo plan case needs a non-root user (root ignores mode bits)\n'
fi

if [ "$failures" -ne 0 ]; then
    printf '%d install directory check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'install directory checks passed\n'
