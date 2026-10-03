kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: other-name
spec:
  crossplane:
    compositionRef:
      name: teamconfig-to-machineconfigs
  team: other-name
EOF
kubectl apply -f - <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: quoted
spec:
  crossplane:
    compositionRef:
      name: $E2E_COMPOSITION
  team: quoted
EOF
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: top-level-ref
spec:
  compositionRef:
    name: ${E2E_COMPOSITION}
  team: top-level-ref
EOF
