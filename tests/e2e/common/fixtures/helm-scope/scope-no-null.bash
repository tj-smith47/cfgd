HELM_SCOPE=(--set-json "mutatingWebhook.namespaceSelector={\"matchLabels\":{\"cfgd.io/e2e-helm\":\"${HELM_NS}\"}}")
