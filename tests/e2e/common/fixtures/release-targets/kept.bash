kubectl get deployment cfgd-server -n cfgd-system
    kubectl get machineconfig mc-1 -n cfgd-system \
        -o name
kubectl get deployment cfgd-operator -n cfgd-system
echo "the release namespace is cfgd-system."
