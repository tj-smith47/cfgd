# Compliance-as-Code for Developer Workstations

Design spec for cfgd's compliance capabilities: file encryption enforcement, secret-backed environment injection, key/credential provisioning, and continuous compliance snapshots.

## Market Context

No tool today continuously enforces and remediates developer-toolchain-level security configuration. MDM tools (Jamf, Intune) operate at the OS policy level. Compliance platforms (Vanta, Drata) detect and report but don't remediate. 1Password Device Trust (Kolide) checks posture but the remediation path is "send a Slack message and hope." cfgd's daemon, policy tiers, and drift detection make it the natural remediation engine that completes this loop.

---

## 1. File Encryption Enforcement

### Problem

Users have no way to declare "this file must be encrypted in the repo." Security teams have no way to enforce "any file targeting `~/.ssh/*` must be encrypted." The existing `backend:` field on secrets says how to decrypt — it doesn't enforce that encryption is required.

### Design

Add `encryption` and `permissions` fields to file entries in `files.managed`. The existing `files.permissions` section is unchanged — it serves a different purpose (enforcing permissions on paths cfgd does not manage as files). Documentation must clearly distinguish the two: `files.permissions` is for unmanaged paths; the per-file `permissions` field is for managed file entries.

#### Profile / Module file entry (new fields)

```yaml
files:
  managed:
    - source: ssh/config
      target: ~/.ssh/config
      permissions: "600"
      encryption:
        backend: sops            # sops | age
        mode: InRepo             # InRepo (default) | Always
    - source: shell/.zshrc
      target: ~/.zshrc
      # no encryption block = no enforcement
  permissions:
    "~/.ssh": "700"              # unmanaged path enforcement
    "~/.gnupg": "700"
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `encryption.backend` | string | Yes (when `encryption` present) | | `"sops"` or `"age"`. Same values and validation as `secrets.backend` in cfgd.yaml. |
| `encryption.mode` | enum | No | `InRepo` | `InRepo`: source must be encrypted in repo, deployed decrypted. `Always`: encrypted in repo AND at target. |
| `permissions` | string | No | | Octal permission mode for this file (e.g. `"600"`). This field on `ManagedFileSpec` does not collide with `FilesSpec.permissions` — they are different structs at different nesting depths. |

#### ConfigSource policy enforcement

Encryption enforcement belongs under `policy.constraints` alongside other enforcement rules (`allowedTargetPaths`, `noScripts`, etc.):

```yaml
policy:
  constraints:
    encryption:
      requiredTargets:
        - "~/.ssh/*"
        - "~/.aws/*"
        - "~/.config/corp/*"
      backend: sops              # optional: mandate specific backend
      mode: InRepo               # optional: mandate mode
```

#### Behavior

- User sets `encryption` on a file entry: reconciler checks that the source file is encrypted with the specified backend. Plan fails with a clear error if not.
- ConfigSource sets `constraints.encryption.requiredTargets`: composition engine checks all files (from any origin — profile or module) whose target matches the globs. Files without an `encryption` block that match a required target are rejected at composition time.
- `mode: Always`: same repo check, plus the target file remains encrypted on disk.
- `mode: InRepo` (default): source must be encrypted, target is deployed decrypted.

#### Edge cases

- A file matches both a user `encryption` block and a ConfigSource `constraints.encryption.requiredTargets` glob: ConfigSource wins on conflict (locked semantics). If both specify backend and they disagree, ConfigSource wins.
- A module ships a file that matches `requiredTargets` but has no encryption: composition error, not a runtime error. The module author must encrypt the source.
- Template files (`strategy: Template`) with `encryption`: source is encrypted in repo, decrypted before template rendering, rendered output deployed per `mode`.
- `mode: Always` with `strategy: Symlink` or `strategy: Hardlink`: error — symlinks and hardlinks cannot have independently encrypted content. Must use `Copy` or `Template`.

### Struct changes

- `ManagedFileSpec` gains `encryption: Option<EncryptionSpec>` and `permissions: Option<String>`.
- New struct `EncryptionSpec { backend: String, mode: Option<EncryptionMode> }`.
- New enum `EncryptionMode { InRepo, Always }` with `InRepo` as default.
- `ConstraintsSpec` gains `encryption: Option<EncryptionConstraint>`.
- New struct `EncryptionConstraint { required_targets: Vec<String>, backend: Option<String>, mode: Option<EncryptionMode> }`.

---

## 2. Secret-Backed Environment Injection

### Problem

There's no way to declare "resolve this secret from 1Password/Vault/Bitwarden and inject its value into the shell environment." Users must use file template workarounds.

### Design

Add an optional `envs` field to secret entries. When present, cfgd resolves the secret and writes the values to its managed shell env file alongside regular `env:` entries.

#### Profile / Module secret entry (new field)

```yaml
secrets:
  # Existing behavior — resolve to a file
  - source: 1password://Work/GitHub/token
    target: ~/.config/gh/token

  # New — resolve and inject into shell env
  - source: 1password://Work/GitHub/token
    envs:
      - GITHUB_TOKEN

  # Both — file AND env
  - source: vault://secret/data/api#key
    target: ~/.config/api-key
    envs:
      - API_KEY

  # Multiple env vars from one provider — use explicit field references
  - source: vault://secret/data/aws#aws_access_key_id
    envs:
      - AWS_ACCESS_KEY_ID
  - source: vault://secret/data/aws#aws_secret_access_key
    envs:
      - AWS_SECRET_ACCESS_KEY
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `envs` | list of strings | No | | Environment variable names to inject with the resolved secret value. |

