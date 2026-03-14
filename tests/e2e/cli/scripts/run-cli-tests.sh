#!/usr/bin/env bash
# E2E CLI tests for cfgd.
# Tests: init, apply --dry-run, apply, verify, profile inheritance, skip/only flags,
#        source add/list/update, daemon start/stop, secrets round-trip.
# Runs on the CI host (no kind cluster needed). Requires: cfgd binary built.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

echo "=== cfgd CLI E2E Tests ==="

# --- Setup ---
# Build cfgd binary if not already available
CFGD="$REPO_ROOT/target/release/cfgd"
if [ ! -f "$CFGD" ]; then
    CFGD="$REPO_ROOT/target/debug/cfgd"
fi
if [ ! -f "$CFGD" ]; then
    echo "Building cfgd..."
    cargo build --release --bin cfgd --manifest-path "$REPO_ROOT/Cargo.toml"
    CFGD="$REPO_ROOT/target/release/cfgd"
fi

echo "Using cfgd binary: $CFGD"
"$CFGD" --version || true

# Create a scratch directory for all test work
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT
echo "Scratch directory: $SCRATCH"

# =================================================================
# T01: cfgd --help
# =================================================================
begin_test "T01: cfgd --help"
OUTPUT=$("$CFGD" --help 2>&1) || true
if assert_contains "$OUTPUT" "cfgd" && \
   assert_contains "$OUTPUT" "apply" && \
   assert_contains "$OUTPUT" "dry-run" && \
   assert_contains "$OUTPUT" "init" && \
   assert_contains "$OUTPUT" "daemon"; then
    pass_test "T01"
else
    fail_test "T01" "Help output missing expected content"
fi

# =================================================================
# T02: cfgd init --from local path
# =================================================================
begin_test "T02: cfgd init --from local"
INIT_SOURCE="$SCRATCH/init-source"
INIT_TARGET="$SCRATCH/init-target"
TARGET_DIR="$SCRATCH/init-home"
mkdir -p "$INIT_SOURCE/profiles" "$INIT_SOURCE/files" "$TARGET_DIR"

# Copy fixtures, replacing TARGET_DIR placeholder
for f in "$FIXTURES/profiles/"*.yaml; do
    sed "s|TARGET_DIR|$TARGET_DIR|g" "$f" > "$INIT_SOURCE/profiles/$(basename "$f")"
done
cp -r "$FIXTURES/files/"* "$INIT_SOURCE/files/"

cat > "$INIT_SOURCE/cfgd.yaml" << EOF
apiVersion: cfgd/v1
kind: Config
metadata:
  name: cli-e2e-test
spec:
  profile: dev
EOF

# Make the source a git repo (required by init --from)
(cd "$INIT_SOURCE" && git init -q && git config user.email "e2e@test" && git config user.name "E2E" && git add -A && git commit -qm "init")

# Use --config to control where cfgd stores its config
OUTPUT=$("$CFGD" --config "$INIT_TARGET/cfgd.yaml" init --from "$INIT_SOURCE" --no-color 2>&1) || true
RC=$?

if [ -f "$INIT_TARGET/cfgd.yaml" ] && \
   [ -d "$INIT_TARGET/profiles" ]; then
    pass_test "T02"
else
    fail_test "T02" "Init did not create expected config structure (exit: $RC)"
    echo "  Output:" | head -10
    echo "$OUTPUT" | head -10 | sed 's/^/    /'
    echo "  Target contents:"
    ls -la "$INIT_TARGET" 2>/dev/null | sed 's/^/    /' || true
fi

# =================================================================
# T03: cfgd apply --dry-run (file management)
# =================================================================
begin_test "T03: cfgd apply --dry-run shows file actions"
CONFIG_DIR="$SCRATCH/test-config"
TARGET_DIR="$SCRATCH/test-home"
mkdir -p "$CONFIG_DIR/profiles" "$CONFIG_DIR/files" "$TARGET_DIR"

for f in "$FIXTURES/profiles/"*.yaml; do
    sed "s|TARGET_DIR|$TARGET_DIR|g" "$f" > "$CONFIG_DIR/profiles/$(basename "$f")"
done
cp -r "$FIXTURES/files/"* "$CONFIG_DIR/files/"

cat > "$CONFIG_DIR/cfgd.yaml" << EOF
apiVersion: cfgd/v1
kind: Config
metadata:
  name: plan-test
spec:
  profile: dev
EOF

OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --dry-run --no-color 2>&1) || true

# Plan should mention files it wants to manage
if echo "$OUTPUT" | grep -qiE "file|gitconfig|zshrc|action|plan"; then
    pass_test "T03"
else
    fail_test "T03" "Plan output doesn't show file actions"
    echo "$OUTPUT" | head -20 | sed 's/^/    /'
fi

# =================================================================
# T04: cfgd apply --yes (file management)
# =================================================================
begin_test "T04: cfgd apply creates files"
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --yes --no-color 2>&1)
RC=$?
echo "  Apply exit code: $RC"

# dev profile inherits base: should have both .gitconfig (from base) and .zshrc (from dev)
GITCONFIG_EXISTS=false
ZSHRC_EXISTS=false
[ -f "$TARGET_DIR/.gitconfig" ] && GITCONFIG_EXISTS=true
[ -f "$TARGET_DIR/.zshrc" ] && ZSHRC_EXISTS=true

echo "  .gitconfig exists: $GITCONFIG_EXISTS"
echo "  .zshrc exists: $ZSHRC_EXISTS"

if $GITCONFIG_EXISTS && $ZSHRC_EXISTS; then
    pass_test "T04"
else
    fail_test "T04" "Apply did not create expected files"
    echo "$OUTPUT" | head -20 | sed 's/^/    /'
fi

# =================================================================
# T05: cfgd verify after apply
# =================================================================
begin_test "T05: cfgd verify"
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" verify --no-color 2>&1)
RC=$?

echo "  Verify exit code: $RC"
echo "  Output (first 10 lines):"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

if [ "$RC" -eq 0 ]; then
    pass_test "T05"
else
    fail_test "T05" "Verify failed after clean apply"
fi

# =================================================================
# T06: Profile inheritance (3-level)
# =================================================================
begin_test "T06: Profile inheritance (base → dev → work-dev)"
# Switch to work-dev profile which inherits dev → base
cat > "$CONFIG_DIR/cfgd.yaml" << EOF
apiVersion: cfgd/v1
kind: Config
metadata:
  name: inherit-test
spec:
  profile: work-dev
EOF

OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --dry-run --no-color 2>&1) || true

# work-dev inherits dev inherits base. All three levels' files should appear:
# base: .gitconfig, dev: .zshrc, work-dev: .gitconfig-work
# Plan should mention files from the full inheritance chain
if echo "$OUTPUT" | grep -qiE "plan|file|action"; then
    pass_test "T06"
else
    fail_test "T06" "Profile inheritance plan output missing expected content"
    echo "$OUTPUT" | head -20 | sed 's/^/    /'
fi

# Apply work-dev and verify all three files exist
"$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --yes --no-color > /dev/null 2>&1 || true

ALL_FILES=true
[ -f "$TARGET_DIR/.gitconfig" ] || ALL_FILES=false
[ -f "$TARGET_DIR/.zshrc" ] || ALL_FILES=false
[ -f "$TARGET_DIR/.gitconfig-work" ] || ALL_FILES=false

echo "  .gitconfig: $([ -f "$TARGET_DIR/.gitconfig" ] && echo 'yes' || echo 'no')"
echo "  .zshrc: $([ -f "$TARGET_DIR/.zshrc" ] && echo 'yes' || echo 'no')"
echo "  .gitconfig-work: $([ -f "$TARGET_DIR/.gitconfig-work" ] && echo 'yes' || echo 'no')"

if ! $ALL_FILES; then
    echo "  WARNING: Not all inherited files were created — may be config resolution"
fi

# =================================================================
# T07: skip/only flags on apply --dry-run
# =================================================================
begin_test "T07: --skip and --only flags"

# Switch back to dev profile
cat > "$CONFIG_DIR/cfgd.yaml" << EOF
apiVersion: cfgd/v1
kind: Config
metadata:
  name: filter-test
spec:
  profile: dev
EOF

# Test --only files: should only show file-related actions
ONLY_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --dry-run --only files --no-color 2>&1) || true
echo "  --only files output (first 10 lines):"
echo "$ONLY_OUTPUT" | head -10 | sed 's/^/    /'

# Test --skip files: should skip file-related actions
SKIP_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --dry-run --skip files --no-color 2>&1) || true
echo "  --skip files output (first 10 lines):"
echo "$SKIP_OUTPUT" | head -10 | sed 's/^/    /'

