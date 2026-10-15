kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
OLD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
kubectl get pod -l "${E2E_OPERATOR_PODS}" -o json | jq -r '.items[1].metadata.uid'
kubectl get po -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}'
kubectl get pods -n "$NS" -l app.kubernetes.io/component=operator \
    --sort-by=.metadata.creationTimestamp \
    -o jsonpath='{.items[-1].metadata.name}'
# kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[*].metadata.uid}'
kubectl get deployment -n "$NS" -l app.kubernetes.io/component=operator -o jsonpath='{.items[0].spec.replicas}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" -o jsonpath='{.items[0].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o name
echo '{.items[0]}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" --sort-by=.metadata.creationTimestamp -o jsonpath='{.items[-1:].metadata.name}'
"$KUBECTL" get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0:1].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[-1:].metadata.name}'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o json | jq -r '.items | first | .metadata.name'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o json | jq -r '[.items[] | .metadata.name] | first'
J="$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o json 2>&1)" || exit 1
jq -r '.items | sort_by(.metadata.name)[] | .metadata.name' <<<"$J"
jq -r '.items[0].metadata.name' <<<"$J"
jq -r '.items[1:2][] | .metadata.uid' \
    <<<"$J"
jq -r '.items | first | .status.phase' <<<"$J"
OTHER="$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_CSI_PODS" -o json)"
jq -r '.items[0].metadata.name' <<<"$OTHER"
echo "${J}" | jq -r '.items[0].metadata.name'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o json > "$SCRATCH/op.json"
jq -r '.items[0].metadata.name' "$SCRATCH/other.json"
jq -r '.items[0].metadata.name' "$SCRATCH/op.json"
jq -r '.items | sort_by(.metadata.creationTimestamp) | first | .metadata.name' <<<"$J"
echo "$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}' | tr -d "'")"
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
POD="$(printf '%s' "$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o json |
    jq -r '.items[0].metadata.name')")"
cat <<'EOF'
kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'
EOF
Q=$(kubectl get pods -l app=cfgd-operator -o json || true)
jq -r '.items[0].metadata.name' <<<"$Q"
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | { jq -r '.items[0].metadata.name'; }
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | (jq -r '.items[0].metadata.name')
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | while read -r l; do echo "$l"; done
f() { kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].metadata.name}'; }
if x=$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | jq -r '.items[0].x'); then :; fi
for i in 1; do kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}' && echo; done
local K="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json 2>/dev/null)"
jq '.items[0]' <<<"$K"
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json > "${SCRATCH}/a.json" || true
jq '.items[0]' "$SCRATCH/a.json"
P=$(echo "${X:-$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}')}")
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json |& jq '.items[0]'
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | jq '[.items[]] | sort_by(.x) | .[0]'
mapfile -t L < <(kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}')
R=$(kubectl get pods -n "$NS" -l "$E2E_OPERATOR_PODS" -o json 2>/dev/null || echo "")
jq -r '.items[0].metadata.name' <<<"$R"
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json &> "$SCRATCH/b.json"
jq '.items[0]' "$SCRATCH/b.json"
{ kubectl get pods -l "$E2E_OPERATOR_PODS" -o json; } | jq '.items[0]'
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | if jq -e '.items[0]' >/dev/null; then :; fi
H=$( (kubectl get pods -l "$E2E_OPERATOR_PODS" -o json) )
jq '.items[0]' <<<"$H"
B=$( { kubectl get pods -l "$E2E_OPERATOR_PODS" -o json; } )
jq '.items[0]' <<<"$B"
echo x | while read -r l; do
    kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}'
done
C=$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | jq '.')
jq '.items[0]' <<<"$C"
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | {
    jq '.items[0]'
}
kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}'; kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[1].x}'
kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}' | while read -r l; do
    kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[1].x}'
done
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | jq '.items[0]' | while read -r l; do
    echo "$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[1].x}')"
done
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json | {
    jq '.items[0]'
    kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[1].x}'
}
echo x | while read -r l; do
    kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[0].x}'
    jq '.items[1]' <<<"$deployments"
done
# an outer whose first stage picks from an operator capture, holding a picking kubectl: both named
pods="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json)"
jq '.items[0]' <<<"$pods" | while read -r l; do
    kubectl get pods -l "$E2E_OPERATOR_PODS" -o jsonpath='{.items[1].x}'
done
# an outer reading one operator capture, holding a jq that reads another: both named
pods="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json)"
other="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json)"
jq '.items[0]' <<<"$pods" | while read -r l; do
    jq '.items[1]' <<<"$other"
done
# the second shape with file captures: both named
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json > a.json
kubectl get pods -l "$E2E_OPERATOR_PODS" -o json > b.json
jq '.items[0]' a.json | while read -r l; do
    jq '.items[1]' b.json
done
# a jq variable spelled like a capture is no read of it: quiet
pods="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json)"
jq --argjson pods '[1]' '.items[0] | $pods' deployments.json
# a capture read only inside a $(...) or <(...) the picking command holds: quiet
pods="$(kubectl get pods -l "$E2E_OPERATOR_PODS" -o json)"
jq --arg n "$(jq -r '.items | length' <<<"$pods")" '.items[0]' deployments.json
jq '.items[0]' deployments.json <(jq -r '.items | length' <<<"$pods")
