kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: with
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: with
EOF
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: with-bare
  namespace: $NS
spec:
  crossplane:
    compositionRef:
      name: $E2E_COMPOSITION
  team: with-bare
EOF
