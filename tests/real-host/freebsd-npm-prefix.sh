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

set -u

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
if id "$USER_NAME" >/dev/null 2>&1; then
    echo "==> removing the previous $USER_NAME"
    pw userdel -n "$USER_NAME" -r || fail 1 "pw userdel failed"
fi
echo "==> creating $USER_NAME"
pw useradd -n "$USER_NAME" -m -s /bin/sh || fail 1 "pw useradd failed"

HOME_DIR=$(getent passwd "$USER_NAME" | cut -d: -f6)
[ -d "$HOME_DIR" ] || fail 1 "no home directory for $USER_NAME"
CONF_DIR=$HOME_DIR/cfgd-config
CFGD=$HOME_DIR/cfgd

# The binary is copied into the user's own home because the CI workspace is
# root-owned and an unprivileged user cannot traverse into it.
cp "$BIN" "$CFGD" || fail 1 "cannot stage the cfgd binary"
chmod 0755 "$CFGD"

mkdir -p "$CONF_DIR/profiles"
cat > "$CONF_DIR/cfgd.yaml" <<'YAML'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: npm-prefix-probe
spec:
  profile: npmtest
YAML
cat > "$CONF_DIR/profiles/npmtest.yaml" <<YAML
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

echo
echo "===== (a) the configured prefix is unwritable to $USER_NAME ====="
PREFIX=$(run_as_user 'npm config get prefix' | tail -1)
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
echo "$RAN" | grep -q cfgd || fail 12 "$FALLBACK_BIN ran but did not produce its own output"
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
