HELM_SCOPE=(
    --set-string "operator.watchLabelSelector=cfgd.io/e2e-run=${HELM_NS}"
    --set-json "webhook.objectSelector={\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}"
    --set-json "mutatingWebhook.namespaceSelector={\"matchExpressions\":null,\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}"
)
