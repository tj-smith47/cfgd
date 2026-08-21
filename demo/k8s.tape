# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is a
# single chain (module → registry → cluster → pod), and every link in it has to
# be watchable or the chain is not what was shown.
Output "demo/.out/k8s.mp4"

# This is the only tape that records on the HOST rather than inside the demo
# container: the beats are kubectl against a live cluster, and the kind
# kubeconfig, the kubectl binary and the freshly built cfgd all live here.
# `demo/scripts/setup-k8s-demo.sh up` has already created the cluster,
# installed the chart and written the fixture before vhs starts.
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

# Hidden setup: point this shell at the demo cluster's own kubeconfig and at
# the freshly built binaries, then move into the fixture directory. The
# KUBECONFIG export is what keeps every beat below off the recording host's
# real cluster — nothing in this tape ever reads the default kubeconfig. The
# PATH export is what makes `cfgd` and `kubectl cfgd` mean the tree that was
# just built rather than whatever release happens to be installed.
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
Type "clear"
Enter
Sleep 500ms
Show

# Beat 1: what is about to be shipped. Both files of the module fit on screen
# at head's default 10 lines, and the `==>` headers name each one — so the
# artifact the next beat pushes, the CRD after it and the file that finally
# runs inside the pod are all visibly the same two files.
Type "head tools/module.yaml tools/bin/hello.sh"
Enter
Wait@30s /demo\$$/
Sleep 5s

# Beat 2: the module leaves the workstation. `./tools` is an ordinary module
# directory (module.yaml + bin/hello.sh); push turns it into an OCI artifact in
# the cluster's registry and prints the digest it landed under. Pushed
# UNSIGNED on purpose — a keyless cosign signature needs Fulcio and an OIDC
# identity, which is a story about Sigstore rather than about module delivery.
Type "cfgd module push ./tools --artifact localhost:5001/demo/tools:v1"
Enter
Wait@60s /demo\$$/
Sleep 4s

# Beat 3: the module becomes a cluster object. The Module CRD is cluster-scoped
# and carries the artifact reference under the name the CLUSTER reaches the
# registry by, which is why the pushed reference and the one printed here name
# the same registry through two different addresses.
Type "kubectl apply -f module.yaml"
Enter
Wait@60s /demo\$$/
Sleep 2s
Type "kubectl get modules"
Enter
Wait@30s /demo\$$/
Sleep 5s

# `clear` only — VHS's `Hide` pauses recording, it does not erase the
# terminal, so without this the pod half of the demo would open underneath the
# module half and scroll the payoff off the top.
Hide
Type "clear"
Enter
Sleep 500ms
Show

# Beat 4: the ask. The whole request is one annotation on an otherwise
# unremarkable busybox pod — no volume, no volumeMount, no init container.
Type "cat pod.yaml"
Enter
Wait@30s /demo\$$/
Sleep 5s
Type "kubectl apply -f pod.yaml"
Enter
Wait@60s /demo\$$/
Sleep 2s

# The mutating webhook rewrites the pod on admission and the CSI driver pulls
# the artifact and bind-mounts it before the container starts, so `Ready` here
# is the whole injection path having already succeeded.
Type "kubectl wait --for=condition=Ready pod/demo-pod --timeout=180s"
Enter
Wait@5m /demo\$$/
Sleep 4s

# Beat 5: the payoff. The module's file is inside the pod, at
# /cfgd-modules/<module>, and it runs.
Type "kubectl exec demo-pod -- sh /cfgd-modules/tools/bin/hello.sh"
Enter
Wait@60s /demo\$$/
Sleep 5s

# Beat 6: the fleet view. `kubectl cfgd` is cfgd's own kubectl plugin (the same
# binary, reached through a `kubectl-cfgd` name on PATH). It reports the
# module as unverified because this one really was pushed unsigned.
Type "kubectl cfgd status"
Enter
Wait@60s /demo\$$/
Sleep 4s

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching every other per-surface tape's closing beat.
Sleep 8s
