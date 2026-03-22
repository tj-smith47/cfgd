# TeamConfig Spec Reference

`TeamConfig` is a Crossplane Composite Resource (XR) defined by the `teamconfigs.cfgd.io` XRD
(`apiextensions.crossplane.io/v2`). It represents the configuration distribution intent for a team:
one `TeamConfig` fans out into one `MachineConfig` per team member via the `function-cfgd`
Composition pipeline.

**API group:** `cfgd.io/v1alpha1`
**XRD kind:** `CompositeResourceDefinition` (`teamconfigs.cfgd.io`)
**Scope:** Namespaced
**Composition:** `teamconfig-to-machineconfigs` (pipeline mode, single step: `function-cfgd`)

Unlike the operator-managed CRDs (`MachineConfig`, `ConfigPolicy`, `DriftAlert`), `TeamConfig` has
no operator-managed `status` subresource. Status is managed by Crossplane's composition engine.

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: string
  namespace: string

spec:
  team: string
  profile: string

  source:
    url: string
    branch: string

  modules:
    - name: string
      sourceRef:
        url: string
        ref: string

  policy:
    requiredModules:
      - string
    recommendedModules:
      - string
    required: {}
    recommended: {}
    locked: {}

  members:
    - username: string
      sshPublicKey: string
      profile: string
      hostname: string
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Resource name. Conventionally the team slug (e.g. `team-platform`). |
| `namespace` | string | Yes | | Kubernetes namespace. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `team` | string | Yes | | Team identifier. Used to name generated `MachineConfig` resources (e.g. `<team>-<username>`). |
| `members` | list | Yes | | Team members who will each receive a generated `MachineConfig`. See [spec.members[]](#specmembers). |
| `profile` | string | No | | Default cfgd profile name for all team members. Individual members may override this. |
| `source` | object | No | | Git repository containing the team's cfgd config source. See [spec.source](#specsource). |
| `modules` | list | No | `[]` | Modules provided or pinned by this team config. See [spec.modules[]](#specmodules). |
| `policy` | object | No | | Policy tiers controlling what members can and cannot override. See [spec.policy](#specpolicy). |

---

### spec.source

Git repository from which the team's cfgd configuration is fetched by subscribed devices.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | string | Yes | | Git clone URL (SSH or HTTPS). |
| `branch` | string | No | `main` | Branch to track. |

**Example:**
```yaml
source:
  url: git@github.com:acme-corp/platform-config.git
  branch: main
```

---

### spec.modules[]

Modules that the team config declares, pins, or makes available to members. Each entry may carry a
`sourceRef` pointing to an external git repository.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Module name. |
| `sourceRef` | object | No | | Reference to a git repository containing this module. See [spec.modules[].sourceRef](#specmodulessourceref). |

#### spec.modules[].sourceRef

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | string | Yes | | Git clone URL of the repository containing the module. |
| `ref` | string | No | | Git ref (tag, branch, or commit SHA) to pin the module to. |

**Example:**
```yaml
modules:
  - name: kubectl
    sourceRef:
      url: git@github.com:acme-corp/cfgd-modules.git
      ref: v1.2.0
  - name: internal-vpn
```

---

### spec.policy

Policy tiers that constrain what team members can override in their generated `MachineConfig`
resources. The three structural tiers (`required`, `recommended`, `locked`) accept arbitrary
YAML objects (preserved via `x-kubernetes-preserve-unknown-fields: true`).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `requiredModules` | list of string | No | `[]` | Module names that all team members must have installed. The composition function injects these into every generated `MachineConfig.spec.moduleRefs` with `required: true`. |
| `recommendedModules` | list of string | No | `[]` | Module names recommended for team members. Injected with `required: false`; members may opt out. |
| `required` | object | No | | Arbitrary config items that subscribers cannot override or remove. Free-form YAML merged at the highest priority in generated configs. |
| `recommended` | object | No | | Arbitrary config items that subscribers receive by default but can override or reject. |
| `locked` | object | No | | Arbitrary config items that subscribers cannot modify or remove under any circumstances. Enforced by the composition function regardless of member overrides. |

**Example:**
```yaml
policy:
  requiredModules:
    - containerd
    - kubelet
  recommendedModules:
    - k9s
    - lens
  required:
    systemSettings:
      net.ipv4.ip_forward: "1"
  locked:
    systemSettings:
      kernel.dmesg_restrict: "1"
```

---

### spec.members[]

Each entry represents one team member. The composition function generates one `MachineConfig` per
member, merging team-level defaults with member-level overrides.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `username` | string | Yes | | Unique identifier for the team member. Used as the `<username>` suffix in the generated `MachineConfig` name. |
| `sshPublicKey` | string | No | | SSH public key for device identity verification during enrollment and check-in. |
| `profile` | string | No | | Profile override for this member. When set, takes precedence over `spec.profile`. Defaults to the team-level `spec.profile`. |
| `hostname` | string | No | | Machine hostname. If empty on creation, the device gateway fills this in on first check-in. |

**Example:**
```yaml
members:
  - username: alice
    sshPublicKey: "ssh-ed25519 AAAAC3Nz..."
    profile: work-macos
    hostname: alice-mbp
  - username: bob
    sshPublicKey: "ssh-ed25519 AAAAC3Nx..."
    hostname: bob-linux
```

---

## Composition Behaviour

When Crossplane reconciles a `TeamConfig`, it runs the `function-cfgd` pipeline step which:

1. Iterates `spec.members[]`.
2. For each member, generates a `MachineConfig` named `<spec.team>-<member.username>` in the same namespace.
3. Sets `MachineConfig.spec.hostname` from `member.hostname` (or leaves it empty for gateway fill-in).
4. Sets `MachineConfig.spec.profile` from `member.profile` if set, otherwise from `spec.profile`.
5. Injects `policy.requiredModules` as `moduleRefs` with `required: true`.
6. Injects `policy.recommendedModules` as `moduleRefs` with `required: false`.
7. Merges `policy.required` and `policy.locked` into the generated spec at the appropriate priority levels.

---

## Full Example

```yaml
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: team-platform
  namespace: cfgd-system
spec:
  team: platform
  profile: work
  source:
    url: git@github.com:acme-corp/platform-config.git
    branch: main
  modules:
    - name: kubectl
      sourceRef:
        url: git@github.com:acme-corp/cfgd-modules.git
        ref: v1.2.0
    - name: internal-vpn
  policy:
    requiredModules:
      - kubectl
      - internal-vpn
    recommendedModules:
      - k9s
    required:
      systemSettings:
        net.ipv4.ip_forward: "1"
    locked:
      systemSettings:
        kernel.dmesg_restrict: "1"
  members:
    - username: alice
      sshPublicKey: "ssh-ed25519 AAAAC3Nz..."
      profile: work-macos
      hostname: alice-mbp
    - username: bob
      sshPublicKey: "ssh-ed25519 AAAAC3Nx..."
      hostname: bob-linux
```

This produces two `MachineConfig` resources: `platform-alice` and `platform-bob`, each in the
`cfgd-system` namespace, with the required modules, locked system settings, and member-specific
profile and hostname filled in.
