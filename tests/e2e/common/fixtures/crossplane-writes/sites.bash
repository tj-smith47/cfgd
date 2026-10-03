kubectl apply -f "$CROSSPLANE_DIR/xrd-teamconfig.yaml"
kubectl apply -f "$REPO_ROOT/manifests/crossplane/composition.yaml"
kubectl get function function-cfgd -o yaml
kubectl rollout restart deployment -l pkg.crossplane.io/function=function-cfgd
crossplane xpkg push "$(e2e_image_repo function-cfgd):latest" -f "$XPKG_OUT"
# kubectl apply -f manifests/crossplane/composition.yaml
kubectl get function "$E2E_FUNCTION" -o yaml
kubectl annotate --local -o json -f "$REPO_ROOT/manifests/crossplane/xrd-teamconfig.yaml" cfgd.io/e2e-unset-
FUNC_REPO="$(e2e_image_repo function-cfgd)"
kubectl get deploymentruntimeconfig function-cfgd-runtime
kubectl delete composition teamconfig-to-machineconfigs
kubectl patch xrd teamconfigs.cfgd.io --type merge -p '{}'
kubectl apply -f "$CROSSPLANE_DIR"
kubectl apply -f manifests/crossplane/
kubectl apply -f - <<EOF
apiVersion: pkg.crossplane.io/v1beta1
kind: Function
metadata:
  name: function-cfgd
spec:
  package: $(e2e_image function-cfgd)
EOF
cat > "$dir/composition.yaml" <<'EOF'
apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: teamconfig-to-machineconfigs
EOF
kubectl apply -f - <<EOF
apiVersion: apiextensions.crossplane.io/v2
kind: CompositeResourceDefinition
metadata:
  name: teamconfigs.cfgd.io
EOF
kubectl apply -f - <<EOF
apiVersion: pkg.crossplane.io/v1beta1
kind: DeploymentRuntimeConfig
metadata:
  name: function-cfgd-runtime
EOF
kubectl apply -f - <<EOF
apiVersion: pkg.crossplane.io/v1beta1
kind: Function
metadata:
  name: ${E2E_FUNCTION}
spec:
  runtimeConfigRef:
    name: function-cfgd-runtime
EOF
kubectl get xrd teamconfigs.cfgd.io -o json
