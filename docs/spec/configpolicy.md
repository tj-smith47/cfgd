# ConfigPolicy Spec Reference

`ConfigPolicy` is a namespaced Kubernetes custom resource (`cfgd.io/v1alpha1`) that declares a set
of configuration requirements that must hold across a fleet of machines. The cfgd operator evaluates
each `ConfigPolicy` against all `MachineConfig` resources that match its `targetSelector` and
reports compliance counts in the status.

**API group:** `cfgd.io/v1alpha1`
**Scope:** Namespaced

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: string
  namespace: string

spec:
  name: string

  requiredModules:
    - string

  packages:
    - string

  packageVersions:
    package-name: semver-requirement

  settings:
    key: value

  targetSelector:
    label-key: label-value

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
| `name` | string | Yes | | Resource name. |
| `namespace` | string | Yes | | Kubernetes namespace. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Human-readable policy name. Must not be empty. Used in status output and violation reports. |
| `requiredModules` | list of string | No | `[]` | Module names that must be present in every matched `MachineConfig`. |
| `packages` | list of string | No | `[]` | Package names that must be declared in every matched `MachineConfig`. Entries must not be empty strings. |
| `packageVersions` | map | No | `{}` | Version requirements keyed by package name. Values are semver range expressions (e.g. `>=1.28`, `~2.40`). |
| `settings` | map | No | `{}` | Key/value system settings that must be present in every matched `MachineConfig`'s `systemSettings`. Keys must not be empty. |
| `targetSelector` | map | No | `{}` | Label selector applied to `MachineConfig` resources. Only matching resources are evaluated. An empty map matches all resources in the namespace. |

#### packageVersions format

Values are semver requirement strings parsed by the `semver` crate. Supported operators:

| Operator | Example | Meaning |
|----------|---------|---------|
| `>=` | `>=1.28` | At least this version. |
| `>` | `>1.27` | Strictly greater than. |
| `<` | `<2.0` | Strictly less than. |
| `~` | `~2.40` | Compatible with patch-level changes. |
| `^` | `^1.28` | Compatible with minor-level changes. |
| `=` | `=1.28.3` | Exact version match. |

**Example:**
```yaml
packageVersions:
  kubectl: ">=1.28"
  git: "~2.40"
  terraform: ">=1.5, <2.0"
```

---

### status

Written by the operator after each evaluation pass. Do not set manually.

| Field | Type | Description |
|-------|------|-------------|
| `compliantCount` | uint | Number of matched `MachineConfig` resources that satisfy all requirements. |
| `nonCompliantCount` | uint | Number of matched `MachineConfig` resources that violate one or more requirements. |
| `conditions` | list | Standard Kubernetes condition list. See [status.conditions[]](#statusconditions). |

---

### status.conditions[]

Follows the standard Kubernetes condition convention.

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Condition type identifier (e.g. `Evaluated`, `AllCompliant`, `Ready`). |
| `status` | string | `"True"`, `"False"`, or `"Unknown"`. |
| `reason` | string | Short CamelCase reason token. |
| `message` | string | Human-readable explanation. |
| `lastTransitionTime` | string (ISO 8601) | When this condition last changed status. |

---

## Full Example

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: k8s-node-baseline
  namespace: team-platform
spec:
  name: Kubernetes Node Baseline
  requiredModules:
    - containerd
    - kubelet
    - apparmor
  packages:
    - socat
    - conntrack
  packageVersions:
    kubectl: ">=1.28"
    containerd: ">=1.7"
  settings:
    net.ipv4.ip_forward: "1"
    net.bridge.bridge-nf-call-iptables: "1"
  targetSelector:
    cfgd.io/role: k8s-node
```
