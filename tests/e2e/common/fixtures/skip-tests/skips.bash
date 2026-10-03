if [ -z "$TOOL" ]; then
    skip_test "FS-X-01" "tool missing"
fi
skip_tests_seen=0
[ -n "$READY" ] || skip_test "FS-X-02" "not ready"
echo "no case here skips"
