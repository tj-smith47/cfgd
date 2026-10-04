# shellcheck shell=bash
# Each line below names sleep without running it here: a heredoc body, a
# quoted word, a word after another command and this comment (sleep 9).
kubectl apply -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: probe
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
EOF
cat > stub.sh <<'STUB'
#!/bin/sh
sleep 60
STUB
echo "sleep 3 is not run"
printf '%s\n' sleep
grep -c sleep stub.sh
grep -c 'sleep 1' stub.sh
