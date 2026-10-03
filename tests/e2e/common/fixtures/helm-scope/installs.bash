helm install cfgd-test "$CHART_DIR" "${HELM_SCOPE[@]}" --wait
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    "${HELM_SCOPE[@]}" \
    --wait
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --wait
OUT=$(helm upgrade cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" 2>&1) || true
if ! helm upgrade --install cfgd-test "$CHART_DIR"; then echo failed >&2; fi
true && helm install other "$CHART_DIR"
    helm   upgrade cfgd-test "$CHART_DIR" "${HELM_SCOPE[@]}"
# helm install --wait has already waited for the Deployment
fail_test "FS-X-01" "Operator is not Available after helm install --wait"
helm uninstall cfgd-test -n "$HELM_NS"
helm template cfgd-test "$CHART_DIR"
myhelm install cfgd-test "$CHART_DIR"
helm installed
helm install cfgd-test "$CHART_DIR" "${HELM_SCOPE[*]}"
helm install cfgd-test "$CHART_DIR" "${HELM_SCOPE[@]}" --set-string operator.watchLabelSelector=
helm upgrade cfgd-test "$CHART_DIR" \
    "${HELM_SCOPE[@]}" \
    --set-json "mutatingWebhook.namespaceSelector={}"
helm install cfgd-test "$CHART_DIR" --set webhook.objectSelector.a=b "${HELM_SCOPE[@]}"
helm install cfgd-test "$CHART_DIR" "${HELM_SCOPE[@]}" --set-json 'webhook={}'
helm install cfgd-test "$CHART_DIR" "${HELM_SCOPE[@]}" --set webhook.enabled=true --set mutatingWebhook.enabled=true
