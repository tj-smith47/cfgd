# Multi-Tenancy

cfgd supports multi-tenant operation where each team works in their own Kubernetes namespace. This document covers the isolation model, RBAC roles, and how cluster-wide policies interact with namespace-scoped ones.

## Namespace Isolation Model

Each team gets a dedicated namespace. Resources are scoped as follows:

| Resource | Scope | Visibility |
|---|---|---|
| MachineConfig | Namespaced | Only within the team's namespace |
| ConfigPolicy | Namespaced | Applies only to MachineConfigs in the same namespace |
| DriftAlert | Namespaced | Associated with a MachineConfig in the same namespace |
| ClusterConfigPolicy | Cluster | Applies across all namespaces matching its `namespaceSelector` |
| Module | Cluster | Shared across all namespaces |

The operator watches all namespaces. Namespace-level RBAC controls which teams can create, view, or modify resources.

## RBAC Roles

The Helm chart includes optional RBAC example templates (enable with `rbacExamples.enabled: true`). Role names are prefixed with the release fullname (`cfgd-team-lead` for a release named `cfgd`). Four personas:

| Role | Scope | Access |
|---|---|---|
| `platform-admin` | Cluster | Full control over all cfgd resources; manages ClusterConfigPolicies, approves modules |
| `team-lead` | Namespace | Full CRUD on MachineConfigs, ConfigPolicies, and DriftAlerts in the team's namespace |
| `team-member` | Namespace | Read-only on MachineConfigs and DriftAlerts |
| `module-publisher` | Cluster | Publish Module CRDs; cannot modify MachineConfigs or policies |

## Policy Merge Semantics

When both a ClusterConfigPolicy and a namespace-scoped ConfigPolicy apply to the same MachineConfig:

| Field | Merge Rule |
|---|---|
| `packages` | Union: both policies' packages are required; ClusterConfigPolicy version constraints override namespace ConfigPolicy for the same package |
| `requiredModules` | Union: both policies' modules are required |
| `settings` | Cluster wins: ClusterConfigPolicy values override namespace ConfigPolicy |
| `trustedRegistries` | Cluster is canonical: namespace policies cannot expand the trusted list |

## Binding Teams to Namespaces

1. Create a namespace per team
2. Apply team-specific RoleBindings referencing the RBAC templates
3. Create ConfigPolicies in each namespace for team-specific requirements
4. Create ClusterConfigPolicies for organization-wide mandates

Example:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: team-alpha-lead
  namespace: team-alpha
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: cfgd-team-lead
subjects:
  - kind: User
    name: alice
    apiGroup: rbac.authorization.k8s.io
```