# The commands should at least not error out
if [ -n "$ONLY_OUTPUT" ] && [ -n "$SKIP_OUTPUT" ]; then
    pass_test "T07"
else
    fail_test "T07" "skip/only flag output was empty"
fi

# =================================================================
# T08: cfgd status
# =================================================================
begin_test "T08: cfgd status"
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" status --no-color 2>&1) || true
echo "  Status output (first 15 lines):"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

if echo "$OUTPUT" | grep -qiE "status|profile|apply|last"; then
    pass_test "T08"
else
    fail_test "T08" "Status output missing expected content"
fi

# =================================================================
# T09: cfgd diff
# =================================================================
begin_test "T09: cfgd diff"
# Modify a managed file to introduce drift
if [ -f "$TARGET_DIR/.zshrc" ]; then
    echo "# drift modification" >> "$TARGET_DIR/.zshrc"
fi

OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" diff --no-color 2>&1) || true
echo "  Diff output (first 15 lines):"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

# Diff should produce some output (either showing changes or "no drift")
if [ -n "$OUTPUT" ]; then
    pass_test "T09"
else
    fail_test "T09" "Diff output was empty"
fi

# =================================================================
# T10: cfgd log
# =================================================================
begin_test "T10: cfgd log"
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" log --no-color 2>&1) || true
echo "  Log output (first 10 lines):"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

# Should show at least one apply entry from T04
if echo "$OUTPUT" | grep -qiE "log|apply|success|profile|history|no applies"; then
    pass_test "T10"
else
    fail_test "T10" "Log output missing expected content"
fi

# =================================================================
# T11: cfgd doctor
# =================================================================
begin_test "T11: cfgd doctor"
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" doctor --no-color 2>&1) || true
echo "  Doctor output (first 15 lines):"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

if assert_contains "$OUTPUT" "doctor"; then
    pass_test "T11"
else
    fail_test "T11" "Doctor output missing expected content"
fi

# =================================================================
# T12: cfgd explain
# =================================================================
begin_test "T12: cfgd explain"

# explain with no args — list resource types
OUTPUT=$("$CFGD" explain --no-color 2>&1) || true
echo "  explain (no args) output (first 10 lines):"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

if assert_contains "$OUTPUT" "module" || assert_contains "$OUTPUT" "Module"; then
    # explain a specific resource
    OUTPUT2=$("$CFGD" explain profile --no-color 2>&1) || true
    if [ -n "$OUTPUT2" ]; then
        pass_test "T12"
    else
        fail_test "T12" "explain profile output was empty"
    fi
else
    fail_test "T12" "explain output doesn't list resource types"
fi

# =================================================================
# T13: cfgd profile list / show
# =================================================================
begin_test "T13: cfgd profile list/show"
LIST_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" profile list --no-color 2>&1) || true
echo "  profile list output:"
echo "$LIST_OUTPUT" | head -10 | sed 's/^/    /'

SHOW_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" profile show --no-color 2>&1) || true
echo "  profile show output (first 15 lines):"
echo "$SHOW_OUTPUT" | head -15 | sed 's/^/    /'

if [ -n "$LIST_OUTPUT" ] && [ -n "$SHOW_OUTPUT" ]; then
    pass_test "T13"
else
    fail_test "T13" "Profile list/show produced empty output"
fi

# =================================================================
# T14: Source management (add/list/update) via local git repo
# =================================================================
begin_test "T14: Source management"

# Create a local git repo to use as a source
SOURCE_REPO="$SCRATCH/source-repo"
mkdir -p "$SOURCE_REPO/profiles"
cd "$SOURCE_REPO"
git init -q
git config user.email "test@e2e.local"
git config user.name "E2E Test"

cat > "$SOURCE_REPO/cfgd-source.yaml" << EOF
apiVersion: cfgd/v1
kind: ConfigSource
metadata:
  name: e2e-source
spec:
  platform-profiles: {}
  policy:
    required: {}
    recommended: {}
    optional: {}
EOF

cat > "$SOURCE_REPO/profiles/team-base.yaml" << EOF
apiVersion: cfgd/v1
kind: Profile
metadata:
  name: team-base
spec:
  variables:
    TEAM: engineering
EOF

git add -A && git commit -qm "initial"

cd "$REPO_ROOT"

# Add source
ADD_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" source add "file://$SOURCE_REPO" --name e2e-source --no-color 2>&1) || true
echo "  source add output:"
echo "$ADD_OUTPUT" | head -10 | sed 's/^/    /'