- `target` becomes `Option<PathBuf>` (currently `PathBuf`). At least one of `target` or `envs` must be set — enforced by a custom serde deserialize validator that rejects entries where both are `None`.
- When `envs` has multiple entries and the source resolves to a single value, all env vars get the same value.
- Multi-field secrets (e.g., Vault paths with multiple keys) require explicit per-field source references (e.g., `vault://secret/data/aws#aws_access_key_id`). There is no positional mapping — each source resolves to exactly one value.
- The daemon refreshes secret-backed env vars on each reconcile cycle.
- Compliance snapshots record that the env var exists and its source, never the value.

#### ConfigSource policy

`PolicyItems` gains a new `secrets: Vec<SecretSpec>` field, available in all four policy tiers (locked, required, recommended, optional):

```yaml
policy:
  required:
    secrets:
      - source: 1password://Work/VPN/cert
        target: ~/.config/corp/vpn-cert.pem
      - source: vault://secret/corp/signing-key
        envs:
          - CORP_SIGNING_KEY
```

### Struct changes

- `SecretSpec.target` changes from `PathBuf` to `Option<PathBuf>`.
- `SecretSpec` gains `envs: Option<Vec<String>>`.
- Custom deserialization validator ensures at least one of `target` or `envs` is present.
- `PolicyItems` gains `secrets: Vec<SecretSpec>`.

---

## 3. Key & Credential Provisioning

### Problem

cfgd can manage existing files but cannot create keys. To tell the compliance story — "every developer machine must have an ed25519 SSH key with git signing configured" — cfgd needs to declare keys as desired state and create them if absent.

### Design

Three new system configurators: `sshKeys`, `gpgKeys`, and `git`. Each is independently useful. `ModuleSpec` gains an optional `system` field (currently only on profiles) to enable compliance modules that bundle key provisioning + git config.

#### `sshKeys` configurator

```yaml
system:
  sshKeys:
    - name: default
      type: ed25519              # ed25519 | rsa
      bits: 4096                 # rsa only, ignored for ed25519
      path: ~/.ssh/id_ed25519   # default based on type
      comment: "jane@work.com"
      passphrase: 1password://Work/SSH/passphrase  # optional, secret ref
      permissions: "600"         # default
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Identifier for this key entry |
| `type` | enum | No | `ed25519` | `ed25519` or `rsa` |
| `bits` | int | No | `4096` | RSA key size. Ignored for ed25519. |
| `path` | string | No | `~/.ssh/id_<type>` | Path to the private key |
| `comment` | string | No | | Key comment (typically email) |
| `passphrase` | string | No | | Secret provider reference for passphrase. Plaintext passphrases are not supported — must be a provider URI or omitted (no passphrase). |
| `permissions` | string | No | `"600"` | Private key file permissions |

**Behavior:**
- Key at `path` doesn't exist: generate via `ssh-keygen`. Public key written to `<path>.pub`.
- Key exists: verify type and permissions match. Key type verification uses the public key file (`<path>.pub`), not the private key, to avoid passphrase prompts during drift checks.
- Drift = wrong permissions, wrong key type, or missing key.
- Parent directory (`~/.ssh`) created with `700` permissions if absent.
- Passphrase resolved from secret provider at generation time only.

#### `gpgKeys` configurator

```yaml
system:
  gpgKeys:
    - name: work-signing
      type: ed25519              # ed25519 | rsa4096
      realName: "Jane Doe"
      email: jane@work.com
      expiry: 2y                 # gpg expiry notation
      usage: sign                # sign | encrypt | auth | sign,encrypt
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Identifier for this key entry |
| `type` | enum | No | `ed25519` | `ed25519` or `rsa4096` |
| `realName` | string | Yes | | GPG uid real name |
| `email` | string | Yes | | GPG uid email |
| `expiry` | string | No | `2y` | GPG expiry notation (`0` = no expiry) |
| `usage` | string | No | `sign` | Comma-separated capabilities |

