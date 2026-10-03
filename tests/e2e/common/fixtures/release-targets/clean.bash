kubectl get lease cfgd-operator-leader -n "$E2E_INSTALL_NS"
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS"
kubectl logs -n "$E2E_INSTALL_NS" deployment/"$E2E_OPERATOR_DEPLOY"
kubectl get endpoints "$E2E_WEBHOOK_SVC" -n "$E2E_INSTALL_NS"
echo "$CFGD_NAMESPACE_HINT"
echo "expected driver=$CSI_DRIVER_NAME"
echo inject-modules.cfgd.io validate-module.cfgd.io
