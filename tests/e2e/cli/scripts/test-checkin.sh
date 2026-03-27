#!/usr/bin/env bash
# E2E tests for: cfgd checkin
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd checkin tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "CI01: checkin --help"
run $C checkin --help
if assert_ok && assert_contains "$OUTPUT" "server-url"; then
    pass_test "CI01"
else fail_test "CI01"; fi

begin_test "CI02: checkin without server fails"
run $C checkin --server-url http://localhost:9999
if assert_fail; then
    pass_test "CI02"
else fail_test "CI02"; fi

# SECTION 40: checkin additional flags

begin_test "CI03: checkin --api-key"
run $C checkin --server-url http://localhost:9999 --api-key test-key
if assert_fail; then
    pass_test "CI03"
else fail_test "CI03"; fi

begin_test "CI04: checkin --device-id"
run $C checkin --server-url http://localhost:9999 --device-id test-device
if assert_fail; then
    pass_test "CI04"
else fail_test "CI04"; fi

begin_test "CI05: checkin --api-key --device-id"
run $C checkin --server-url http://localhost:9999 --api-key k --device-id d
if assert_fail; then
    pass_test "CI05"
else fail_test "CI05"; fi

print_summary "Checkin"
