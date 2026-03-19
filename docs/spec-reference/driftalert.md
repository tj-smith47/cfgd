# DriftAlert Spec Reference

`DriftAlert` is a namespaced Kubernetes custom resource (`cfgd.io/v1alpha1`) created by the cfgd
operator when a device's reported state diverges from the desired state declared in its
`MachineConfig`. Alerts are created automatically — you do not create them manually. They are the
primary mechanism for surfacing fleet drift in the operator dashboard and via external alerting
integrations.

**API group:** `cfgd.io/v1alpha1`
**Scope:** Namespaced

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: string
  namespace: string

spec:
  deviceId: string
  machineConfigRef: string
  severity: Low | Medium | High | Critical

  driftDetails:
    - field: string
      expected: string
      actual: string

status:
  detectedAt: string
  resolvedAt: string
  resolved: bool
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Resource name. Conventionally `<device-id>-<timestamp>` to make each alert uniquely addressable. |
| `namespace` | string | Yes | | Kubernetes namespace. Typically the same namespace as the associated `MachineConfig`. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `deviceId` | string | Yes | | Unique identifier for the device that reported the drift. Matches the device's enrollment ID in the gateway database. |
| `machineConfigRef` | string | Yes | | Name of the `MachineConfig` resource that the device is reconciled against. |
| `severity` | enum | Yes | | Severity classification of this drift event. See [DriftSeverity values](#driftseverity-values). |
| `driftDetails` | list | No | `[]` | Itemised list of fields that are out of sync. See [spec.driftDetails[]](#specdriftdetails). |

#### DriftSeverity values

Serialised as PascalCase (no rename applied to enum variants).

| Value | Description |
|-------|-------------|
| `Low` | Minor divergence with no immediate operational impact (e.g. a missing optional package). |
| `Medium` | Divergence that may affect reliability or observability but is not immediately dangerous. |
| `High` | Divergence that affects security posture or cluster operation (e.g. missing kernel module, wrong sysctl). |
| `Critical` | Divergence that constitutes an active security or availability risk. Triggers immediate alerting. |

---

### spec.driftDetails[]

Each entry describes a single field that differs between desired and actual state.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `field` | string | Yes | | Dot-path to the field that differs (e.g. `spec.systemSettings.net.ipv4.ip_forward`, `spec.packages[2]`). |
| `expected` | string | Yes | | The value declared in the `MachineConfig` (desired state). |
| `actual` | string | Yes | | The value reported by the device (actual state). |

**Example:**
```yaml
driftDetails:
  - field: spec.systemSettings.net.ipv4.ip_forward
    expected: "1"
    actual: "0"
  - field: spec.packages
    expected: "socat"
    actual: "<not installed>"
```

---

### status

Written by the operator when an alert is created or resolved. Do not set manually.

| Field | Type | Description |
|-------|------|-------------|
| `detectedAt` | string (ISO 8601) | Timestamp when the drift was first detected and the alert was created. |
| `resolvedAt` | string (ISO 8601) | Timestamp when the drift was corrected and the device returned to desired state. Absent until resolved. |
| `resolved` | bool | `true` when the drift has been corrected. The operator patches this field when the next successful check-in reports full compliance. |

---

## Full Example

```yaml
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: node-42-2026-03-19t14-30-00z
  namespace: team-platform
spec:
  deviceId: "node-42"
  machineConfigRef: alice-k8s-worker
  severity: High
  driftDetails:
    - field: spec.systemSettings.net.ipv4.ip_forward
      expected: "1"
      actual: "0"
    - field: spec.moduleRefs[containerd]
      expected: "installed"
      actual: "not found"
status:
  detectedAt: "2026-03-19T14:30:00Z"
  resolved: false
```
