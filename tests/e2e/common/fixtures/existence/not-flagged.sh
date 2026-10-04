# shellcheck shell=bash
if [ -z "$X" ]; then
    fail_test "E-24" "empty"
else
    pass_test "E-24"
fi
if [ -n "$ID" ]; then
    pass_test "E-25" # verdict-ok: the id is the whole reply the endpoint documents
fi
if echo "$BODY" | jq -e '.method == "token"' >/dev/null; then
    pass_test "E-26"
fi
cat <<EOF
if [ -n "\$X" ]; then
    pass_test "E-27"
fi
EOF
if [ -n "$ERR" ]; then
    fail_test "E-30" "$ERR"
else
    pass_test "E-30"
fi
if kubectl get crd modules.cfgd.io -o name >/dev/null 2>&1; then
    pass_test "E-31"
fi
