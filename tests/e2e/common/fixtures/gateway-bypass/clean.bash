SERVER_URL="$E2E_GATEWAY_URL"
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_GATEWAY_SVC" 60
port_forward "$E2E_INSTALL_NS" "svc/$E2E_GATEWAY_SVC" 18080 "$E2E_GATEWAY_PORT"
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_GATEWAY_LEADER_PODS"
kubectl get svc "$E2E_INSTALL_RELEASE-gateway-canary"
echo "the gateway serves on its own port"
