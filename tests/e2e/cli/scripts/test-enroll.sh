#!/usr/bin/env bash
# E2E tests for: cfgd enroll
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd enroll tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "EN01: enroll --help"
run $C enroll --help
if assert_ok && assert_contains "$OUTPUT" "server"; then
    pass_test "EN01"
else fail_test "EN01"; fi

begin_test "EN02: enroll without server fails"
run $C enroll --server-url http://localhost:9999
if assert_fail; then
    pass_test "EN02"
else fail_test "EN02"; fi

begin_test "EN03: enroll --ssh-key flag accepted"
run $C enroll --server-url http://localhost:9999 --ssh-key ~/.ssh/id_ed25519
if assert_fail; then
    pass_test "EN03"
else fail_test "EN03"; fi

begin_test "EN04: enroll --gpg-key flag accepted"
run $C enroll --server-url http://localhost:9999 --gpg-key ABCD1234
if assert_fail; then
    pass_test "EN04"
else fail_test "EN04"; fi

begin_test "EN05: enroll --username flag"
run $C enroll --server-url http://localhost:9999 --username testuser
if assert_fail; then
    pass_test "EN05"
else fail_test "EN05"; fi

# SECTION 40: enroll additional flags

begin_test "EN06: enroll --token"
run $C enroll --server-url http://localhost:9999 --token test-bootstrap-token
if assert_fail; then
    pass_test "EN06"
else fail_test "EN06"; fi

print_summary "Enroll"
