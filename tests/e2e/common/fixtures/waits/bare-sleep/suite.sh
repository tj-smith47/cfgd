# shellcheck shell=bash
# A suite script that pads a step with a fixed sleep.
kubectl apply -f manifest.yaml
sleep 5
kubectl get pod probe
