# cfgd Helm Chart

Declarative, GitOps-style machine configuration management for Kubernetes.

## Install

```bash
helm install cfgd ./chart/cfgd -n cfgd-system --create-namespace
```

## Components

| Component | Default | Description |
|-----------|---------|-------------|
| `operator.enabled` | `true` | CRD controllers + admission webhooks |
| `agent.enabled` | `false` | Node agent DaemonSet |
| `csiDriver.enabled` | `false` | CSI driver for pod module injection |
| `deviceGateway.enabled` | `false` | Device enrollment + fleet management |
| `webhook.enabled` | `true` | Validating admission webhooks |
| `mutatingWebhook.enabled` | `true` | Pod module injection webhook |

## Configuration

See [values.yaml](values.yaml) for all configurable values.

## Examples

- [Operator only](examples/operator-only.yaml)
- [With gateway](examples/with-gateway.yaml)
- [Full deployment](examples/full.yaml)
