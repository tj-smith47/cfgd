SERVER_URL="http://$E2E_GATEWAY_SVC.$E2E_INSTALL_NS.svc.cluster.local:8080"
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_INSTALL_RELEASE-gateway" 60
port_forward "$E2E_INSTALL_NS" "svc/${E2E_INSTALL_RELEASE}-gateway" 18080 8080
kubectl get pods -n "$E2E_INSTALL_NS" -l cfgd.io/gateway-leader=true
# reach it at http://x.y.svc.cluster.local
