kubectl get pods -n cfgd-system
wait_for_deployment "$CFGD_NAMESPACE" x 120
kubectl get pods -n "${CFGD_NAMESPACE}"
kubectl get pods -l app=cfgd-operator
kubectl logs deployment/cfgd-operator
kubectl rollout status deployment cfgd-operator
kubectl get endpoints cfgd-operator -n "$E2E_INSTALL_NS"
port_forward "$E2E_INSTALL_NS" svc/cfgd-operator 18443 443
kubectl get validatingwebhookconfiguration cfgd-validating-webhooks
kubectl get mutatingwebhookconfiguration cfgd-mutating-webhooks
echo "expected driver=csi.cfgd.io"
# the release operator runs in cfgd-system
  namespace: cfgd-system