**Behavior:**
- Key matching is on the primary UID email and usage capabilities. Revoked keys are ignored.
- No matching key in keyring: generate via `gpg --batch --gen-key` with a parameter file.
- Key exists but expired: drift. Key exists and valid: compliant.
- Key fingerprint is discoverable via `cfgd status` for use in git config.

#### `git` configurator

```yaml
system:
  git:
    user.name: "Jane Doe"
    user.email: jane@work.com
    user.signingKey: ~/.ssh/id_ed25519.pub
    commit.gpgSign: true
    gpg.format: ssh
    init.defaultBranch: main
```

The `git` configurator uses dotted-key-value pairs deliberately (not nested YAML). This matches `git config`'s internal model. The configurator parses each key literally, not as a nested path. Every key maps directly to `git config --global <key> <value>`.

**Behavior:**
- For each key: read current value via `git config --global --get <key>`. If it differs from desired, set it.
- Drift = any managed key has a different value than declared.
- Keys not declared by cfgd are not touched.

#### ConfigSource policy

```yaml
policy:
  required:
    system:
      sshKeys:
        - type: ed25519
      git:
        commit.gpgSign: true
        gpg.format: ssh
```

#### Compliance module example

A team ships a module that bundles all three. This requires `ModuleSpec` to gain an optional `system` field:

```yaml
# modules/corp-signing/module.yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: corp-signing
spec:
  system:
    sshKeys:
      - name: corp
        type: ed25519
        comment: "jane@work.com"
    git:
      commit.gpgSign: true
      gpg.format: ssh
      user.signingKey: ~/.ssh/id_ed25519.pub
```

Note: system configurator values do not support Tera template expansion. Use literal values in system fields. Dynamic values (like email) should be set via profile-level system config that inherits from env vars, not from module system config.

### Struct changes

- `ModuleSpec` gains `system: Option<HashMap<String, serde_yaml::Value>>`, matching the existing pattern on `ProfileSpec`.
- Module merge logic extended to deep-merge `system` values from modules into the profile's system config (module wins on conflict, consistent with other merge rules).
- Three new `SystemConfigurator` implementations: `SshKeysConfigurator`, `GpgKeysConfigurator`, `GitConfigurator`.

---

## 4. Continuous Compliance Snapshots

### Problem

cfgd can verify managed state and detect drift, but produces no structured evidence for compliance auditing. Platform/security teams need continuous proof that machines meet policy, exportable to external systems.

### Design

The daemon continuously captures machine state and writes full snapshots when state changes. Snapshots are stored locally and exportable via CLI. Fleet-wide, the operator aggregates snapshot summaries from device checkins.

#### Configuration

New `compliance` field on `ConfigSpec`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  compliance:
    enabled: true
    interval: 1h                  # snapshot interval (independent of reconcile)
    retention: 720h               # local snapshot retention (parse_duration_str needs 'd' support, or use hours)
    scope:
      files: true                 # managed file state
      packages: true              # managed package state
      system: true                # system configurator state
      secrets: true               # secret targets exist + permissions (never values)
      watchPaths:                 # additional unmanaged paths to audit
        - ~/.ssh
        - ~/.gnupg
        - ~/.aws
      watchPackageManagers:       # full inventory from these managers
        - brew
        - apt
    export:
      format: json                # json | yaml
      path: ~/.local/share/cfgd/compliance/
