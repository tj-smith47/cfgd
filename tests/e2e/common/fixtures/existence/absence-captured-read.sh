# shellcheck shell=bash
DS_RC=0
DS=$(kubectl get ds -o name 2>/dev/null) || DS_RC=$?
if [ "$DS_RC" -ne 0 ]; then
    fail_test "E-45" "kubectl get failed (rc=$DS_RC)"
elif [ -z "$DS" ]; then
    pass_test "E-45"
fi
OUT=$(kubectl get ds -o name 2>/dev/null || echo "")
OUT=$(kubectl get ds -o name)
if [ -z "$OUT" ]; then
    pass_test "E-46"
fi
LIST=$(kubectl get ds -o name 2>/dev/null || echo "")
if [ "$LIST" = "ds/a" ]; then
    pass_test "E-47"
else
    fail_test "E-47" "got $LIST"
fi
TYPE=$(jq -r type <<<"$BODY" 2>/dev/null || echo "")
if [ "$TYPE" != "array" ]; then
    fail_test "E-50" "got $TYPE"
else
    pass_test "E-50"
fi
COUNT=$(jq length <<<"$BODY" 2>/dev/null || echo "0")
if [ "$COUNT" -lt 1 ]; then
    fail_test "E-51" "empty"
else
    pass_test "E-51"
fi
SEEN=$(kubectl get ds -o name 2>/dev/null || echo "")
GONE_RC=0
GONE=$(kubectl get ds -o name 2>/dev/null) || GONE_RC=$?
if [ -n "$SEEN" ] && [ "$GONE_RC" -eq 0 ] && [ -z "$GONE" ]; then
    pass_test "E-52"
fi
