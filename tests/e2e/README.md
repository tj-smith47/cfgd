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
| `OPERATOR_IMAGE_TAG` | `IMAGE_TAG` | Tag of `cfgd-operator` (operator, device gateway) |
| `CSI_IMAGE_TAG` | `IMAGE_TAG` | Tag of `cfgd-csi` (CSI node plugin) |
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

## Components ArgoCD owns

On the shared cluster ArgoCD deploys the operator, the device gateway and the CSI
node plugin from `/db/manifests/k3s/namespaces/cfgd-system/` (the CSI plugin from
`csi-daemonset.yaml`), so the operator and CSI suites there run the release those
manifests pin, whatever this run built. `setup-cluster.sh` prints the image each
of them runs, warns when `OPERATOR_IMAGE_TAG` or `CSI_IMAGE_TAG` is set for one of
them, and installs the CSI plugin with Helm only on a cluster where ArgoCD does
not own the `cfgd-csi-csi` DaemonSet.
