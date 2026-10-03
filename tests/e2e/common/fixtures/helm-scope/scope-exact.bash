# HELM_SCOPE=( in a comment is no definition
helm_test_ns() {
    HELM_SCOPE=(
        --set-string "operator.watchLabelSelector=cfgd.io/e2e-helm=${HELM_NS}"
        --set-json "webhook.objectSelector={\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}"
        --set-json "mutatingWebhook.namespaceSelector={\"matchExpressions\":null,\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}"
    )
    HELM_SCOPE+=(--set-string operator.watchLabelSelector=)
    HELM_SCOPE=(
        --set-string "operator.watchLabelSelector=cfgd.io/e2e-helm=${HELM_NS}"
    )
    HELM_SCOPE[3]="--set-string"
    unset HELM_SCOPE
    echo "${HELM_SCOPE[@]}"
}
