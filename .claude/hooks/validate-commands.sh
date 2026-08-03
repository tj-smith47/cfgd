#!/usr/bin/env bash
# Pre-tool-use hook: block dangerous bash commands
set -euo pipefail

INPUT=$(cat)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)

[[ -z "$COMMAND" ]] && exit 0

# Block destructive filesystem operations EXCEPT when targeted at this repo's
# target/ build dir — cargo build artifacts are reproducible, so allowing
# `rm -rf target/...` keeps cleanup ergonomic. The target-path check requires
# `target/` (or its absolute form) as the argument's prefix, so `rm -rf ..`,
# `rm -rf /`, `rm -rf $HOME` still fail.
if echo "$COMMAND" | grep -qE '^rm -rf|cargo clean.*--release'; then
    if echo "$COMMAND" | grep -qE '^rm -rf (target/|/opt/repos/cfgd/target/)[^[:space:]]+[[:space:]]*$'; then
        exit 0
    fi
    echo "Blocked: destructive operation. Use targeted deletes instead." >&2
    echo "  (rm -rf is allowed when the path starts with target/ or /opt/repos/cfgd/target/)" >&2
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
