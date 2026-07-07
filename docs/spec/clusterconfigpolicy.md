# ClusterConfigPolicy Spec Reference

`ClusterConfigPolicy` is a **cluster-scoped** Kubernetes custom resource (`cfgd.io/v1alpha1`) that
declares configuration requirements that hold across every namespace it selects. It is the
cluster-wide sibling of the namespaced [`ConfigPolicy`](configpolicy.md): where a `ConfigPolicy`
targets `MachineConfig` resources by label within one namespace, a `ClusterConfigPolicy` selects
whole namespaces via `namespaceSelector` and additionally carries a fleet-wide `security` policy
for module provenance. The cfgd operator evaluates it against all matching `MachineConfig`
resources and reports compliance counts in the status.

When both a `ClusterConfigPolicy` and a namespaced `ConfigPolicy` apply to the same
`MachineConfig`, the cluster policy takes precedence on conflicts — see
[multi-tenancy.md](../multi-tenancy.md#policy-merge-semantics) for the full merge rules.

**API group:** `cfgd.io/v1alpha1`
**Scope:** Cluster
**Short name:** `ccpol`

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: string          # cluster-scoped: no namespace

spec:
  namespaceSelector:
    matchLabels:
      label-key: label-value

  requiredModules:
    - name: string
      required: bool

  debugModules:
    - name: string
      required: bool

  packages:
    - name: string
      version: semver-requirement  # optional

  settings:
    key: value

  security:
    trustedRegistries:
      - string
    allowUnsigned: bool

status:
  compliantCount: int
  nonCompliantCount: int

  conditions:
    - type: string
      status: string
      reason: string
      message: string
      lastTransitionTime: string
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Resource name. `ClusterConfigPolicy` is cluster-scoped, so it has no `namespace`. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `namespaceSelector` | LabelSelector | No | `{}` | Kubernetes-style label selector applied to **namespaces**. `MachineConfig` resources in matching namespaces are evaluated. Uses `matchLabels` (and optional `matchExpressions`); an empty selector matches every namespace. |
| `requiredModules` | list of ModuleRef | No | `[]` | Modules that must be present in every matched `MachineConfig`. Each entry has a `name` (required) and optional `required` bool. |
| `debugModules` | list of ModuleRef | No | `[]` | Modules staged as debug-only (CSI volume without volumeMount on declared containers). Same entry shape as `requiredModules`. |
| `packages` | list of PackageRef | No | `[]` | Required packages. Each entry has a `name` (required) and optional `version` constraint (semver range, e.g. `>=1.28`, `~2.40`). See [ConfigPolicy → packages[].version format](configpolicy.md#packagesversion-format). |
| `settings` | map | No | `{}` | Key/value system settings that must be present in every matched `MachineConfig`'s `systemSettings`. Keys must not be empty. |
| `security` | SecurityPolicy | No | `{}` | Cluster-wide module provenance policy. See [security](#specsecurity). |

---

### spec.security

A fleet-wide module-provenance gate. Unlike the other fields (which assert desired state on matched
machines), `security` constrains where module content may come from and whether it must be signed —
a control only a cluster administrator should set, which is why it lives on the cluster-scoped policy
rather than on the namespaced `ConfigPolicy`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `trustedRegistries` | list of string | No | `[]` | OCI registries modules may be pulled from. An empty list imposes no registry restriction. |
| `allowUnsigned` | bool | No | `false` | When `false`, modules must carry a valid cosign signature; when `true`, unsigned modules are permitted. |

---

### status

Written by the operator after each evaluation pass. Do not set manually.

| Field | Type | Description |
|-------|------|-------------|
| `compliantCount` | uint | Number of matched `MachineConfig` resources that satisfy all requirements. |
| `nonCompliantCount` | uint | Number of matched `MachineConfig` resources that violate one or more requirements. |
| `conditions` | list | Standard Kubernetes condition list. See [status.conditions[]](#statusconditions). |

The `Enforced` condition surfaces in `kubectl get ccpol` as a dedicated column.

---

### status.conditions[]

Follows the standard Kubernetes condition convention.

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Condition type identifier (e.g. `Enforced`, `Evaluated`, `Ready`). |
| `status` | string | `"True"`, `"False"`, or `"Unknown"`. |
| `reason` | string | Short CamelCase reason token. |
| `message` | string | Human-readable explanation. |
| `lastTransitionTime` | string (ISO 8601) | When this condition last changed status. |

---

## Full Example

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: org-baseline
spec:
  namespaceSelector:
    matchLabels:
      cfgd.io/managed: "true"
  requiredModules:
    - name: security-tools
      required: true
    - name: corp-certs
      required: true
  packages:
    - name: osquery
      version: ">=5.0"
  settings:
    net.ipv4.ip_forward: "1"
  security:
    trustedRegistries:
      - ghcr.io/acme-corp
    allowUnsigned: false
```
