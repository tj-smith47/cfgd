kubectl delete crd widgets.example.io
kubectl -n e2e patch crd/widgets.example.io --type merge -p '{}'
echo "$CRD_YAML" | kubectl apply -f -
"$REPO_ROOT/target/release/cfgd-gen-crds" \
    | kubectl replace -f -
kubectl apply -f schemas/crds.yaml
helm install demo ./chart
helm upgrade -i demo ./chart
helm upgrade --install demo ./chart \
    --namespace demo
kubectl apply -f - <<EOF
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
metadata:
  name: widgets.example.io
EOF
apply_yaml <<EOF
apiVersion: v1
kind: ConfigMap
---
apiVersion: apiextensions.k8s.io/v1
kind: CustomResourceDefinition
EOF
