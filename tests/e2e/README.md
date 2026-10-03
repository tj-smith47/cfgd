# End-to-end suites

The suites under `tests/e2e/` run against a real Kubernetes cluster. `task e2e:setup`
(`setup-cluster.sh`) builds and pushes the images, then deploys what the suites need;
each `task e2e:<suite>` target runs one suite.

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
(directly, through `exec_in_pod`, `$KUBECTL` or a function) or a wrapper such as
`apply_yaml`. The heredoc delimiter stays unquoted (`<<EOF`) so
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
  itself (`kubectl apply -f - <<EOF`, `apply_yaml "T01" <<EOF`). A manifest applied by path
  (`-f mc.yaml`, `--filename`, `-k`, `-f <(...)`) fails, and so does an apply reading stdin
  from anything else: a `<` redirect, a here-string, a process substitution, a heredoc on
  another descriptor (`3<<EOF`), a pipe (from `cat`, `echo`, `printf`, or a heredoc fed to
  another command), or nothing on the line. An apply is a command with an `apply`, `create`
  or `replace` word and a `-f`, `--filename`, `-k` or `--kustomize` argument, run by
  `kubectl`, a variable or array (`$KUBECTL`, `"${kc[@]}"`) or a function a scanned script
  defines; another tool's `apply` (`cfgd apply`) is not one. A wrapper is a function whose
  body reads stdin with nothing feeding it into an apply or into another wrapper, such as
  `apply_yaml`: its body passes, and a call of it is an apply reading stdin. A function is
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
cannot read (a script, say) is skipped. A manifest another suite applies by path (the
crossplane suite's) is listed.

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
XP-01 has found Crossplane running, before it applies its XRD. The Helm suites install
`chart/cfgd` with `--skip-crds`; FS-HELM-05 and FS-HELM-08 run `check_pr_crds` after
the upgrade and the uninstall to show the CRDs ArgoCD applied are still in place.

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

Because the gateway suite, and each suite not yet moved to the PR install, runs a
release, a check reads a counter through
`metric_sample_lines` or `metric_sample_value` in `common/helpers.sh`, which
accept both the `<family>_total` sample and the `<family>_total_total` sample an
older release renders. `common/test-metrics.sh` (run by `task e2e:tags:check`)
fails when a script matches a counter sample by hand.

A check for behaviour that only a newer build than the pinned release has reads
the running component's capability first and calls `skip_test` naming the image
(`running_image` in `common/helpers.sh`) when the release lacks it. FS-CSI-04
skips only when the driver served no cache-hit sample for its module after the
mount, runs an image other than this run's, and its DaemonSet carries ArgoCD's
tracking-id annotation (`argocd_managed` in `common/helpers.sh`); without that
annotation the mismatch fails, naming the running and wanted images, and when the
DaemonSet cannot be read the case fails with the ERROR `argocd_owner` prints, which
names the object, its namespace and the rerun to do. XP-01 reads the same annotation on
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
