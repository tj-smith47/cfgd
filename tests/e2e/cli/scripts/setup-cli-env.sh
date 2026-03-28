#!/usr/bin/env bash
# Shared setup for CLI E2E tests.
# Source this from each test-*.sh file.
# Creates: CFGD, SCRATCH, FIXTURES, CFG, TGT, STATE, CONF, C, SOURCE_REPO
set -euo pipefail

# Prevent double-sourcing
if [ -n "${CLI_ENV_LOADED:-}" ]; then return 0; fi
CLI_ENV_LOADED=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

# Find cfgd binary
if [ -z "${CFGD:-}" ]; then
    if [ -f "$REPO_ROOT/target/release/cfgd" ]; then
        CFGD="$REPO_ROOT/target/release/cfgd"
    elif [ -f "$REPO_ROOT/target/debug/cfgd" ]; then
        CFGD="$REPO_ROOT/target/debug/cfgd"
    else
        CFGD="$(command -v cfgd)"
    fi
fi
export CFGD

# Scratch directory (each domain file gets its own subdir)
if [ -z "${CLI_SCRATCH:-}" ]; then
    CLI_SCRATCH=$(mktemp -d)
    trap 'rm -rf "$CLI_SCRATCH"' EXIT
fi
export CLI_SCRATCH

# Per-file scratch (uses caller's filename to create unique subdir)
CALLER="$(basename "${BASH_SOURCE[1]}" .sh)"
SCRATCH="$CLI_SCRATCH/$CALLER"
mkdir -p "$SCRATCH"

# Git identity — use isolated config so tests never modify user's global gitconfig
export GIT_CONFIG_GLOBAL="$CLI_SCRATCH/.gitconfig"
if [ ! -f "$GIT_CONFIG_GLOBAL" ]; then
    git config --file "$GIT_CONFIG_GLOBAL" user.name "cfgd-test"
    git config --file "$GIT_CONFIG_GLOBAL" user.email "test@cfgd.io"
fi

# --- Helpers ---

setup_config_dir() {
    local config_dir="$1"
    local target_dir="$2"
    mkdir -p "$config_dir/profiles" "$config_dir/files" "$config_dir/modules" "$target_dir"
    for f in "$FIXTURES/profiles/"*.yaml; do
        sed "s|TARGET_DIR|$target_dir|g" "$f" > "$config_dir/profiles/$(basename "$f")"
    done
    cp -r "$FIXTURES/files/"* "$config_dir/files/"
    cat > "$config_dir/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: exhaustive-e2e
spec:
  profile: dev
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
YAML
}

run() {
    local rc=0
    OUTPUT=$("$CFGD" "$@" 2>&1) || rc=$?
    RC=$rc
}

assert_ok() {
    if [ "$RC" -ne 0 ]; then
        echo "  ASSERT FAILED: expected exit 0, got $RC"
        echo "$OUTPUT" | head -5 | sed 's/^/    /'
        return 1
    fi
}

assert_fail() {
    if [ "$RC" -eq 0 ]; then
        echo "  ASSERT FAILED: expected non-zero exit, got 0"
        return 1
    fi
}

# Standard config for this domain file
CFG="$SCRATCH/cfg"
TGT="$SCRATCH/home"
STATE="$SCRATCH/state"
mkdir -p "$STATE"
setup_config_dir "$CFG" "$TGT"
CONF="$CFG/cfgd.yaml"
C="--config $CONF --state-dir $STATE --no-color"

# Source repo (shared across all domain files via CLI_SCRATCH)
SOURCE_REPO="$CLI_SCRATCH/source-repo"
if [ ! -d "$SOURCE_REPO/.git" ]; then
    mkdir -p "$SOURCE_REPO/profiles" "$SOURCE_REPO/files"
    cat > "$SOURCE_REPO/cfgd-source.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
spec:
  provides:
    profiles: [base, dev, work-dev]
  policy:
    recommended:
      packages:
        brew:
          formulae: [jq, yq]
    constraints: {}
YAML
    for f in "$FIXTURES/profiles/"*.yaml; do
        sed "s|TARGET_DIR|$CLI_SCRATCH/source-target|g" "$f" > "$SOURCE_REPO/profiles/$(basename "$f")"
    done
    cp -r "$FIXTURES/files/"* "$SOURCE_REPO/files/" 2>/dev/null || true
    (cd "$SOURCE_REPO" && git init -q -b master && git add -A && git commit -qm "init source repo")
fi
