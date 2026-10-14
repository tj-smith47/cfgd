#!/usr/bin/env bash
# Pre-tool-use hook: block dangerous bash commands
set -euo pipefail

INPUT=$(cat)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)

[[ -z "$COMMAND" ]] && exit 0

# Block destructive filesystem operations EXCEPT inside the two directories the
# agent owns: this repo's target/ (cargo artifacts are reproducible) and the
# session scratch tree ~/.cache/cfgd-debug/ (seat logs, probe copy trees). Every
# argument must sit at least one component below one of those roots, so
# `rm -rf ..`, `rm -rf /`, `rm -rf $HOME` and `rm -rf ~/.cache` still fail.
if echo "$COMMAND" | grep -qE '^rm -rf'; then
    # shellcheck disable=SC2016  # a regex; the $HOME spelling is matched literally
    owned='(target/|/opt/repos/cfgd/target/|~/\.cache/cfgd-debug/|\$HOME/\.cache/cfgd-debug/|/root/\.cache/cfgd-debug/)[^[:space:]]+'
    if echo "$COMMAND" | grep -qE "^rm -rf( ${owned})+[[:space:]]*\$"; then
        exit 0
    fi
    echo "Blocked: destructive operation. Use targeted deletes instead." >&2
    echo "  (rm -rf is allowed below target/, /opt/repos/cfgd/target/ or ~/.cache/cfgd-debug/)" >&2
    exit 2
fi

# Block force git operations that can destroy history or working state
if echo "$COMMAND" | grep -qE 'git push.*--force|git reset --hard'; then
    echo "Blocked: force operation requires explicit user approval." >&2
    exit 2
fi

# Block git stash — commit or build on existing changes instead
if echo "$COMMAND" | grep -qE 'git stash'; then
    echo "Blocked: git stash is prohibited. Commit your changes or continue building on them." >&2
    exit 2
fi

# Block commands that silently discard all working-tree changes
if echo "$COMMAND" | grep -qE 'git checkout -- \.|git restore \.( |$)'; then
    echo "Blocked: discarding all working-tree changes requires explicit user approval." >&2
    exit 2
fi

exit 0
