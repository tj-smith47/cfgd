#!/usr/bin/env bash
# Pre-tool-use hook: block dangerous bash commands
set -euo pipefail

INPUT=$(cat)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)

[[ -z "$COMMAND" ]] && exit 0

# Block destructive filesystem operations EXCEPT inside the three directories the
# agent owns: this repo's target/ (cargo artifacts are reproducible), the
# session scratch tree ~/.cache/cfgd-debug/ (seat logs, probe copy trees) and
# the gitignored SDD plan workspaces under .superpowers/sdd/. Every argument
# must sit at least one component below one of those roots, so `rm -rf ..`,
# `rm -rf /`, `rm -rf $HOME`, `rm -rf ~/.cache` and `rm -rf .superpowers` fail.
if grep -qE '^rm -rf' <<<"$COMMAND"; then
    # shellcheck disable=SC2016  # a regex; the $HOME spelling is matched literally
    owned='(target/|/opt/repos/cfgd/target/|~/\.cache/cfgd-debug/|\$HOME/\.cache/cfgd-debug/|/root/\.cache/cfgd-debug/|\.superpowers/sdd/|/opt/repos/cfgd/\.superpowers/sdd/)[^[:space:]]+'
    if grep -qE "^rm -rf( ${owned})+[[:space:]]*\$" <<<"$COMMAND"; then
        exit 0
    fi
    echo "Blocked: destructive operation. Use targeted deletes instead." >&2
    echo "  (rm -rf is allowed below target/, ~/.cache/cfgd-debug/ or .superpowers/sdd/)" >&2
    exit 2
fi

# Block force git operations that can destroy history or working state
if grep -qE 'git push.*--force|git reset --hard' <<<"$COMMAND"; then
    echo "Blocked: force operation requires explicit user approval." >&2
    exit 2
fi

# Block git stash — commit or build on existing changes instead
if grep -qE 'git stash' <<<"$COMMAND"; then
    echo "Blocked: git stash is prohibited. Commit your changes or continue building on them." >&2
    exit 2
fi

# Block commands that silently discard all working-tree changes
if grep -qE 'git checkout -- \.|git restore \.( |$)' <<<"$COMMAND"; then
    echo "Blocked: discarding all working-tree changes requires explicit user approval." >&2
    exit 2
fi

exit 0
