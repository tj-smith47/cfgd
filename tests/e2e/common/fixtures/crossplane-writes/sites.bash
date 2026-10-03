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
