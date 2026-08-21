# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# that a module can be attached to a pod that is already running, so the
# before and the after both have to be watchable.
Output "demo/.out/connect.mp4"

# Records on the HOST against the same throwaway kind cluster k8s.tape uses
# (`demo/scripts/setup-k8s-demo.sh up`), for the same reason: the beats are
# kubectl against a live cluster.
Require kubectl

Set Shell "bash"
# Same font as every other per-surface tape. No Nerd Font glyphs are on screen
# here, but cfgd's own output (icons, rules) shares the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Hidden setup: the demo cluster's own kubeconfig and the freshly built
# binaries, then the starting point this tape opens on — the nettools artifact
# in the registry, its Module CRD, and a pod that asked for it and is already
# running. Publishing a module is k8s.tape's claim, not this one's, so it
# happens off camera here.
Hide
Type "export KUBECONFIG=$HOME/.cache/cfgd-debug/demo-k8s/kubeconfig PATH=$HOME/.cache/cfgd-debug/demo-k8s/bin:$PATH"
Enter
Sleep 500ms
Type "cd $HOME/.cache/cfgd-debug/demo-k8s/fixture"
Enter
Sleep 500ms
# The prompt the other tapes get from the demo container's own `tj` account on
# a `--hostname jarvispro` container. Set explicitly here because this shell is
# the recording host's, and every `Wait` below keys off the `demo$` it ends in.
Type "export PS1='tj@jarvispro:~/demo$ '"
Enter
Wait@30s /demo\$$/
Type "cfgd module push ./nettools --artifact localhost:5001/demo/nettools:v1"
Enter
Wait@2m /demo\$$/
Type "kubectl apply -f debug-module.yaml && kubectl apply -f app-pod.yaml"
Enter
Wait@2m /demo\$$/
Type "kubectl wait --for=condition=Ready pod/app --timeout=180s"
Enter
Wait@5m /demo\$$/
Type "clear"
Enter
Sleep 500ms
Show

# Beat 1: the policy that makes the rest of this tape possible. `mountPolicy:
# Debug` stages the module's CSI volume on any pod that asks for it and mounts
# it into none of that pod's own containers — the module is on the node, ready,
# and invisible to the application.
Type "cat debug-module.yaml"
Enter
Wait@30s /demo\$$/
Sleep 5s

# Beat 2: the "not in the container" half, proven rather than asserted. The pod
# carries the annotation and is running, and its own container still has no
# /cfgd-modules at all.
Type "kubectl exec app -- ls /cfgd-modules"
Enter
Wait@60s /demo\$$/
Sleep 5s

# Beat 3: attach it. One command adds an ephemeral container to the pod that is
# already running, mounts the staged module into it, and puts the module's bin
# directory on that container's PATH.
Type "kubectl cfgd debug app --module nettools:v1 --image busybox:1.36 --namespace demo"
Enter
Wait@60s /demo\$$/
Sleep 5s

# Beat 4: the shell, taken with the command the previous beat printed. The
# prompt is the one cfgd set on the ephemeral container, so it names the module
# that is mounted in the shell being typed into.
Type "kubectl attach -n demo app -c cfgd-debug -it"
Enter
Sleep 3s
# kubectl prints "If you don't see a command prompt, try pressing enter."
Enter
Sleep 2s

# Beat 5: the payoff. `netcheck.sh` is the module's own file, reached by name
# off the PATH cfgd set, and it reports the address and resolver of the pod it
# is running beside — so it is genuinely inside that pod's network namespace
# and not somewhere else.
Type "netcheck.sh"
Enter
Sleep 6s

# Leaving the shell ends the ephemeral container with it.
Type "exit"
Enter
Wait@60s /demo\$$/
Sleep 4s

# Beat 6: the boundary held. The application's own container never gained the
# module — the mount was scoped to the debug shell, which has now gone.
Type "kubectl exec app -- ls /cfgd-modules"
Enter
Wait@60s /demo\$$/
Sleep 5s

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching every other per-surface tape's closing beat.
Sleep 8s
