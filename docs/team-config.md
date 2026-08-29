# Team Config Distribution

How a platform engineer distributes and enforces team configuration across developer machines using [Crossplane](https://www.crossplane.io/). Builds on the [cfgd-operator](operator.md) CRDs. For the complete field-by-field reference, see the [TeamConfig spec reference](spec/teamconfig.md).

[Crossplane](https://docs.crossplane.io/latest/) is a Kubernetes framework for defining custom composite resources. In cfgd's case, a platform engineer defines a single TeamConfig resource listing team members, and Crossplane's composition function generates one MachineConfig CRD per team member: no manual YAML per developer.

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
  crossplane xpkg install function ghcr.io/tj-smith47/function-cfgd:v0.9.0
  ```
  The tag is the cfgd release the function ships with; check the one you run with
  `cfgd --version`.

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
              source:
                type: object
                properties:
                  url:
                    type: string
                  branch:
                    type: string
                required: [url]
              modules:
                type: array
                items:
                  type: object
                  properties:
                    name:
                      type: string
                    sourceRef:
                      type: object
                      properties:
                        url:
                          type: string
                        ref:
                          type: string
                  required: [name]
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
      packages:
        brew: [git-secrets, pre-commit]
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
3. For each member, generates a **MachineConfig**:
   - `metadata.generateName: <team>-<username>-`, labeled `cfgd.io/team` and `cfgd.io/username`
   - `spec.hostname` from the member, or the placeholder `pending-<username>` until the device checks in and reports its real hostname
   - `spec.profile` from the member override or the team default; a member with neither is an error
   - `spec.packages`, `spec.files`, `spec.systemSettings` collected from all policy tiers (locked wins dedup, then required, then recommended)
   - `spec.moduleRefs` from `spec.modules` plus `requiredModules` and `recommendedModules`; only `requiredModules` entries get `required: true`
4. Generates one **ConfigPolicy** per policy tier (`required`, `locked`) that has enforceable content
5. Returns all desired resources via `response.SetDesiredComposedResources`

Packaged as a [Crossplane function package](https://docs.crossplane.io/latest/concepts/composition-functions/) via `crossplane xpkg build` and pushed to `ghcr.io`.

## What Gets Generated

For the backend-team example above, `function-cfgd` produces:

**3 MachineConfigs** (one per member; jdoe's shown):
```yaml
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  generateName: backend-jdoe-
  labels:
    cfgd.io/team: backend
    cfgd.io/username: jdoe
spec:
  hostname: jdoe-macbook
  profile: backend-dev
  moduleRefs:
    - name: corp-vpn
      required: true
    - name: corp-certs
      required: true
    - name: approved-editor
      required: false
  packages:
    - name: git-secrets
    - name: pre-commit
```

asmith and bjones declared no `hostname`, so their MachineConfigs carry the placeholders `pending-asmith` and `pending-bjones` until their devices check in.

**1 ConfigPolicy** (the `required` tier; the `locked` tier is empty here, so none is generated for it):
```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  generateName: backend-required-
  labels:
    cfgd.io/team: backend
    cfgd.io/tier: required
spec:
  requiredModules:
    - name: corp-vpn
    - name: corp-certs
  packages:
    - name: git-secrets
    - name: pre-commit
  targetSelector:
    matchLabels:
      cfgd.io/team: backend
```

## Resource Lifecycle

When a team member is removed from the TeamConfig XR, the Crossplane composition function stops generating their MachineConfig. Crossplane's garbage collection handles cleanup of resources it no longer desires.

## Multi-Team Composition

A developer can be a member of multiple TeamConfigs. Each generates a MachineConfig. The operator merges applicable ConfigPolicies. On the device side, the developer subscribes to multiple [config sources](sources.md) with priority-based conflict resolution.

```
Engineer's machine:
  ├── acme-base (priority 400)     company-wide baseline
  ├── acme-backend (priority 500)  backend team tools
  └── security (priority 800)      security team hardening
```

## Namespace-per-Team Model

Each team gets a namespace for their TeamConfig, ConfigPolicy, and MachineConfig resources. RBAC controls:
- Team leads: `edit` on their namespace
- Platform team: `cluster-admin` on `cfgd.io` API group
