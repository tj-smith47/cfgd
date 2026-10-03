kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: without
spec:
  team: without
EOF
