#!/usr/bin/env bash
# E2E tests for: cfgd mcp-server
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd mcp-server tests ==="

# Helper: send JSON-RPC messages to mcp-server and capture output.
# Uses timeout to prevent hanging if server doesn't respond.
mcp_send() {
    local input="$1"
    local rc=0
    local out
    out=$(echo "$input" | timeout 10 "$CFGD" $C mcp-server 2>/dev/null) || rc=$?
    # timeout exits 124 when it kills the process; the server exits 0 on EOF.
    # Both are acceptable — we care about the stdout content.
    if [ "$rc" -ne 0 ] && [ "$rc" -ne 124 ]; then
        MCP_OUTPUT=""
        MCP_RC=$rc
        return
    fi
    MCP_OUTPUT="$out"
    MCP_RC=0
}

INIT_REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e-test","version":"1.0"}}}'

# --- MCP01: mcp-server --help ---

begin_test "MCP01: mcp-server --help"
run $C mcp-server --help
if assert_ok && assert_contains "$OUTPUT" "MCP"; then
    pass_test "MCP01"
else fail_test "MCP01"; fi

# --- MCP02: MCP server initialize ---

begin_test "MCP02: MCP server initialize"
mcp_send "$INIT_REQ"
if [ "$MCP_RC" -eq 0 ] \
    && echo "$MCP_OUTPUT" | grep -q '"protocolVersion"' \
    && echo "$MCP_OUTPUT" | grep -q '"serverInfo"' \
    && echo "$MCP_OUTPUT" | grep -q '"cfgd"'; then
    pass_test "MCP02"
else
    fail_test "MCP02" "initialize response missing expected fields: $MCP_OUTPUT"
fi

# --- MCP03: MCP server tools/list ---

begin_test "MCP03: MCP server tools/list"
TOOLS_REQ='{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
mcp_send "${INIT_REQ}
${TOOLS_REQ}"
if [ "$MCP_RC" -eq 0 ] \
    && echo "$MCP_OUTPUT" | grep -q '"tools"' \
    && echo "$MCP_OUTPUT" | grep -q 'cfgd_'; then
    pass_test "MCP03"
else
    fail_test "MCP03" "tools/list response missing expected fields: $MCP_OUTPUT"
fi

# --- MCP04: MCP server resources/list ---

begin_test "MCP04: MCP server resources/list"
RES_REQ='{"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}'
mcp_send "${INIT_REQ}
${RES_REQ}"
if [ "$MCP_RC" -eq 0 ] \
    && echo "$MCP_OUTPUT" | grep -q '"resources"' \
    && echo "$MCP_OUTPUT" | grep -q 'cfgd://'; then
    pass_test "MCP04"
else
    fail_test "MCP04" "resources/list response missing expected fields: $MCP_OUTPUT"
fi

# --- MCP05: MCP server prompts/list ---

begin_test "MCP05: MCP server prompts/list"
PROMPTS_REQ='{"jsonrpc":"2.0","id":2,"method":"prompts/list","params":{}}'
mcp_send "${INIT_REQ}
${PROMPTS_REQ}"
if [ "$MCP_RC" -eq 0 ] \
    && echo "$MCP_OUTPUT" | grep -q '"prompts"' \
    && echo "$MCP_OUTPUT" | grep -q 'cfgd_generate'; then
    pass_test "MCP05"
else
    fail_test "MCP05" "prompts/list response missing expected fields: $MCP_OUTPUT"
fi

# --- MCP06: MCP server invalid request ---

begin_test "MCP06: MCP server invalid request"
BAD_REQ='{"not valid json at all'
mcp_send "${INIT_REQ}
${BAD_REQ}"
if echo "$MCP_OUTPUT" | grep -q '"Parse error"' \
    || echo "$MCP_OUTPUT" | grep -q -- '-32700'; then
    pass_test "MCP06"
else
    fail_test "MCP06" "server did not return parse error (-32700)"
fi

print_summary "MCP Server"
