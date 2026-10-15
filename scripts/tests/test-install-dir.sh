#!/bin/sh
# Checks the installer's install-directory choice and its `latest` version
# lookup without downloading anything: resolve_install_dir and resolve_version
# are lifted out of scripts/install.sh.tpl and run against scratch directories
# and fixture release lists, one case per way each can resolve.
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
# a directory put first on PATH (for a sudo stand-in), or empty. SUDO_EXIT is
# forwarded by name because a caller sets it as a prefix on this function, and
# FreeBSD's sh keeps such an assignment out of the environment of the
# function's children, so the stand-in would never see it.
resolve() {
    printf '%s\n' "$body" | sed "s#$system_dir_literal#$2#g" > "$scratch/fn.sh"
    INSTALL_DIR="$1" DRY_RUN="$3" HOME="$scratch/home" PATH="${4:+$4:}$PATH" SUDO_EXIT="${SUDO_EXIT-}" \
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
# shellcheck disable=SC2012  # one path; ls -ld mode string reads the same on GNU and BSD
before="$(ls -ld "$existing" | awk '{print $1}')"
got="$(resolve "$existing" "$missing_system" false "")" || fail "an existing override directory: resolve_install_dir failed"
[ "$got" = "$existing" ] || fail "an existing override directory: echoed '$got', want '$existing'"
# shellcheck disable=SC2012  # one path; ls -ld mode string reads the same on GNU and BSD
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
printf '%s\n%s\n%s\n%s\n' "$(function_body info)" "$(function_body error)" "$(function_body download_and_install)" \
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

# A file standing where a parent directory should be fails the dry run with the
# refusal the real run gives above, so the plan never promises a directory the
# install cannot create.
if dry_out="$(DRY_RUN=true REPO=o/r VERSION=v0 sh -c '. "$1"; download_and_install linux x86_64 "$2"' \
    sh "$scratch/dry-install.sh" "$blocked" 2> "$scratch/dry-stderr")"; then
    fail "a dry run under a file: download_and_install succeeded: $dry_out"
fi
grep -q "Cannot create install directory $blocked" "$scratch/dry-stderr" \
    || fail "a dry run under a file: stderr does not name it: $(cat "$scratch/dry-stderr")"

# A dangling symlink where a parent directory should be: the install's mkdir
# cannot create through it, and the dry run refuses with the same words.
# Git Bash's ln copies its target unless MSYS asks for a native link, so a
# target that does not exist fails outright there; ln anywhere else ignores MSYS.
MSYS=winsymlinks:nativestrict ln -s "$scratch/nowhere" "$scratch/dangling"
dangling="$scratch/dangling/bin"
if got="$(SUDO_EXIT=1 resolve "$dangling" "$missing_system" false "$shim" 2> "$scratch/stderr")"; then
    fail "a directory under a dangling link: resolve_install_dir succeeded, echoing '$got'"
fi
grep -q "Cannot create install directory $dangling" "$scratch/stderr" \
    || fail "a directory under a dangling link: stderr does not name it: $(cat "$scratch/stderr")"
if dry_out="$(DRY_RUN=true REPO=o/r VERSION=v0 sh -c '. "$1"; download_and_install linux x86_64 "$2"' \
    sh "$scratch/dry-install.sh" "$dangling" 2> "$scratch/dry-stderr")"; then
    fail "a dry run under a dangling link: download_and_install succeeded: $dry_out"
fi
grep -q "Cannot create install directory $dangling" "$scratch/dry-stderr" \
    || fail "a dry run under a dangling link: stderr does not name it: $(cat "$scratch/dry-stderr")"

# A missing directory whose nearest existing parent the user cannot write is
# created through sudo, and the copy into it needs sudo too; the plan says both.
# The case only means something where mode bits bind this user: root ignores
# them (the FreeBSD guest runs this as root), and so does Windows, whose ACLs
# leave a 555 directory writable under Git Bash. A probe write decides, since
# `id -u` answers only for root.
locked="$scratch/locked"
mkdir -p "$locked"
chmod 555 "$locked"
if ! touch "$locked/probe" 2>/dev/null; then
    dry_out="$(DRY_RUN=true REPO=o/r VERSION=v0 sh -c '. "$1"; download_and_install linux x86_64 "$2"' \
        sh "$scratch/dry-install.sh" "$locked/bin")" || fail "a dry run's sudo plan: download_and_install failed"
    chmod 755 "$locked"
    case "$dry_out" in
        *"Would create $locked/bin (requires sudo)"*"Would require sudo for $locked/bin"*) ;;
        *) fail "a dry run's plan does not name sudo for creating and filling $locked/bin: $dry_out" ;;
    esac
