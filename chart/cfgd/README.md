# cfgd Helm Chart

Declarative, GitOps-style machine configuration management for Kubernetes. One
chart ships the operator, the node-agent DaemonSet, the CSI driver, and the
device gateway; each component is toggled independently.

## Install

```bash
helm install cfgd ./chart/cfgd -n cfgd-system --create-namespace
```

Or from the published OCI chart:

```bash
helm install cfgd oci://ghcr.io/tj-smith47/charts/cfgd -n cfgd-system --create-namespace
```

## Prerequisites

- Kubernetes cluster with RBAC
- [cert-manager](https://cert-manager.io/), when `webhook.enabled` is true with
  `webhook.certManager.enabled: true` (the default): the chart creates a
  self-signed Issuer and Certificate for the webhook TLS serving cert

## Components

| Component | Default | Description |
|-----------|---------|-------------|
| `operator.enabled` | `true` | CRD controllers + admission webhooks |
| `agent.enabled` | `false` | Node agent DaemonSet (`cfgd daemon` on every node) |
| `csiDriver.enabled` | `false` | CSI driver for pod module injection |
| `deviceGateway.enabled` | `false` | Device enrollment + fleet management |
| `webhook.enabled` | `true` | Validating admission webhooks |
| `mutatingWebhook.enabled` | `true` | Pod module injection webhook |

## Key values

| Value | Default | Description |
|---|---|---|
| `installCRDs` | `true` | Install the cfgd.io CRDs with the chart |
| `operator.replicaCount` | `1` | Operator replicas; `operator.leaderElection.enabled` (`true`) makes >1 safe |
| `agent.serverUrl` | `""` | Device gateway URL the node agent checks in to |
| `agent.apiKeySecret.name` | `""` | Secret holding the agent API key (key: `agent.apiKeySecret.key`, default `api-key`) |
| `agent.reconcileInterval` | `5m` | Node agent reconcile interval |
| `webhook.failurePolicy` | `Fail` | Validating webhook failure policy |
| `mutatingWebhook.failurePolicy` | `Ignore` | `Ignore` skips injection silently on webhook failure; set `Fail` to require it |
| `deviceGateway.enrollmentMethod` | `token` | `token` (bootstrap tokens) or `key` (SSH/GPG challenge-response) |
| `deviceGateway.persistence.enabled` | `true` | PVC for the gateway SQLite database (`deviceGateway.persistence.size`, default `1Gi`) |
| `csiDriver.cache.maxSizeGi` | `5` | Per-node module cache size |
| `metrics.enabled` | `true` | Prometheus metrics endpoint; `metrics.serviceMonitor.enabled` (`false`) adds a ServiceMonitor |
| `rbacExamples.enabled` | `false` | Install example RBAC roles for multi-tenant personas |
| `networkPolicy.enabled` | `false` | NetworkPolicy for chart components |
| `podDisruptionBudget.enabled` | `false` | PDB for the operator |

See [values.yaml](values.yaml) for the full set (images, resources, probes,
tolerations, security contexts).

Agent pods run privileged by design: host config management requires root
access. They do not use the shared `podSecurityContext` /
`containerSecurityContext` values.

## Examples

- [Operator only](examples/operator-only.yaml)
- [With gateway](examples/with-gateway.yaml)
- [Full deployment](examples/full.yaml)

```bash
helm install cfgd ./chart/cfgd -n cfgd-system --create-namespace \
  -f chart/cfgd/examples/with-gateway.yaml
```

## Documentation

- [Operator, CRDs, device gateway](../../docs/operator.md)
- [Multi-tenancy and RBAC](../../docs/multi-tenancy.md)
