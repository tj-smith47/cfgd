kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: unlabelled
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: unlabelled
EOF
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: hand-spelled
  labels:
    cfgd.io/e2e-run: "42"
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: hand-spelled
EOF
