# DriftAlert Spec Reference

`DriftAlert` is a namespaced Kubernetes custom resource (`cfgd.io/v1alpha1`) created by the cfgd
device gateway when a device reports a **system setting** whose live value diverges from the one
its `MachineConfig` declares. Alerts are created automatically; you do not create them manually.
They are the mechanism for surfacing reported system-settings drift in the operator dashboard and
via external alerting integrations.

**What a DriftAlert covers.** A device's report is produced by `cfgd checkin`, whose drift payload
is the answers of the system configurators its profile declares (`sysctl`, `kernelModules`,
`macosDefaults`, `windowsRegistry`, ...) and nothing else. Managed files, packages, env vars and
aliases are checked on the device by `cfgd diff`, and reach the fleet only as the aggregate
counts of a check-in's compliance summary — never as findings. A device with no open DriftAlert
is a device whose system settings matched, not a device proven in sync.

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
  machineConfigRef:
    name: string
    namespace: string
  severity: Low | Medium | High | Critical

  driftDetails:
    - field: string
      expected: string
      actual: string

status:
  detectedAt: string
  resolvedAt: string

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
| `name` | string | Yes | | Resource name. Conventionally `<device-id>-<timestamp>` to make each alert uniquely addressable. |
| `namespace` | string | Yes | | Kubernetes namespace. Typically the same namespace as the associated `MachineConfig`. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `deviceId` | string | Yes | | Unique identifier for the device that reported the drifted settings. Matches the device's enrollment ID in the gateway database. |
| `machineConfigRef` | object | Yes | | Typed reference to the `MachineConfig` resource that the device is reconciled against. See [spec.machineConfigRef](#specmachineconfigref). |
| `severity` | enum | Yes | | Severity classification of this drift event. See [DriftSeverity values](#driftseverity-values). |
| `driftDetails` | list | No | `[]` | Itemised list of system settings that are out of sync. See [spec.driftDetails[]](#specdriftdetails). |

#### DriftSeverity values

Serialised as PascalCase (no rename applied to enum variants).

| Value | Description |
|-------|-------------|
| `Low` | Minor divergence with no immediate operational impact (e.g. a cosmetic desktop setting). |
| `Medium` | Divergence that may affect reliability or observability but is not immediately dangerous. |
| `High` | Divergence that affects security posture or cluster operation (e.g. missing kernel module, wrong sysctl). |
| `Critical` | Divergence that constitutes an active security or availability risk. Triggers immediate alerting. |

---

### spec.machineConfigRef

Typed reference to a `MachineConfig` resource.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Name of the `MachineConfig` resource. |
| `namespace` | string | No | | Namespace of the `MachineConfig`. When omitted, the alert's own namespace is assumed. |

**Example:**
```yaml
machineConfigRef:
  name: alice-k8s-worker
  namespace: team-platform
```

---

### spec.driftDetails[]

Each entry describes a single system setting whose live value differs from its declared one.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `field` | string | Yes | | The setting's key within its system configurator, as the device reported it (e.g. `net.ipv4.ip_forward`). |
| `expected` | string | Yes | | The value declared in the `MachineConfig` (desired state). |
| `actual` | string | Yes | | The value reported by the device (actual state). |

**Example:**
```yaml
driftDetails:
  - field: net.ipv4.ip_forward
    expected: "1"
    actual: "0"
  - field: net.bridge.bridge-nf-call-iptables
    expected: "1"
    actual: "0"
```

---

### status

Written by the operator when an alert is created or resolved. Do not set manually.

| Field | Type | Description |
|-------|------|-------------|
| `detectedAt` | string (ISO 8601) | Timestamp when the drifted setting was first reported and the alert was created. |
| `resolvedAt` | string (ISO 8601) | Timestamp when the drift was corrected and the device returned to desired state. Absent until resolved. |
| `conditions` | list | Standard Kubernetes conditions. The `Resolved` condition reflects current resolution state. |

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
  machineConfigRef:
    name: alice-k8s-worker
  severity: High
  driftDetails:
    - field: net.ipv4.ip_forward
      expected: "1"
      actual: "0"
    - field: overlay
      expected: "loaded"
      actual: "not loaded"
status:
  detectedAt: "2026-03-19T14:30:00Z"
```