```

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `enabled` | bool | No | `false` | Enable compliance snapshots |
| `interval` | duration | No | `1h` | How often to snapshot |
| `retention` | duration | No | `720h` | How long to keep snapshots locally |
| `scope.files` | bool | No | `true` | Include managed file state |
| `scope.packages` | bool | No | `true` | Include managed package state |
| `scope.system` | bool | No | `true` | Include system configurator state |
| `scope.secrets` | bool | No | `true` | Include secret target existence/permissions |
| `scope.watchPaths` | list | No | `[]` | Additional unmanaged paths to audit |
| `scope.watchPackageManagers` | list | No | `[]` | Full package inventory from these managers |
| `export.format` | enum | No | `json` | `json` or `yaml` |
| `export.path` | string | No | `~/.local/share/cfgd/compliance/` | Snapshot output directory |

Note: `parse_duration_str` should be extended to support `d` (days) notation for retention values. Until then, use hours.

#### CLI

```sh
cfgd compliance                        # run snapshot now, print summary
cfgd compliance export                 # run + write to export path
cfgd compliance export -o json         # write to stdout as json
cfgd compliance history                # list past snapshots
cfgd compliance history --since 7d     # filtered
cfgd compliance diff <id1> <id2>       # what changed between two snapshots
```

#### Snapshot structure

Check categories map to cfgd's existing concepts: `file`, `package`, `system` (covers all system configurators including sshKeys, gpgKeys, git), `secret`, and `watchPath`. System configurator checks use `key` with a `<configurator>:<identifier>` format.

```json
{
  "timestamp": "2026-03-24T14:30:00Z",
  "machine": { "hostname": "janes-mbp", "os": "macos", "arch": "aarch64" },
  "profile": "work",
  "sources": ["acme-corp"],
  "checks": [
    {
      "category": "file",
      "target": "~/.ssh/config",
      "status": "compliant",
      "detail": "encrypted(sops), permissions 600"
    },
    {
      "category": "package",
      "name": "git-secrets",
      "status": "compliant",
      "version": "1.3.0",
      "manager": "brew"
    },
    {
      "category": "system",
      "key": "git:commit.gpgSign",
      "status": "compliant",
      "value": "true"
    },
    {
      "category": "system",
      "key": "sshKeys:~/.ssh/id_ed25519",
      "status": "compliant",
      "detail": "type ed25519, permissions 600"
    },
    {
      "category": "system",
      "key": "gpgKeys:jane@work.com",
      "status": "compliant",
      "detail": "ed25519, usage sign, expires 2028-03-24"
    },
    {
      "category": "watchPath",
      "path": "~/.aws/credentials",
      "status": "warning",
      "detail": "unmanaged, permissions 644"
    }
  ],
  "summary": { "compliant": 42, "warning": 2, "violation": 0 }
}
```

#### Daemon integration

- The daemon runs compliance snapshots on its own interval, independent of the reconcile interval.
- Snapshots are content-hashed. If nothing changed since the last snapshot, no new file is written.
- Snapshot summaries are included in device checkin payloads to the operator gateway.
- Fleet dashboard shows per-device compliance scores.
- Retention is enforced by the daemon — snapshots older than `retention` are deleted.
- Package manager inventory (`watchPackageManagers`) is cached between snapshots and refreshed only when the compliance interval fires. Inventory commands run in parallel across managers.

#### Export extensibility

The MVP is file output + stdout. Future extensibility for push-based integrations:

```yaml
    export:
      format: json
      path: ~/.local/share/cfgd/compliance/
      targets:                          # future, not in MVP
        - type: webhook
          url: https://api.vanta.com/v1/evidence
          headers:
            Authorization: "Bearer ${secret:vault://corp/vanta-token}"
```

This follows the same pattern as the existing notification system (`Desktop | Stdout | Webhook`), so the extension path is natural when demand materializes.

### Struct changes

- `ConfigSpec` gains `compliance: Option<ComplianceConfig>`.
- New structs: `ComplianceConfig`, `ComplianceScope`, `ComplianceExport`.
- New `ComplianceSnapshot` struct for serialization.
- StateStore gains a `compliance_snapshots` table for local history.

---

## Implementation Order

These four capabilities are independent and can be built incrementally:

1. **File encryption enforcement** — extends existing `ManagedFileSpec` and composition engine
2. **Secret-backed env injection** — extends existing `SecretSpec` and env file writer
3. **Key provisioning (sshKeys, gpgKeys, git)** — three new system configurators following established patterns; `ModuleSpec` gains `system` field
4. **Compliance snapshots** — new subsystem, depends on 1-3 being queryable but can stub initially

Each builds on existing patterns (config structs, provider traits, reconciler actions) and doesn't require new architectural concepts.
