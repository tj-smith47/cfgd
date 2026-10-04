# End-to-end suites

The suites under `tests/e2e/` run against a real Kubernetes cluster. `task e2e:setup`
(`setup-cluster.sh`) builds and pushes the images, then deploys what the suites need;
each `task e2e:<suite>` target runs one suite. `task e2e:pr-install:down`
(`pr-install-down.sh`) removes what setup installed for the run, and `task e2e` runs it
last whatever setup's and the suites' result. The full-stack suite also needs `cosign`
on PATH, and its setup stops without it.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `REGISTRY` | required | Registry every image is pulled from and pushed to |
| `IMAGE_TAG` | `e2e-<short HEAD sha>` | Tag of every image that has no override below |
| `CFGD_IMAGE_TAG` | `IMAGE_TAG` | Tag of `cfgd` (agent, test pod) |
| `OPERATOR_IMAGE_TAG` | `IMAGE_TAG` | Tag of `cfgd-operator` (the PR install's operator; the device gateway where setup deploys it) |
| `CSI_IMAGE_TAG` | `IMAGE_TAG` | Tag of `cfgd-csi` (the PR install's CSI node plugin) |
| `FUNCTION_IMAGE_TAG` | `IMAGE_TAG` | Tag of `function-cfgd` (Crossplane function) |

A release tags each image at its own crate's version, so a released set needs the
per-image overrides. `function-cfgd` is tagged with the cfgd version behind a `v`
prefix, which the other three images do not carry. To run the suites against the
images one release published:

```bash
export REGISTRY=ghcr.io/tj-smith47
export CFGD_IMAGE_TAG=0.11.0 OPERATOR_IMAGE_TAG=0.9.0 CSI_IMAGE_TAG=0.7.2
export FUNCTION_IMAGE_TAG=v0.11.0
task e2e:operator
```

An overridden image is used as it is: setup never builds, pushes or retags it, and
stops with an error naming the reference when the registry does not hold it.

Every image reference the scripts compose comes from three functions in
`common/helpers.sh`:

```bash
e2e_image cfgd-csi        # ghcr.io/tj-smith47/cfgd-csi:0.7.2
e2e_image_repo cfgd-csi   # ghcr.io/tj-smith47/cfgd-csi
e2e_image_tag cfgd-csi    # 0.7.2
```

`task e2e:tags:check` runs `common/test-image-tags.sh`, which needs no cluster: it
resolves each image under every override and fails when a script under `tests/e2e/`
spells `IMAGE_TAG` or a first-party image reference itself.

## The PR install

Each run installs its own operator and CSI driver beside the live release, using
`manifests/pr-install-values.yaml`. Every name it uses comes from the run id in
`common/helpers.sh`. The run id is `GITHUB_RUN_ID` in CI and `local-<short HEAD sha>`
on a workstation, so the setup process and every suite process of one checkout
agree on it. For run 42:

```bash
E2E_INSTALL_RELEASE     # cfgd-e2e-42 (the Helm release)
E2E_INSTALL_NS          # cfgd-e2e-42-sys
E2E_OPERATOR_DEPLOY     # cfgd-e2e-42-operator
E2E_CSI_DS              # cfgd-e2e-42-csi
E2E_WEBHOOK_SVC         # cfgd-e2e-42-webhook
E2E_WEBHOOK_CERT        # cfgd-e2e-42-webhook-tls (the cert-manager Certificate)
E2E_VALIDATING_WEBHOOK  # cfgd-e2e-42
E2E_MUTATING_WEBHOOK    # cfgd-e2e-42-pod-injector
E2E_OPERATOR_PODS       # app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=operator
E2E_CSI_PODS            # app.kubernetes.io/instance=cfgd-e2e-42,app.kubernetes.io/component=csi-driver
CSI_DRIVER_NAME         # e2e.csi.cfgd.io
```

The values file leaves the CSI driver name out, and the install passes
`--set-string "csiDriver.name=$CSI_DRIVER_NAME"`, so `helpers.sh` is the only place
the name is spelled.

`ensure_namespace` labels each namespace it creates with `cfgd.io/e2e-run=<run id>`,
which the PR install's mutating webhook selects on, the heartbeat refreshes and the
janitor reaps by. It never labels `cfgd-system` or `$CFGD_NAMESPACE`. `running_image`
takes the namespace as an optional fourth argument (default `cfgd-system`), so
`running_image daemonset "$E2E_CSI_DS" cfgd-csi "$E2E_INSTALL_NS"` reads the PR
install's driver. `common/test-pr-install.sh` (run by `task e2e:tags:check`) checks
these names, the run id, both helpers and the values file with a stub `kubectl`, so it
needs no cluster.

`create_e2e_namespace`, which setup uses for `$E2E_INSTALL_NS` and each suite for its own
namespace, waits up to 120s for a namespace an earlier teardown of the same run id is
still deleting, and stops when it outlasts that.

`pr-install-down.sh` removes the install once every cluster suite has finished: CI's
`e2e-teardown` job runs it after all of them, and `task e2e` runs it last. It deletes
the run's MachineConfigs, ConfigPolicies, DriftAlerts, BackupPolicies,
ClusterConfigPolicies and Modules (`-l "$E2E_RUN_LABEL"`) while the run's operator
still clears their finalizers, then uninstalls `$E2E_INSTALL_RELEASE` and deletes
`$E2E_INSTALL_NS` without waiting. It then reads back that `$CSI_DRIVER_NAME` no longer
belongs to the install and that both webhook configurations are gone, and prints the
namespace's phase. A release that is not installed counts as removed. Every step runs
when one before it fails, and the script exits 1 naming each failed step.
`test-pr-install.sh` runs it against the stubs in `common/fixtures/pr-install-down/bin/`.

The operator suite runs against the PR install: its pods, Deployment, leader lease
(`cfgd-operator-leader` in `$E2E_INSTALL_NS`), webhook Service and CSI driver name all
come from the names above, and OP-PR-01 fails unless `$E2E_OPERATOR_DEPLOY` runs
`e2e_image cfgd-operator`. The full-stack suite runs against the PR install too: its
setup waits for `$E2E_OPERATOR_DEPLOY`, the endpoints of `$E2E_WEBHOOK_SVC` and
`$E2E_CSI_DS`, and stops when the CSI driver is not ready or `cosign` is missing. No
full-stack case calls `skip_test`, and `test-pr-install.sh` fails on one under
`full-stack/`. The FS-CSI cases find the driver pods by `$E2E_CSI_PODS` in `$E2E_INSTALL_NS` and the
injected volume by `$CSI_DRIVER_NAME`. `test-pr-install.sh` fails on a tracked `*.sh`
under `operator/` or `full-stack/` that names a release target by hand:

- `cfgd-system` or `$CFGD_NAMESPACE` / `${CFGD_NAMESPACE}`
- `app=cfgd-operator` or `app.kubernetes.io/name=cfgd-operator`
- `cfgd-operator`, bare or quoted, after `deploy`, `deployment`, `deployments`,
  `deployment.apps`, `deployments.apps`, `endpoints`, `ep`, `svc`, `service` or
  `services`, joined by `/` or spaces
- `cfgd-validating-webhooks`, `cfgd-mutating-webhooks` or `csi.cfgd.io`

A longer name that starts the same (`cfgd-systemd`, `cfgd-operator-leader`) passes.
The full-stack suite names `cfgd-system` on purpose in two places, listed by file and
line text in `common/release-targets-kept.tsv`: the device gateway `cfgd-server`,
which the PR install does not replace, and the MachineConfigs, ConfigPolicy and
DriftAlert of the fleet and drift cases, which live where the gateway works and reach
the PR operator through the run label. Each row clears one line, so a text kept on
three lines of a file has three rows; a further line with that text fails, as does a
row no line uses. Each suite has a floor of scripts the scan must read. The
YAML under `operator/manifests/` is the release operator's own definition, which setup
applies where ArgoCD does not run it, so the scan does not read it. `helpers.sh` names
the release webhook configurations once, as `$E2E_RELEASE_VALIDATING_WEBHOOK` and
`$E2E_RELEASE_MUTATING_WEBHOOK`, for setup and the check below.

Setup scopes the release's webhooks away from run-labelled objects and namespaces
(`cfgd.io/e2e-run DoesNotExist`), and a setup run from a branch without that scoping
re-applies them unscoped. The operator, full-stack and gateway suites call
`require_release_webhooks_scoped` before their first case. It stops the suite when an
entry of `cfgd-validating-webhooks` lacks the expression in its `objectSelector`, or one
of `cfgd-mutating-webhooks` in its `namespaceSelector`; when either configuration is
missing or cannot be read; and when ArgoCD tracks either, where setup stops too.
`test-pr-install.sh` drives it against the fixtures in `common/fixtures/release-webhooks/`
and fails when a suite that applies operator objects does not call it from its setup.

Every `ERROR` line an e2e script prints goes to stderr, so a caller that captures or
discards stdout still shows it. `test-pr-install.sh` reads every tracked `tests/e2e/*.sh`
and fails, naming file and line, on an `echo` or `printf` of an `ERROR` line outside
comments and heredoc bodies whose own command carries no `>&2`, `1>&2` or
`>/dev/stderr` and no enclosing compound is redirected to stderr at its closer: `}`,
`)`, `fi`, `done` or `esac`, on the same line or a later one. The commands on a line are
split at `;`, `&&`, `||`, `|` and `&` outside quotes, so in
`echo "ERROR: a" >&2; echo "ERROR: b"` the second echo is named. The echo is found past
assignments, redirects, a function header (`f() {`, `function f {`), a case pattern
(`*)`), the openers `{`, `(`, `if`, `while`, `until`, `for`, `select` and `case ... in`,
and the prefix words `!`, `then`, `do`, `else`, `elif`, `time`, `command`, `builtin` and
`exec`.

The PR operator reconciles only objects labelled with the run, so every object of
a kind the operator serves that a suite applies carries the label in its own
`metadata.labels`, spelled as the variable:

```yaml
metadata:
  name: e2e-workstation-1
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
```

The kinds are the ones `schemas/crds.yaml` declares (`.spec.names.kind`: today
BackupPolicy, ClusterConfigPolicy, ConfigPolicy, DriftAlert, MachineConfig and
Module), read when the check runs, so a new CRD joins the rule. An object is applied
when its heredoc feeds `kubectl apply`, `create` or `replace` on the same command
(directly, through `exec_in_pod`, `$KUBECTL` or a function) or a wrapper function
whose body applies stdin. The heredoc delimiter stays unquoted (`<<EOF`) so
the label expands.

`test-pr-install.sh` reads every `tests/e2e/*/scripts` directory and `common/helpers.sh`
through `common/heredocs.awk`, the heredoc reader `test-verdicts.sh` uses too. It reads
each heredoc body as bash sends it and parses it with mikefarah `yq` v4, which the check
needs on PATH and refuses to run without. A body with an unquoted delimiter is rendered
first: each `export NAME_YAML="key: ..."` entry of `helpers.sh` to its key and a value (the
run label's value one the check recognises), any other `$VAR`, `${...}`, `$(...)` or
backtick span to one plain word. A body with a quoted delimiter is read as written. JSON,
flow style, quoted keys, tags, anchors and aliases read as block YAML does, and cfgd.io
text inside a string value is no object. It fails on:

- an object with no `${E2E_RUN_LABEL_YAML}` in its labels, or the label spelled by hand
- a quoted delimiter, or a cfgd.io object nested inside another document (a `List` item, a map below the root); an `ownerReferences` entry is a reference and passes
- a cfgd.io object whose kind is missing or not a string
- a shell expansion in an applied or captured heredoc that the scan cannot see through: in a
  `kind` or `apiVersion` (`kind: $KIND`), or as a whole document or `List` item
  (`$MODULE_DOC`, `$(cat module.yaml)`); one inside a block scalar is text and passes
- a line of an unquoted heredoc applied or captured that ends in one `\`, which bash joins
  to the next line
- a heredoc applied or captured that is not valid YAML once rendered, such as a `$(...)` in column 0 inside a block scalar
- an operator object in a heredoc captured into a variable (`yaml=$(cat <<EOF`), whose destination the scan cannot see
- a cfgd.io `apiVersion` outside any heredoc
- a script other than `helpers.sh` that sets `E2E_RUN_LABEL_YAML`
- in the operator, full-stack and gateway suites or `helpers.sh`, an apply the scan cannot
  read. An operator object reaches the cluster only from a heredoc on the apply command
  itself (`kubectl apply -f - <<EOF`, or a wrapper function fed the same way). A manifest applied by path
  (`-f mc.yaml`, `--filename`, `-k`, `-f <(...)`) fails, and so does an apply reading stdin
  from anything else: a `<` redirect, a here-string, a process substitution, a heredoc on
  another descriptor (`3<<EOF`), a pipe (from `cat`, `echo`, `printf`, or a heredoc fed to
  another command), or nothing on the line. An apply is a command with an `apply`, `create`
  or `replace` word and a `-f`, `--filename`, `-k` or `--kustomize` argument, run by
  `kubectl`, a variable or array (`$KUBECTL`, `"${kc[@]}"`) or a function a scanned script
  defines; another tool's `apply` (`cfgd apply`) is not one. A wrapper is a function whose
  body reads stdin with nothing feeding it into an apply or into another wrapper: its body passes, and a call of it is an apply reading stdin. A function is
  called, or defined, only where bash reads a command word (`FOO=1 w`, `time w`, `true && w`),
  so `echo w` and `> w` call nothing. An apply with `--dry-run=client` sends nothing and passes
- a function body (`{ }` or `( )`) still open at the end of its script, where the scan
  cannot tell which commands are inside it. A `{` or `}` counts only where bash reads it as
  a reserved word, the first word of a command (`echo {` opens nothing)
- a quote still open at the end of its script, where the scan cannot tell which lines are
  commands. A backslash escapes the next character inside `"..."` and `$'...'`
- a function whose name holds `{` or `}` (`a{b() {`), whose calls the scan cannot tell from
  a brace group. Any other name bash takes for a function (`k+x`, `1k`, `function k%x`) is
  read
- one function name defined with two different bodies. Every function a scanned script
  (each `tests/e2e/*/scripts` directory and `helpers.sh`) defines is visible to all of them,
  so the scan names both definitions. Bodies are compared as bash runs them, with
  comments, spacing and line breaks aside
- fewer sites than its suite's floor (the operator, full-stack and gateway suites each carry one)

A cfgd.io document of another kind (the crossplane suite's `TeamConfig`) is listed
as outside the operator's watch. A heredoc written to a file (`cat >`, or a cfgd
config file written inside a pod), or captured into a variable when it holds a kind the
operator does not serve, is counted and needs no label; one written to a file that `yq`
cannot read (a script, say) is skipped. Every TeamConfig heredoc names the run's
Composition (see below). No suite applies a manifest by path; the crossplane suite's
Composition reaches `kubectl apply -f -` on a here-string from `render_run_composition`.

## Components ArgoCD owns

On the shared cluster ArgoCD deploys the live operator, the device gateway and the
CSI node plugin from `/db/manifests/k3s/namespaces/cfgd-system/`, so those run the
release the manifests pin, whatever this run built. This run's operator and CSI
images run in the PR install instead: Helm release `$E2E_INSTALL_RELEASE` in
`$E2E_INSTALL_NS`, with CSI driver `e2e.csi.cfgd.io`, scoped to the objects and
namespaces that carry the run label. `setup-cluster.sh` prints the image each live
component and each PR install workload runs, and warns when `OPERATOR_IMAGE_TAG` is
set while ArgoCD owns the gateway, which the override then does not reach. When
`CFGD_DEPLOY_MANIFESTS` names a tree that `task deploy:operator` applied, that tree
owns the operator and gateway Deployments, and setup warns the same way.

ArgoCD owns the Crossplane objects in
`/db/manifests/k3s/namespaces/crossplane-system/` too: `Function/function-cfgd`, pinned
to the released `ghcr.io/tj-smith47/function-cfgd` package, its
`DeploymentRuntimeConfig/function-cfgd-runtime`, the TeamConfig XRD and the
`teamconfig-to-machineconfigs` Composition. No e2e script writes them. Setup pushes
the run's function package to `registry.jarvispro.io/function-cfgd:<tag>`
(`e2e_image function-cfgd`) and to no other tag. The Crossplane suite runs
`check_pr_xrd`, which compares the spec of `manifests/crossplane/xrd-teamconfig.yaml`
with the cluster's XRD, Crossplane's `defaultCompositeDeletePolicy: Background` and
`defaultCompositionUpdatePolicy: Automatic` filled in on both sides, and stops on a
difference, a missing XRD or one that is not `Established`; to run a PR that changes
the XRD, copy the file over `xrd-teamconfig.yaml` there (keeping its ArgoCD
annotations), push and rerun the suite. It then deletes any TeamConfig (in every
namespace), Composition and Function labelled `cfgd.io/e2e-run`, in that order (the E2E
workflow runs one at a time, so any found is a leftover; a leftover TeamConfig's
MachineConfigs would count toward this run's cases, and Crossplane's package lock admits
one Function per repository), and waits for the removed Functions' revisions to go. A
warm-up TeamConfig that composes no MachineConfig within 180s stops the suite with its
conditions. It installs `$E2E_FUNCTION`
(`function-cfgd-<run>`) from the run's package with ArgoCD's runtime config, and
`$E2E_COMPOSITION` (`teamconfig-to-machineconfigs-<run>`), which
`render_run_composition` renders from `manifests/crossplane/composition.yaml` with its
name, its Function and the run label changed. Every TeamConfig carries `${E2E_RUN_LABEL_YAML}` and sets
`spec.crossplane.compositionRef.name: ${E2E_COMPOSITION}`. The suite's end and
`pr-install-down.sh` delete the run's TeamConfigs, then its Composition and Function,
and read back that both are gone. `test-pr-install.sh` drives `check_pr_xrd` against
`common/fixtures/xrd/`, the render against the real Composition, and fails on a
TeamConfig heredoc without the run label or that reference (or that yq cannot read), a
tracked manifest holding a TeamConfig, and a script line that applies anything under
`manifests/crossplane`, runs `kubectl` on `function-cfgd` or
`teamconfig-to-machineconfigs`, runs a write verb on `teamconfigs.cfgd.io` or names a
`:latest` function package, and on a heredoc, whatever reads it, holding a Function,
Composition, XRD or DeploymentRuntimeConfig named after one of ArgoCD's four.

ArgoCD also owns the cluster's CRDs, applied from
`/db/manifests/k3s/namespaces/crossplane-system/cfgd-crds.yaml`, and no e2e script
writes them. Setup compares the spec of each CRD that `cfgd-gen-crds` prints with the
cluster's copy, with description text dropped and the API server's defaults
(`names.listKind`, `names.singular`, `conversion: {strategy: None}`,
`preserveUnknownFields: false`) filled in on both sides. Setup stops, printing the
difference, when a CRD is missing from the cluster, its spec differs or its
`Established` condition is not `True`. To run a PR that changes a CRD, copy
`schemas/crds.yaml` over that file, push `/db/manifests`, let ArgoCD sync, then rerun
setup. `crd_docs_json`, `crd_shape` and `check_pr_crds` in `common/helpers.sh` do
the comparison, and `common/test-pr-install.sh` drives them against the fixtures in
`common/fixtures/crd-schema/` with a stub `kubectl get`. It needs `kubectl`, which
reads the fixtures' YAML offline (`annotate --local`), and `jq`.
The Crossplane suite runs the same `check_pr_crds` against `schemas/crds.yaml` once
XP-01 has found Crossplane running, before `check_pr_xrd` compares its XRD. The Helm suites install
`chart/cfgd` with `--skip-crds`; FS-HELM-05 and FS-HELM-08 run `check_pr_crds` after
the upgrade and the uninstall to show the CRDs ArgoCD applied are still in place.
The full-stack Helm suite installs the chart as `cfgd-test` beside the PR install, so
each install and upgrade passes `"${HELM_SCOPE[@]}"` from `helm_test_ns`: it sets
`operator.watchLabelSelector`, `webhook.objectSelector` and
`mutatingWebhook.namespaceSelector` to the install's own `cfgd.io/e2e-helm=$HELM_NS`
label (the injector's default `matchExpressions` nulled, since Helm merges map
values), and `helm_test_ns` puts that label on the namespace. The test operator,
validating webhook and pod injector then act only on objects and namespaces carrying
the label, and an object a case applies for its own install carries it too.
`test-pr-install.sh` fails on a full-stack `helm install` or `helm upgrade` that does
not pass the array, one with a later flag that sets one of its keys or their parent,
a `HELM_SCOPE` array that holds anything but its three flags, and any other line that
writes the array (an append, a second definition, an element write or an unset), and
fails when `test-helm.sh` falls below its floor of sites or defines no complete array.

The CRD-write scan in `test-pr-install.sh` reads the `kubectl` and `helm` write
commands and heredoc bodies of every tracked `tests/e2e/*.sh`, through
`common/crd-writes.awk`, and every tracked manifest under `tests/e2e`. It fails on a
`kubectl` write that names a CRD (`apply`, `create`, `replace`, `patch`, `delete` and
the like, outside `--local` and `--dry-run`), a `helm install` or `helm upgrade
--install` without `--skip-crds`, a heredoc holding `kind: CustomResourceDefinition`
whatever reads it, or a tracked `.yaml`, `.yml` or `.json` outside `common/fixtures/`
holding one. Both read the kind bare, quoted or followed by a comment, as a list
item's first key, or as a JSON member.
An exempt write is listed in the test by tag, file and command text, with its reason;
the one entry is XP-01's Helm install of Crossplane, which runs only where ArgoCD does
not run Crossplane and whose chart holds none of the CRDs in `cfgd-crds.yaml`.

A check reads a counter through `metric_sample_lines` or `metric_sample_value` in
`common/helpers.sh`, which read only the `<family>_total` sample the components
built from the checkout render, so a sample whose name doubles the suffix is not read.
`common/test-metrics.sh` (run by `task e2e:tags:check`)
fails when a script matches a counter sample by hand.

A check for behaviour that only a newer build than the pinned release has reads
the running component's capability first and calls `skip_test` naming the image
(`running_image` in `common/helpers.sh`) when the release lacks it. The FS-CSI
cases run this run's CSI driver, so FS-CSI-04 fails when the driver served no
cache-hit sample for its module after the mount. XP-01 reads ArgoCD's tracking-id
annotation (`argocd_managed` in `common/helpers.sh`) on
`deployment/crossplane` in `crossplane-system`: where ArgoCD tracks it the suite
installs nothing, where it does not the suite runs `helm upgrade --install`, and where
it cannot be read the suite stops before calling Helm. Setup stops the same way when
it cannot read the operator Deployment or a webhook configuration.

prometheus-client renders no line at all for a metric family with no sample, so
a metrics check drives the event first and then scrapes the pod that performed
it: FS-CSI-04 and FS-CSI-07 the driver on the mounting node. OP-LC-01 touches
its own MachineConfig and scrapes the operator pod the leader lease names, up to
`E2E_METRICS_TRIES` attempts (12 by default, 5s apart), reading the lease again
on each attempt. OP-LC-02 reads the lease up to `E2E_LEASE_TRIES` times (6 by
default, 5s apart) until its holder names an operator pod, since a deleted pod
keeps the lease until it expires. No case passes on a note that the thing it
checks did not happen: `common/test-verdicts.sh` fails on a `pass_test` that
follows such a note in the same branch, unless that line carries
`# verdict-ok: <why>`.

A step that needs something to happen first waits on that state through a helper in
`common/helpers.sh`, which polls up to a deadline and says on timeout what it waited
for:

```bash
wait_until 30 1 "two reconcile ticks" pod_log_count_at_least /tmp/d.log 'reconcile: complete' 2
retry_tries 6 5 lc04_apply                  # RETRY_ATTEMPT holds the attempts made
wait_for_pod_log /tmp/daemon.log 'daemon: running' 30
wait_for_deleted 60 namespace "$NS_A"
wait_for_injection "$NS" "my-module:v1"     # a server-side dry run comes back with the CSI volume
```

A refresh that has to outlive the step starting it, such as the run's heartbeat or
the setup lease renewal, runs through `run_every <interval_s> <command...>`, the one
background cadence in `helpers.sh`:

```bash
run_every "$HEARTBEAT_INTERVAL_SECONDS" _heartbeat_beat
HEARTBEAT_PID=$!
```

`common/test-waits.sh` (run by `task e2e:tags:check`) fails on a `sleep` anywhere
else under `tests/e2e/`, and on one in a `helpers.sh` function whose body has no
deadline test: a `[`, `[[` or `((` test naming `SECONDS`, `$deadline` or `$tries`.
It reads every way bash runs `sleep` as a command, wrappers such as `timeout`, `env`
and `exec_in_pod`, `kubectl exec ... --` and `sh -c` scripts included, and prints
`run_every` under Cadences. A sleep that waits on wall-clock behaviour under test,
such as a timestamp with one-second resolution, carries `# sleep-ok: <why>` on its
line, and the check prints every such line with its reason.
