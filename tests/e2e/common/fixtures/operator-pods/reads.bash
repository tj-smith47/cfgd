kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
OLD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
kubectl get pod -l "${E2E_OPERATOR_PODS}" -o json | jq -r '.items[1].metadata.uid'
kubectl get po -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}'
kubectl get pods -n "$NS" -l app.kubernetes.io/component=operator \
    --sort-by=.metadata.creationTimestamp \
    -o jsonpath='{.items[-1].metadata.name}'
# kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[*].metadata.uid}'
kubectl get deployment -n "$NS" -l app.kubernetes.io/component=operator -o jsonpath='{.items[0].spec.replicas}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" -o jsonpath='{.items[0].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o name
echo '{.items[0]}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" --sort-by=.metadata.creationTimestamp -o jsonpath='{.items[-1:].metadata.name}'
