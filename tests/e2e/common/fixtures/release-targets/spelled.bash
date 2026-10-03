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
kubectl logs deploy/cfgd-operator
kubectl get deployments/cfgd-operator
kubectl get deployment.apps/cfgd-operator
kubectl get deployments.apps cfgd-operator
kubectl get deployment "cfgd-operator"
kubectl get ep cfgd-operator
kubectl get endpoints/cfgd-operator
kubectl get services cfgd-operator
kubectl get service/cfgd-operator
kubectl get svc 'cfgd-operator'
kubectl get pods -l app.kubernetes.io/name=cfgd-operator
kubectl get pods -l "app=cfgd-operator"
echo "the release namespace is cfgd-system."
