HELM_SCOPE=(
    --set-string "operator.watchLabelSelector=cfgd.io/e2e-helm=${HELM_NS}"
    --set-json "webhook.objectSelector={\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}"
)