else
    rm -f "$locked/probe"
    chmod 755 "$locked"
    printf 'skip: the sudo plan case needs a user a 555 directory refuses (root and Windows write through it)\n'
fi

# CFGD_VERSION=latest reads the releases list, whose newest entry can belong to
# another crate in the repository. fetch_stdout is stubbed to print a fixture
# list, in GitHub's own pretty-printed and in a minified layout.
printf '%s\n%s\n' "$(function_body error)" "$(function_body resolve_version)" > "$scratch/resolve-version.sh"
resolve_latest() {
    sh -c '. "$1"; fixture="$2"; fetch_stdout() { cat "$fixture"; }; resolve_version; printf "%s\n" "$VERSION"' \
        sh "$scratch/resolve-version.sh" "$1"
}
cat > "$scratch/releases.json" <<'JSON'
[
  {
    "tag_name": "operator-v0.11.0",
    "prerelease": false,
    "assets": []
  },
  {
    "tag_name": "v0.13.0-rc.1",
    "prerelease": true,
    "assets": []
  },
  {
    "tag_name": "v0.12.0",
    "prerelease": false,
    "assets": [{"name": "cfgd-0.12.0-linux-amd64.tar.gz"}]
  },
  {
    "tag_name": "csi-v0.8.0",
    "prerelease": false,
    "assets": []
  },
  {
    "tag_name": "v0.11.0",
    "prerelease": false,
    "assets": []
  }
]
JSON
got="$(VERSION=latest REPO=o/r resolve_latest "$scratch/releases.json")" || fail "the latest cfgd release: resolve_version failed"
[ "$got" = v0.12.0 ] || fail "the latest cfgd release: chose '$got', want 'v0.12.0'"
tr -d ' \n' < "$scratch/releases.json" > "$scratch/releases-min.json"
got="$(VERSION=latest REPO=o/r resolve_latest "$scratch/releases-min.json")" || fail "the latest cfgd release, minified: resolve_version failed"
[ "$got" = v0.12.0 ] || fail "the latest cfgd release, minified: chose '$got', want 'v0.12.0'"

# A version named outright is used as it is, with no request.
got="$(VERSION=v0.9.0 REPO=o/r resolve_latest /nonexistent)" || fail "a named version: resolve_version failed"
[ "$got" = v0.9.0 ] || fail "a named version: resolved to '$got', want 'v0.9.0'"

# A list holding only other crates' releases stops the installer with an error
# naming the repository.
cat > "$scratch/siblings.json" <<'JSON'
[
  {"tag_name": "operator-v0.11.0", "assets": []},
  {"tag_name": "csi-vnightly", "assets": []},
  {"tag_name": "vnightly", "assets": []},
  {"tag_name": "csi-v0.8.0", "assets": []}
]
JSON
if got="$(VERSION=latest REPO=o/r resolve_latest "$scratch/siblings.json" 2> "$scratch/stderr")"; then
    fail "only other crates' releases: resolve_version succeeded, choosing '$got'"
fi
grep -q "Could not find a cfgd release in the releases of o/r" "$scratch/stderr" \
    || fail "only other crates' releases: stderr does not name the repository: $(cat "$scratch/stderr")"

if [ "$failures" -ne 0 ]; then
    printf '%d installer check(s) failed\n' "$failures" >&2
    exit 1
fi
printf 'installer checks passed\n'
