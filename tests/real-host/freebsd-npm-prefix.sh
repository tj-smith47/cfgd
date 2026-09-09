#!/bin/sh
# Prove the unprivileged npm global-prefix fallback documented in
# docs/packages.md on a real FreeBSD host.
#
# The unit pins for `resolve_npm_prefix_with` inject both elevation and the
# write-probe, so they can never observe the one thing that matters here: a
# real non-root user, a real `www/npm` whose configured prefix is the
# root-owned /usr/local, and npm itself honouring the `--prefix` cfgd passes.
# Only a real host answers that, so this runs on the FreeBSD CI guest.
#
# Usage: freebsd-npm-prefix.sh <path-to-cfgd-binary>
# Must run as root: it installs npm, creates the unprivileged user, and then
# drops to that user for every cfgd invocation.
#
# Exit codes: 1 setup refused, 11/12/13/14 assertion a/b/c/d failed.
#
# `set -e` is load-bearing rather than tidy: every assertion below judges the
# OUTPUT of a command run through `su -l`, so a setup step that failed silently
# would be read as a verdict about cfgd. FreeBSD's /bin/sh has no `pipefail`,
# so every pipeline here reads its own producer's status explicitly.

set -eu

BIN=${1:-}
USER_NAME=${CFGD_NPM_TEST_USER:-cfgdnpm}
PKG_NAME=cowsay
PKG_BIN=cowsay

fail() {
    code=$1
    shift
    echo "FAIL: $*" >&2
    exit "$code"
}

[ -n "$BIN" ] || fail 1 "usage: $0 <path-to-cfgd-binary>"
[ -x "$BIN" ] || fail 1 "not an executable cfgd binary: $BIN"
[ "$(id -u)" = "0" ] || fail 1 "must run as root (creates a user and installs npm)"
[ "$(uname -s)" = "FreeBSD" ] || fail 1 "FreeBSD only (uname -s is $(uname -s))"

command -v npm >/dev/null 2>&1 || {
    echo "==> installing www/npm"
    pkg install -y npm || fail 1 "pkg install npm failed"
}

# A fresh user every run is what makes assertion (d) meaningful: the second
# apply can only be judged a no-op if the first one is the install.
# `pw userdel -r` deletes the account's home directory and its mail spool, and
# CFGD_NPM_TEST_USER can name anybody. Nothing is removed unless the account
# looks like the disposable one this check creates: an unprivileged uid, and a
# home under /home that is not /home itself.
delete_test_user() {
    victim_uid=$(id -u "$USER_NAME") || fail 1 "cannot read the uid of $USER_NAME"
    victim_home=$(passwd_home "$USER_NAME")
    [ "$victim_uid" -ge 1000 ] ||
        fail 1 "refusing to delete $USER_NAME: uid $victim_uid is a system account"
    case $victim_home in
    */../* | */..)
        fail 1 "refusing to delete $USER_NAME: home '$victim_home' contains a '..' component"
        ;;
    /home/?*) ;;
    *) fail 1 "refusing to delete $USER_NAME: home '$victim_home' is not under /home" ;;
    esac
    pw userdel -n "$USER_NAME" -r || fail 1 "pw userdel failed"
}

passwd_home() {
    entry=$(getent passwd "$1") || fail 1 "no passwd entry for $1"
    echo "$entry" | cut -d: -f6
}

# `pw` accepts a login name containing dots, so `CFGD_NPM_TEST_USER=..` would
# record home /home/.. and hand `pw userdel -r` every real user's home. The shape
# is judged here, before any `pw` call and before the home check downstream.
[ -n "$USER_NAME" ] || fail 1 "CFGD_NPM_TEST_USER is empty"
case $USER_NAME in
*[!a-z0-9_-]* | [!a-z_]*)
    fail 1 "CFGD_NPM_TEST_USER must be a plain login name ([a-z_][a-z0-9_-]*), got '$USER_NAME'"
    ;;
esac
if id "$USER_NAME" >/dev/null 2>&1; then
    echo "==> removing the previous $USER_NAME"
    delete_test_user
fi
echo "==> creating $USER_NAME"
pw useradd -n "$USER_NAME" -m -s /bin/sh || fail 1 "pw useradd failed"

HOME_DIR=$(passwd_home "$USER_NAME")
[ -d "$HOME_DIR" ] || fail 1 "no home directory for $USER_NAME"
CONF_DIR=$HOME_DIR/cfgd-config
CFGD=$HOME_DIR/cfgd

# The binary is copied into the user's own home because the CI workspace is
# root-owned and an unprivileged user cannot traverse into it.
cp "$BIN" "$CFGD" || fail 1 "cannot stage the cfgd binary"
chmod 0755 "$CFGD" || fail 1 "cannot make the staged cfgd binary executable"

mkdir -p "$CONF_DIR/profiles" || fail 1 "cannot create $CONF_DIR/profiles"
cat > "$CONF_DIR/cfgd.yaml" <<'YAML' || fail 1 "cannot write $CONF_DIR/cfgd.yaml"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: npm-prefix-probe
spec:
  profile: npmtest
YAML
cat > "$CONF_DIR/profiles/npmtest.yaml" <<YAML || fail 1 "cannot write the test profile"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: npmtest
spec:
  packages:
    npm:
      - $PKG_NAME
YAML
chown -R "$USER_NAME:$USER_NAME" "$CONF_DIR" "$CFGD" || fail 1 "chown failed"

run_as_user() {
    su -l "$USER_NAME" -c "$1"
}