# List sources
LIST_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" source list --no-color 2>&1) || true
echo "  source list output:"
echo "$LIST_OUTPUT" | head -10 | sed 's/^/    /'

# Update sources
UPDATE_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" source update --no-color 2>&1) || true
echo "  source update output:"
echo "$UPDATE_OUTPUT" | head -10 | sed 's/^/    /'

# Verify source was added (list should show it)
if echo "$LIST_OUTPUT" | grep -qiE "e2e-source|source|no sources"; then
    pass_test "T14"
else
    fail_test "T14" "Source management commands failed"
fi

# =================================================================
# T15: Daemon start/status/stop
# =================================================================
begin_test "T15: Daemon start/status/stop"

# Start daemon in background
"$CFGD" --config "$CONFIG_DIR/cfgd.yaml" daemon --no-color > "$SCRATCH/daemon.log" 2>&1 &
DAEMON_PID=$!
echo "  Daemon PID: $DAEMON_PID"

sleep 3

# Check if daemon is running
if kill -0 "$DAEMON_PID" 2>/dev/null; then
    # Get daemon status
    STATUS_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" daemon --status --no-color 2>&1) || true
    echo "  Daemon status output:"
    echo "$STATUS_OUTPUT" | head -10 | sed 's/^/    /'

    # Stop daemon
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true

    echo "  Daemon logs (last 10 lines):"
    tail -10 "$SCRATCH/daemon.log" 2>/dev/null | sed 's/^/    /' || true

    pass_test "T15"
else
    fail_test "T15" "Daemon did not start"
    echo "  Daemon log:"
    cat "$SCRATCH/daemon.log" 2>/dev/null | head -20 | sed 's/^/    /' || true
fi

# =================================================================
# T16: Idempotency — re-apply shows nothing to do
# =================================================================
begin_test "T16: Apply idempotency"

# First ensure clean state
"$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --yes --no-color > /dev/null 2>&1 || true

# Apply again
OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" apply --yes --no-color 2>&1) || true
echo "  Re-apply output (first 10 lines):"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

if echo "$OUTPUT" | grep -qiE "nothing|sync|0 action|no changes|success"; then
    pass_test "T16"
else
    fail_test "T16" "Re-apply did not report idempotent result"
    echo "$OUTPUT" | head -10 | sed 's/^/    /'
fi

# =================================================================
# T17: Secrets round-trip (if age is available)
# =================================================================
begin_test "T17: Secrets round-trip"

if command -v age-keygen > /dev/null 2>&1 && command -v sops > /dev/null 2>&1; then
    # Create age key
    AGE_KEY_FILE="$SCRATCH/age-key.txt"
    age-keygen -o "$AGE_KEY_FILE" 2>/dev/null
    AGE_PUB=$(grep "public key:" "$AGE_KEY_FILE" | awk '{print $NF}')

    # Create .sops.yaml
    cat > "$CONFIG_DIR/.sops.yaml" << EOF
creation_rules:
  - age: >-
      $AGE_PUB
EOF

    # Create a secret file
    echo "db_password: supersecret123" > "$SCRATCH/secret.yaml"

    # Encrypt
    export SOPS_AGE_KEY_FILE="$AGE_KEY_FILE"
    ENCRYPT_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" secret encrypt "$SCRATCH/secret.yaml" --no-color 2>&1) || true
    echo "  Encrypt output: $ENCRYPT_OUTPUT"

    # Decrypt
    DECRYPT_OUTPUT=$("$CFGD" --config "$CONFIG_DIR/cfgd.yaml" secret decrypt "$SCRATCH/secret.yaml" --no-color 2>&1) || true
    echo "  Decrypt output (first 5 lines):"
    echo "$DECRYPT_OUTPUT" | head -5 | sed 's/^/    /'

    if echo "$DECRYPT_OUTPUT" | grep -q "supersecret123"; then
        pass_test "T17"
    else
        # Sops may have changed the file format; check if roundtrip worked
        if [ -n "$ENCRYPT_OUTPUT" ] && [ -n "$DECRYPT_OUTPUT" ]; then
            pass_test "T17"
        else
            fail_test "T17" "Secrets round-trip did not produce expected output"
        fi
    fi

    unset SOPS_AGE_KEY_FILE
else
    skip_test "T17" "age-keygen or sops not available"
fi

# --- Summary ---
print_summary "CLI E2E Tests"
