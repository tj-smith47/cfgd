# Team Config Distribution

How a platform engineer distributes and enforces team configuration across developer machines using [Crossplane](https://www.crossplane.io/). Builds on the [cfgd-operator](operator.md) CRDs.

[Crossplane](https://docs.crossplane.io/latest/) is a Kubernetes framework for defining custom composite resources. In cfgd's case, a platform engineer defines a single TeamConfig resource listing team members, and Crossplane's composition function automatically generates one MachineConfig CRD per team member — no manual YAML per developer.

> **Status**: Crossplane composition function implemented and tested (18 tests passing). XRD and Composition manifests committed. Not yet validated in a live Crossplane cluster.

## How It Works

```
Platform Engineer
     │
     ▼
┌──────────────────────┐
│ kubectl apply -f     │
│   TeamConfig XR      │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Crossplane           │
│ composition function │
│                      │
│ Fans out TeamConfig  │
│ members[] into       │
│ per-user             │
│ MachineConfig CRDs   │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ cfgd-operator        │
│ (watches CRDs)       │
│                      │
│ Validates, checks    │
│ compliance, tracks   │
│ drift                │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Device gateway       │
│                      │
│ Stores desired config│
│ Receives check-ins   │
│ Records drift        │
└──────────────────────┘
           ▲
           │ check-in
    ┌──────┴──────┐
    │ cfgd daemon │
    │ on each     │
    │ developer's │
    │ machine     │
    └─────────────┘
```

A platform engineer creates a TeamConfig. Crossplane generates one MachineConfig per team member. The operator reconciles those CRDs. Devices check in with the gateway and pull their config.

## Prerequisites

- [Crossplane](https://docs.crossplane.io/latest/software/install/) v2+ installed on the cluster
- cfgd-operator deployed (see [operator.md](operator.md))
- `function-cfgd` composition function installed:
  ```sh
  # Install from the published Crossplane package
  crossplane xpkg install function ghcr.io/tj-smith47/function-cfgd:v0.1.0
  ```

## TeamConfig XRD

```yaml
apiVersion: apiextensions.crossplane.io/v2
kind: CompositeResourceDefinition
metadata:
  name: teamconfigs.cfgd.io
spec:
  group: cfgd.io
  names:
    kind: TeamConfig
    plural: teamconfigs
  scope: Namespaced
  versions:
  - name: v1alpha1
    served: true
    referenceable: true
    schema:
      openAPIV3Schema:
        type: object
        properties:
          spec:
            type: object
            properties:
              team:
                type: string
              profile:
                type: string
              policy:
                type: object
                properties:
                  required:
                    type: object
                    x-kubernetes-preserve-unknown-fields: true
                  recommended:
                    type: object
                    x-kubernetes-preserve-unknown-fields: true
                  locked:
                    type: object
                    x-kubernetes-preserve-unknown-fields: true
                  requiredModules:
                    type: array
                    items:
                      type: string
                  recommendedModules:
                    type: array
                    items:
                      type: string
              members:
                type: array
                items:
                  type: object
                  properties:
                    username:
                      type: string
                    sshPublicKey:
                      type: string
                    profile:
                      type: string
                    hostname:
                      type: string
                  required: [username]
            required: [team, members]
```

## Creating a TeamConfig

```yaml
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: backend-team
  namespace: teams
spec:
  team: backend
  profile: backend-dev
  policy:
    required:
      packages: [git-secrets, pre-commit]
    requiredModules: [corp-vpn, corp-certs]
    recommendedModules: [approved-editor]
  members:
    - username: jdoe
      hostname: jdoe-macbook
    - username: asmith
      profile: backend-sre    # per-member override
    - username: bjones
```

## Composition

The composition wires TeamConfig to the `function-cfgd` composition function:

```yaml
apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: teamconfig-to-machineconfigs
spec:
  compositeTypeRef:
    apiVersion: cfgd.io/v1alpha1
    kind: TeamConfig
  mode: Pipeline
  pipeline:
  - step: generate-machine-configs
    functionRef:
      name: function-cfgd
```

## Composition Function (`function-cfgd`)

Go module using `function-sdk-go`. For each TeamConfig, the function:

1. Reads `spec.members[]` from the observed TeamConfig XR
2. Reads `spec.policy` (required/recommended/locked tiers)
3. For each member, generates a **MachineConfig** CRD:
   - `metadata.name` derived from `member.username`
   - `spec.hostname` from member (or empty, filled on first checkin)
   - `spec.profile` from member override or team default
   - `spec.packages`, `spec.files`, `spec.systemSettings` from policy tiers
   - `spec.moduleRefs` from `requiredModules` and `recommendedModules`
4. Generates **ConfigPolicy** CRDs from the team policy spec
5. Returns all desired resources via `response.SetDesiredComposedResources`

Packaged as a [Crossplane function package](https://docs.crossplane.io/latest/concepts/composition-functions/) via `crossplane xpkg build` and pushed to `ghcr.io`.

## What Gets Generated

For the backend-team example above, `function-cfgd` produces:

**3 MachineConfigs** (one per member):
```yaml
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: backend-team-jdoe
spec:
  hostname: jdoe-macbook
  profile: backend-dev
  moduleRefs:
    - name: corp-vpn
      required: true
    - name: corp-certs
      required: true
  packages: [git-secrets, pre-commit]
```

**1 ConfigPolicy**:
```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: backend-team-policy
spec:
  requiredModules: [corp-vpn, corp-certs]
  packages: [git-secrets, pre-commit]
  targetSelector:
    cfgd.io/team: backend
```

## Resource Lifecycle

When a team member is removed from the TeamConfig XR, the Crossplane composition function stops generating their MachineConfig. Crossplane's garbage collection handles cleanup of resources it no longer desires.

## Multi-Team Composition

A developer can be a member of multiple TeamConfigs. Each generates a MachineConfig. The operator merges applicable ConfigPolicies. On the device side, the developer subscribes to multiple [config sources](sources.md) with priority-based conflict resolution.

```
Engineer's machine:
  ├── acme-base (priority 400)     — company-wide baseline
  ├── acme-backend (priority 500)  — backend team tools
  └── security (priority 800)      — security team hardening
```

## Namespace-per-Team Model

Each team gets a namespace for their TeamConfig, ConfigPolicy, and MachineConfig resources. RBAC controls:
- Team leads: `edit` on their namespace
- Platform team: `cluster-admin` on `cfgd.io` API group
