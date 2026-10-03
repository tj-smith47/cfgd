kubectl get crd widgets.example.io -o json
kubectl wait --for=condition=established crd/widgets.example.io --timeout=30s
printf '%s\n' "$CRD_YAML" | kubectl annotate --local -o json -f - cfgd.io/e2e-unset-
kubectl apply --dry-run=client -f schemas/crds.yaml
helm upgrade --install demo ./chart --skip-crds
helm upgrade demo ./chart
helm template demo ./chart --include-crds
fail_test "X" "helm install demo failed: kubectl delete crd left it behind"
# kubectl delete crd widgets.example.io
kubectl apply -f app.yaml
kubectl apply -f - <<EOF
apiVersion: v1
kind: ConfigMap
# kind: CustomResourceDefinition
data:
  note: "kind: CustomResourceDefinition"
  json: '{"note": "kind: CustomResourceDefinition"}'
EOF
