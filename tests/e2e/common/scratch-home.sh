#!/usr/bin/env bash
# The ONE home redirect every e2e suite runs under.
#
# Sourced by tests/e2e/common/helpers.sh, and directly by every
# tests/e2e/*/scripts/run-all.sh so the redirect is in force where a run starts.
#
# The suites run the real cfgd binary as the invoking user. Without the
# redirect, every path cfgd resolves from $HOME or $XDG_* is that user's own:
# `--from` with no destination resolves to ~/.config/cfgd, `secret` writes
# ~/.config/cfgd/age-key.txt, and an apply deploys into the real home. All three
# happened — see .claude/rules/testing.md.

if [ -n "${E2E_SCRATCH_HOME_LOADED:-}" ]; then return 0; fi
E2E_SCRATCH_HOME_LOADED=1

# Captured before the redirect: what a passthrough resolves against, and where
# the config directory that must survive the run lives.
E2E_REAL_HOME="${E2E_REAL_HOME:-${HOME:?HOME must be set}}"
E2E_REAL_XDG_CONFIG_HOME="${E2E_REAL_XDG_CONFIG_HOME:-${XDG_CONFIG_HOME:-$E2E_REAL_HOME/.config}}"
E2E_REAL_CONFIG_DIR="${E2E_REAL_CONFIG_DIR:-$E2E_REAL_XDG_CONFIG_HOME/cfgd}"
export E2E_REAL_HOME E2E_REAL_XDG_CONFIG_HOME E2E_REAL_CONFIG_DIR

# What the suites genuinely read out of the real home, passed through one tool
# seam at a time rather than by inheriting the whole home. Each is set only when
# the caller has not already chosen one.
export CARGO_HOME="${CARGO_HOME:-$E2E_REAL_HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$E2E_REAL_HOME/.rustup}"
if [ -z "${KUBECONFIG:-}" ] && [ -f "$E2E_REAL_HOME/.kube/config" ]; then
    export KUBECONFIG="$E2E_REAL_HOME/.kube/config"
fi
if [ -z "${DOCKER_CONFIG:-}" ] && [ -d "$E2E_REAL_HOME/.docker" ]; then
    export DOCKER_CONFIG="$E2E_REAL_HOME/.docker"
fi
# helm resolves all three of its directories off XDG, which the redirect moves,
# so a cluster suite would otherwise lose its repo list and registry login.
export HELM_CONFIG_HOME="${HELM_CONFIG_HOME:-$E2E_REAL_XDG_CONFIG_HOME/helm}"
export HELM_CACHE_HOME="${HELM_CACHE_HOME:-${XDG_CACHE_HOME:-$E2E_REAL_HOME/.cache}/helm}"
export HELM_DATA_HOME="${HELM_DATA_HOME:-${XDG_DATA_HOME:-$E2E_REAL_HOME/.local/share}/helm}"

# The scratch root. A suite that made its own owns its removal; anything else
# gets one here, which cleanup_e2e drops.
if [ -z "${CLI_SCRATCH:-}" ]; then
    CLI_SCRATCH="$(mktemp -d)"
    E2E_SCRATCH_OWNED="$CLI_SCRATCH"
    export E2E_SCRATCH_OWNED
fi
export CLI_SCRATCH

E2E_SCRATCH_HOME="$CLI_SCRATCH/home"
mkdir -p "$E2E_SCRATCH_HOME/.config" "$E2E_SCRATCH_HOME/.local/state" \
    "$E2E_SCRATCH_HOME/.local/share" "$E2E_SCRATCH_HOME/.cache"
export HOME="$E2E_SCRATCH_HOME"
# Windows resolves `~` from USERPROFILE first.
export USERPROFILE="$E2E_SCRATCH_HOME"
export XDG_CONFIG_HOME="$E2E_SCRATCH_HOME/.config"
export XDG_STATE_HOME="$E2E_SCRATCH_HOME/.local/state"
export XDG_DATA_HOME="$E2E_SCRATCH_HOME/.local/share"
export XDG_CACHE_HOME="$E2E_SCRATCH_HOME/.cache"

# Asserted, not assumed: every guard below reads $HOME, so a redirect that
# silently did not happen would check the real home against itself and pass.
case "$HOME" in
    "$CLI_SCRATCH"/*) ;;
    *)
        echo "FATAL: e2e HOME ($HOME) is not inside the scratch root ($CLI_SCRATCH)" >&2
        exit 2
        ;;
esac

# A git identity, so a suite's `git init` / `git commit` works under a home that
# holds no gitconfig. GIT_CONFIG_GLOBAL wins over $HOME/.gitconfig, so a suite
# that sets its own keeps it.
if [ ! -f "$HOME/.gitconfig" ]; then
    git config --file "$HOME/.gitconfig" user.name "cfgd-test"
    git config --file "$HOME/.gitconfig" user.email "test@cfgd.io"
    git config --file "$HOME/.gitconfig" init.defaultBranch master
fi

# --- the real config directory comes out of the run untouched ---

# Every line carries `|| true`: the group is the left side of a pipeline, so it
# runs in a subshell that inherits `set -e`, and one unreadable subdirectory
# would abort it before the two `git` lines ran — leaving a shorter digest that
# still compares equal to itself and a guard weaker than it reads.
#
# `cksum` rather than a `find -printf` format: the CLI suite also runs on macOS,
# whose find has no -printf, and content is what a clobber changes. `.git` is
# walked as HEAD plus the porcelain status instead of byte-for-byte, because
# reading the status refreshes the index's stat cache and would move a
# byte-for-byte digest on its own.
e2e_real_config_fingerprint() {
    if [ ! -e "$E2E_REAL_CONFIG_DIR" ] && [ ! -L "$E2E_REAL_CONFIG_DIR" ]; then
        printf 'absent\n'
        return 0
    fi
    {
        readlink "$E2E_REAL_CONFIG_DIR" || true
        find -L "$E2E_REAL_CONFIG_DIR" -name .git -prune -o -print \
            -type f -exec cksum {} + 2>/dev/null || true
        git -C "$E2E_REAL_CONFIG_DIR" rev-parse HEAD 2>/dev/null || true
        git -C "$E2E_REAL_CONFIG_DIR" status --porcelain 2>/dev/null || true
    } | LC_ALL=C sort | { sha256sum 2>/dev/null || shasum -a 256; } | cut -d' ' -f1
}

E2E_REAL_CONFIG_FINGERPRINT="$(e2e_real_config_fingerprint)"
export E2E_REAL_CONFIG_FINGERPRINT

# Called by every run-all.sh before it reports its own verdict. A run that
# changed the real config directory fails whatever its assertions said.
assert_real_config_dir_unchanged() {
    local now
    now="$(e2e_real_config_fingerprint)"
    if [ "$now" = "$E2E_REAL_CONFIG_FINGERPRINT" ]; then
        return 0
    fi
    echo "FATAL: the real config directory changed during this run" >&2
    echo "  $E2E_REAL_CONFIG_DIR" >&2
    echo "  before: $E2E_REAL_CONFIG_FINGERPRINT" >&2
    echo "  after:  $now" >&2
    return 1
}
