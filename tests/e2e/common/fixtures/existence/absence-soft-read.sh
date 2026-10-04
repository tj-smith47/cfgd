# shellcheck shell=bash
DS=$(kubectl get ds -o name 2>/dev/null || echo "")
if [ -z "$DS" ]; then
    pass_test "E-40" # want-absent
fi
MOUNTS=$(exec_in_pod mount 2>/dev/null | grep cfgd || echo "")
if echo "$MOUNTS" | grep -q pod-a; then
    fail_test "E-41" "mount left"
else
    pass_test "E-41" # want-absent
fi
ENV=$(kubectl get deploy -o jsonpath='{.items[0].spec}' \
    2>/dev/null || echo '')
if [ "$ENV" = "" ]; then
    pass_test "E-42" # want-absent
fi
if ! echo "$DS" | grep -q cfgd; then
    pass_test "E-43" # want-absent
fi
[ -z "$DS" ] && pass_test "E-44" # want-absent
PHASE=$(kubectl get pod p -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
if [ "$PHASE" != "Failed" ]; then
    pass_test "E-48" # want-absent
fi
REASON=$(kubectl get pod p -o jsonpath='{.status.reason}' 2>/dev/null || echo "")
if [ "$REASON" = "CrashLoopBackOff" ]; then
    fail_test "E-49" "crash loop"
else
    pass_test "E-49" # want-absent
fi
LEFT=$(kubectl get ds -o name 2>/dev/null || echo "")
if [ "$LEFT" != "" ]; then
    fail_test "E-53" "left: $LEFT"
else
    pass_test "E-53" # want-absent
fi
