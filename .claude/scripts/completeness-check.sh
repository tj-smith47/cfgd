#!/usr/bin/env bash
# Completeness check: verify a phase or task touched ALL affected surfaces
# Run after completing any implementation phase, naming migration, or refactor.
#
# Usage: bash .claude/scripts/completeness-check.sh [pattern...]
#   Without args: runs all built-in checks
#   With args: treats each arg as a grep pattern to search for stale references
#
# Exit codes: 0 = clean, 1 = stale references found
set -euo pipefail
cd "$(dirname "$0")/../.."

RED=$(printf '\033[0;31m')
GREEN=$(printf '\033[0;32m')
YELLOW=$(printf '\033[0;33m')
BOLD=$(printf '\033[1m')
RESET=$(printf '\033[0m')

ISSUES=0
WARNINGS=0

log_issue() { echo "${RED}ISSUE${RESET}: $1"; ISSUES=$((ISSUES + 1)); }
log_warn()  { echo "${YELLOW}WARN${RESET}:  $1"; WARNINGS=$((WARNINGS + 1)); }
log_ok()    { echo "${GREEN}OK${RESET}:    $1"; }

SEARCH_DIRS="crates/ charts/ tests/ examples/ docs/ schemas/ manifests/"
SEARCH_INCLUDES="--include=*.rs --include=*.yaml --include=*.yml --include=*.md --include=*.sh --include=*.json --include=*.toml"

echo "${BOLD}=== Completeness Check ===${RESET}"
echo ""

# --- 1. Check for stale references to deleted/renamed files ---
echo "--- Stale File References ---"
for ref in ".claude/architecture.md" ".claude/team-config-controller.md" ".claude/modules-design.md"; do
    hits=$(grep -rn "$ref" $SEARCH_DIRS .claude/ CLAUDE.md README.md $SEARCH_INCLUDES 2>/dev/null \
        | grep -v 'COMPLETED.md\|target/\|\.git/\|completeness-check\.sh' || true)
    if [[ -n "$hits" ]]; then
        log_issue "References to deleted/renamed '$ref':"
        echo "$hits" | head -5
    fi
done
log_ok "Stale file reference check complete"

# --- 2. Check serde conventions are consistent ---
echo ""
echo "--- Serde Convention Consistency ---"
bad_serde=$(grep -rn 'rename_all = "kebab-case"\|rename_all = "lowercase"' crates/ --include='*.rs' 2>/dev/null || true)
if [[ -n "$bad_serde" ]]; then
    log_issue "Found kebab-case/lowercase serde attributes (should be camelCase or removed):"
    echo "$bad_serde"
else
    log_ok "All serde rename_all attributes use camelCase or are removed"
fi

bad_rename=$(grep -rn '#\[serde(rename = "' crates/ --include='*.rs' 2>/dev/null \
    | grep -E 'rename = "[a-z]+-[a-z]+"' || true)
if [[ -n "$bad_rename" ]]; then
    log_issue "Found kebab-case explicit serde rename attributes:"
    echo "$bad_rename"
else
    log_ok "All explicit serde renames use camelCase"
fi

# --- 3. Check that docs match code conventions ---
echo ""
echo "--- Documentation Consistency ---"
# Generate expected camelCase field names from config structs
config_fields=$(grep -E '^\s+pub [a-z_]+:' crates/cfgd-core/src/config/mod.rs 2>/dev/null \
    | sed 's/.*pub \([a-z_]*\):.*/\1/' \
    | grep '_' \
    | sed 's/_/-/g' \
    | sort -u \
    | paste -sd '|' - || true)

if [[ -n "$config_fields" ]]; then
    # Check docs for kebab-case field names that should be camelCase
    doc_hits=$(grep -rnE "($config_fields)" docs/ README.md --include='*.md' 2>/dev/null \
        | grep -v 'spec-reference/\|\.claude/' \
        | grep -v '\-\-' \
        | grep -v '\.txt\|\.key\|\.sh\|\.rs\|\.yaml\|\.json' \
        | grep -v 'keygen\|x86_64\|aarch64\|cert-manager\|kube-system' \
        | head -10 || true)
    if [[ -n "$doc_hits" ]]; then
        log_warn "Possible kebab-case config field names in docs (verify these are field refs, not prose):"
        echo "$doc_hits"
    else
        log_ok "No obvious kebab-case config field names in documentation"
    fi
fi

# --- 4. Check that YAML examples parse with current schema ---
echo ""
echo "--- YAML Example Consistency ---"
for yaml in cfgd.yaml examples/cfgd.yaml examples/node/cfgd.yaml; do
    if [[ -f "$yaml" ]]; then
        if grep -q 'apiVersion: cfgd.io' "$yaml" 2>/dev/null; then
            # Check for obvious kebab-case fields
            kebab=$(grep -nE '^\s+[a-z]+-[a-z]+:' "$yaml" \
                | grep -v 'apiVersion\|kind\|metadata\|spec\|status' \
                | grep -v '^\s*#' || true)
            if [[ -n "$kebab" ]]; then
                log_issue "Kebab-case fields in $yaml:"
                echo "$kebab"
            fi
        fi
    fi
done
log_ok "YAML example check complete"

# --- 5. Check for orphaned test fixtures ---
echo ""
echo "--- Test Fixture Consistency ---"
for fixture in tests/e2e/*/fixtures/**/*.yaml; do
    if [[ -f "$fixture" ]]; then
        kebab=$(grep -nE '^\s+[a-z]+-[a-z]+:' "$fixture" \
            | grep -v 'apiVersion\|kind\|metadata\|spec\|status\|cert-manager\|kube-system\|node-\|cfgd-' \
            | grep -v '^\s*#' || true)
        if [[ -n "$kebab" ]]; then
            log_warn "Possible stale kebab-case in $fixture:"
            echo "$kebab" | head -3
        fi
    fi
done 2>/dev/null || true
log_ok "Test fixture check complete"

# --- 6. Custom pattern search (user-supplied args) ---
if [[ $# -gt 0 ]]; then
    echo ""
    echo "--- Custom Pattern Search ---"
    for pattern in "$@"; do
        hits=$(grep -rn "$pattern" $SEARCH_DIRS .claude/ CLAUDE.md README.md $SEARCH_INCLUDES 2>/dev/null \
            | grep -v 'target/\|\.git/\|COMPLETED.md' \
            | head -10 || true)
        if [[ -n "$hits" ]]; then
            log_issue "Pattern '$pattern' still found:"
            echo "$hits"
        else
            log_ok "Pattern '$pattern' not found"
        fi
    done
fi

# --- 7. Build verification ---
echo ""
echo "--- Build & Test ---"
if cargo check --workspace 2>/dev/null; then
    log_ok "cargo check passes"
else
    log_issue "cargo check failed"
fi

if cargo clippy --workspace -- -D warnings 2>/dev/null; then
    log_ok "cargo clippy clean"
else
    log_issue "cargo clippy has warnings/errors"
fi

# --- Summary ---
echo ""
echo "${BOLD}=== Completeness Check: $ISSUES issues, $WARNINGS warnings ===${RESET}"
[[ "$ISSUES" -gt 0 ]] && exit 1
exit 0
