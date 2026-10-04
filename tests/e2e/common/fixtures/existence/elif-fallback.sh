# shellcheck shell=bash
if [ "$X" = "True/DriftActive" ]; then
    pass_test "E-11"
elif kubectl get driftalert "da-$RUN" -o name >/dev/null 2>&1; then
    pass_test "E-12" # want-flag
elif k8s_exists module "m-$RUN"; then
    pass_test "E-13" # want-flag
elif [ -n "$Y" ]; then
    pass_test "E-14" # want-flag
elif kubectl get module "m-$RUN" -o yaml | grep -q 'verified: true'; then
    pass_test "E-15"
fi