cfgd_run() {
    run_as_user "$CFGD --config-dir $CONF_DIR --color never $1"
}

# Assertion (a) is a NEGATIVE: `test -w` must fail. A login shell that cannot
# start at all fails it too, for a reason that has nothing to do with npm's
# prefix, so the shell itself is proven first. This is not hypothetical: a
# root-only /etc/profile.d/cfgd-env.sh aborted `su -l` here with 126.
echo "==> checking that $USER_NAME has a working login shell"
LOGIN_PROBE=$(run_as_user 'echo LOGIN_OK' 2>&1) ||
    fail 1 "su -l $USER_NAME could not run a command: $LOGIN_PROBE"
case $LOGIN_PROBE in
*LOGIN_OK*) ;;
*) fail 1 "su -l $USER_NAME produced no output of its own: $LOGIN_PROBE" ;;
esac

echo
echo "===== (a) the configured prefix is unwritable to $USER_NAME ====="
PREFIX_RAW=$(run_as_user 'npm config get prefix' 2>&1) ||
    fail 1 "npm config get prefix failed for $USER_NAME: $PREFIX_RAW"
PREFIX=$(echo "$PREFIX_RAW" | tail -1)
echo "npm config get prefix -> $PREFIX"
[ "$PREFIX" = "/usr/local" ] || fail 11 "expected npm's configured prefix to be /usr/local, got '$PREFIX'"
if run_as_user "test -w $PREFIX"; then
    fail 11 "$PREFIX is writable to $USER_NAME, so the fallback arm is not the one under test"
fi
echo "PASS (a): prefix $PREFIX, not writable to $USER_NAME"

echo
echo "===== cfgd plan ====="
PLAN_OUT=$(cfgd_run plan 2>&1) || fail 1 "cfgd plan failed: $PLAN_OUT"
echo "$PLAN_OUT"

echo
echo "===== cfgd apply --yes (first) ====="
APPLY_OUT=$(cfgd_run 'apply --yes' 2>&1) || fail 1 "cfgd apply failed: $APPLY_OUT"
echo "$APPLY_OUT"

echo
echo "===== cfgd status ====="
STATUS_OUT=$(cfgd_run status 2>&1) || fail 1 "cfgd status failed: $STATUS_OUT"
echo "$STATUS_OUT"

echo
echo "===== cfgd -o json status ====="
JSON_OUT=$(run_as_user "$CFGD --config-dir $CONF_DIR -o json status" 2>&1) || fail 1 "cfgd -o json status failed: $JSON_OUT"
echo "$JSON_OUT"

echo
echo "===== (b) the package's binary landed under the fallback prefix ====="
FALLBACK_BIN=$HOME_DIR/.npm-global/bin/$PKG_BIN
ls -l "$HOME_DIR/.npm-global/bin/" || fail 12 "no fallback bin directory at $HOME_DIR/.npm-global/bin"
[ -e "$FALLBACK_BIN" ] || fail 12 "$FALLBACK_BIN does not exist, so the --prefix fallback did not install there"
RAN=$(run_as_user "$FALLBACK_BIN -t cfgd" 2>&1) || fail 12 "$FALLBACK_BIN did not run: $RAN"
echo "$RAN"
# `cfgd` is the word handed in, so finding it proves nothing about WHAT ran.
# The cow itself is cowsay's own output, and the bubble must carry the argument.
# The horns are the one part every mode draws: `-t` tires the eyes to `(--)`.
echo "$RAN" | grep -qF '^__^' ||
    fail 12 "$FALLBACK_BIN ran but drew no cow, so it is not the cowsay npm installed"
echo "$RAN" | grep -qF 'cfgd' ||
    fail 12 "$FALLBACK_BIN drew a cow but not the text it was given"
echo "PASS (b): $FALLBACK_BIN exists and runs"

echo
echo "===== (c) the apply's row is Ok and the env surface exports the bin dir ====="
echo "$APPLY_OUT" | grep -q "✓ npm install $PKG_NAME" ||
    fail 13 "the apply did not render an Ok row for 'npm install $PKG_NAME'"
echo "$JSON_OUT" | grep -q "\"npm/$PKG_NAME\"" ||
    fail 13 "-o json status does not name the npm/$PKG_NAME package row"
ENV_FILE=$HOME_DIR/.cfgd.env
[ -f "$ENV_FILE" ] || fail 13 "no generated env file at $ENV_FILE"
cat "$ENV_FILE"
grep -q '^export PATH=.*\.npm-global/bin' "$ENV_FILE" ||
    fail 13 "$ENV_FILE does not put the fallback bin directory on PATH"
echo "PASS (c): Ok row rendered and $ENV_FILE exports the fallback bin directory"

echo
echo "===== cfgd apply --yes (second) ====="
APPLY2_OUT=$(cfgd_run 'apply --yes' 2>&1) || fail 1 "second cfgd apply failed: $APPLY2_OUT"
echo "$APPLY2_OUT"

echo
echo "===== (d) the second apply is a no-op for the package ====="
if echo "$APPLY2_OUT" | grep -q "npm install $PKG_NAME"; then
    fail 14 "the second apply planned 'npm install $PKG_NAME' again, so the resolved prefix did not survive the run"
fi
echo "$APPLY2_OUT" | grep -q "Nothing to do" ||
    fail 14 "the second apply reported work to do rather than converged state"
echo "PASS (d): the second apply is a no-op"

echo
echo "ALL ASSERTIONS PASSED (a, b, c, d)"
