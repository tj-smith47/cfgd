# Config Spec Reference

The Config document (`cfgd.yaml`) is the root configuration file for cfgd. It controls the active
profile, daemon behaviour, secret backend, remote sources, module registries, theming, and AI
integration.

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: string
spec:
  profile: string

  # Origin — where this config lives remotely (single object or list)
  origin:
    type: Git | Server
    url: string
    branch: string
    auth: string

  daemon:
    enabled: bool
    reconcile:
      interval: string
      onChange: bool
      autoApply: bool
      driftPolicy: Auto | NotifyOnly | Prompt
      policy:
        newRecommended: Notify | Accept | Reject | Ignore
        newOptional: Notify | Accept | Reject | Ignore
        lockedConflict: Notify | Accept | Reject | Ignore
      patches:
        - kind: Module | Profile
          name: string
          interval: string
          autoApply: bool
          driftPolicy: Auto | NotifyOnly | Prompt
    sync:
      autoPush: bool
      autoPull: bool
      interval: string
    notify:
      drift: bool
      method: Desktop | Stdout | Webhook
      webhookUrl: string

  secrets:
    backend: string
    sops:
      ageKey: path
    integrations:
      - name: string
        # provider-specific extra fields

  sources:
    - name: string
      origin:
        type: Git | Server
        url: string
        branch: string
        auth: string
      subscription:
        profile: string
        priority: uint
        acceptRecommended: bool
        optIn:
          - string
        overrides: {}
        reject: {}
      sync:
        interval: string
        autoApply: bool
        pinVersion: string

  modules:
    registries:
      - name: string
        url: string
    security:
      requireSignatures: bool

  security:
    allowUnsigned: bool

  fileStrategy: Symlink | Copy | Template | Hardlink

  aliases:
    alias-name: command string

  theme: string
  # or:
  theme:
    name: string
    overrides:
      success: string
      warning: string
      error: string
      info: string
      muted: string
      header: string
      subheader: string
      key: string
      value: string
      diffAdd: string
      diffRemove: string
      diffContext: string
      iconSuccess: string
      iconWarning: string
      iconError: string
      iconInfo: string
      iconPending: string
      iconArrow: string

  ai:
    provider: string
    model: string
    apiKeyEnv: string

  compliance:
    enabled: bool
    interval: string
    retention: string
    scope:
      files: bool
      packages: bool
      system: bool
      secrets: bool
      watchPaths:
        - string
      watchPackageManagers:
        - string
    export:
      format: json | yaml
      path: string
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Human-readable name for this configuration (e.g. `my-workstation`). |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `profile` | string | No | | Name of the active profile. Run `cfgd profile create <name>` to set. |
| `origin` | object or list | No | | Remote git or server origin(s) for this config. See [spec.origin](#specorigin). |
| `daemon` | object | No | | Daemon and reconciliation settings. See [spec.daemon](#specdaemon). |
| `secrets` | object | No | | Secret backend configuration. See [spec.secrets](#specsecrets). |
| `sources` | list | No | `[]` | Remote config sources to subscribe to. See [spec.sources[]](#specsources). |
| `modules` | object | No | | Module registry and security settings. See [spec.modules](#specmodules). |
| `security` | object | No | | Source signature verification overrides. See [spec.security](#specsecurity). |
| `fileStrategy` | enum | No | `Symlink` | Global default file deployment strategy. See [FileStrategy](#filestrategy-values). |
| `aliases` | map | No | `{}` | CLI aliases: map of alias name to command string. |
| `theme` | string or object | No | | Output theme name or detailed theme config. See [spec.theme](#spectheme). |
| `ai` | object | No | | AI assistant configuration. See [spec.ai](#specai). |
| `compliance` | object | No | | Continuous compliance snapshot settings. See [spec.compliance](#speccompliance). |

---

### spec.origin

Controls where this configuration lives remotely. Used by `cfgd sync` and the daemon sync loop.

Can be written as a single object or as a list (first entry is the primary origin).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `type` | enum | Yes | | Origin type. See [OriginType values](#origintype-values). |
| `url` | string | Yes | | Remote URL. For `Git`: a git clone URL. For `Server`: the device gateway base URL. |
| `branch` | string | No | `master` | Git branch to track. Only used when `type: Git`. |
| `auth` | string | No | | SSH key path or credential reference for authenticated access. |

#### OriginType values

| Value | Description |
|-------|-------------|
| `Git` | Git repository (SSH or HTTPS). |
| `Server` | cfgd device gateway HTTP endpoint. |

**Examples:**

Single origin (shorthand):
```yaml
origin:
  type: Git
  url: git@github.com:you/machine-config.git
  branch: main
```

Multiple origins (list form):
```yaml
origin:
  - type: Git
    url: git@github.com:you/machine-config.git
    branch: main
  - type: Server
    url: https://cfgd.example.com
```

---

### spec.daemon

Controls the long-running daemon process started with `cfgd daemon`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `enabled` | bool | No | `false` | Whether the daemon is active. |
| `reconcile` | object | No | | Reconciliation loop settings. See [spec.daemon.reconcile](#specdaemonreconcile). |
| `sync` | object | No | | Git sync settings. See [spec.daemon.sync](#specdaemonsync). |
| `notify` | object | No | | Notification settings. See [spec.daemon.notify](#specdaemonnotify). |

---

### spec.daemon.reconcile

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `interval` | string | No | `5m` | How often to check for drift. Duration string: `30s`, `5m`, `1h`. |
| `onChange` | bool | No | `false` | Also trigger reconciliation when config files change on disk (inotify/kqueue). |
| `autoApply` | bool | No | `false` | Automatically apply detected drift without user confirmation. |
| `driftPolicy` | enum | No | `NotifyOnly` | Governs what the daemon does when drift is detected. See [DriftPolicy values](#driftpolicy-values). |
| `policy` | object | No | | Fine-grained `autoApply` policy per change category. See [spec.daemon.reconcile.policy](#specdaemonreconcilepolicy). |
| `patches` | list | No | `[]` | Per-module or per-profile reconcile overrides. See [spec.daemon.reconcile.patches[]](#specdaemonreconcilepatches). |

#### DriftPolicy values

| Value | Description |
|-------|-------------|
| `Auto` | Silently apply drift corrections. Must be explicitly opted in to. |
| `NotifyOnly` | Notify and record drift but do not apply. User must run `cfgd apply`. **(default)** |
| `Prompt` | Notify with actionable prompt (future interactive mode). |

---

### spec.daemon.reconcile.policy

Fine-grained policy for different categories of `autoApply` decisions. All fields default to safe
values (notify or ignore).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `newRecommended` | enum | No | `Notify` | Action when a remote source pushes a new recommended item. |
| `newOptional` | enum | No | `Ignore` | Action when a remote source pushes a new optional item. |
| `lockedConflict` | enum | No | `Notify` | Action when a locked item conflicts with an incoming change. |

#### PolicyAction values

| Value | Description |
|-------|-------------|
| `Notify` | Send a notification but do not apply. |
| `Accept` | Accept and apply the change automatically. |
| `Reject` | Reject the change and record the conflict. |
| `Ignore` | Silently ignore the change. |

---

### spec.daemon.reconcile.patches[]

Kustomize-style per-target overrides for reconcile settings. Each patch targets all entities of the
given kind (when `name` is omitted) or a single named entity.

Precedence: Module patch > Profile patch > global reconcile settings.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `kind` | enum | Yes | | Target kind. `Module` or `Profile`. |
| `name` | string | No | | Name of the specific module or profile to patch. Omit to target all of that kind. |
| `interval` | string | No | | Override reconcile interval for this target. |
| `autoApply` | bool | No | | Override `autoApply` for this target. |
| `driftPolicy` | enum | No | | Override `driftPolicy` for this target. See [DriftPolicy values](#driftpolicy-values). |

**Example** — disable `autoApply` for a sensitive module while enabling it everywhere else:
```yaml
daemon:
  reconcile:
    autoApply: true
    patches:
      - kind: Module
        name: ssh-keys
        autoApply: false
        driftPolicy: NotifyOnly
```

---

### spec.daemon.sync

Controls automatic git synchronisation (push/pull) in the daemon sync loop.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `autoPush` | bool | No | `false` | Automatically push local changes to the remote origin after applying. |
| `autoPull` | bool | No | `false` | Automatically pull from the remote origin before reconciling. |
| `interval` | string | No | `1h` | How often to sync with the remote. Duration string: `30s`, `5m`, `1h`. |

---

### spec.daemon.notify

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `drift` | bool | No | `false` | Send notifications when drift is detected. |
| `method` | enum | No | `Desktop` | Notification delivery method. See [NotifyMethod values](#notifymethod-values). |
| `webhookUrl` | string | No | | Webhook URL. Required when `method: Webhook`. |

#### NotifyMethod values

| Value | Description |
|-------|-------------|
| `Desktop` | OS desktop notification (macOS: `osascript`, Linux: `notify-send`). **(default)** |
| `Stdout` | Print to stdout (useful for systemd journal or log aggregators). |
| `Webhook` | HTTP POST to `webhookUrl` with JSON payload. |

---

### spec.secrets

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `backend` | string | No | `sops` | Secret backend identifier. Built-in values: `sops`. |
| `sops` | object | No | | SOPS-specific configuration. See [spec.secrets.sops](#specsecretssops). |
| `integrations` | list | No | `[]` | Additional secret integrations (1Password, Bitwarden, Vault). See [spec.secrets.integrations[]](#specsecretsintegrations). |

---

### spec.secrets.sops

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `ageKey` | path | No | | Path to the age private key file. Supports `~/` expansion. |

---

### spec.secrets.integrations[]

Each entry enables an additional secret provider alongside the primary backend.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Integration identifier (e.g. `onepassword`, `bitwarden`, `vault`). |
| *(extra fields)* | any | No | | Provider-specific configuration fields merged inline. |

**Example:**
```yaml
secrets:
  backend: sops
  sops:
    ageKey: ~/.config/cfgd/age-key.txt
  integrations:
    - name: onepassword
      account: my.1password.com
```

---

### spec.sources[]

Remote config sources that cfgd subscribes to. Each source is a git repository or server endpoint
that publishes profiles and modules. See `docs/sources.md` for the full multi-source model.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Short name for this source (used in status output and overrides). |
| `origin` | object | Yes | | Where to fetch the source. Same structure as [spec.origin](#specorigin). |
| `subscription` | object | No | | How to subscribe to this source's content. See [spec.sources[].subscription](#specsourcessubscription). |
| `sync` | object | No | | Sync schedule and pinning. See [spec.sources[].sync](#specsourcessync). |

---

### spec.sources[].subscription

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `profile` | string | No | | Profile name from this source to activate. |
| `priority` | uint | No | `500` | Merge priority. Higher values win conflicts when multiple sources provide the same key. |
| `acceptRecommended` | bool | No | `false` | Automatically accept all items in the source's `recommended` policy tier. |
| `optIn` | list of string | No | `[]` | Explicit list of optional item names to opt in to from this source. |
| `overrides` | object | No | | Free-form YAML overrides merged on top of the source's profile after fetching. |
| `reject` | object | No | | Free-form YAML specifying items to reject from this source's output. |

---

### spec.sources[].sync

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `interval` | string | No | `1h` | How often to pull updates from this source. Duration string: `30s`, `5m`, `1h`. |
| `autoApply` | bool | No | `false` | Automatically apply changes from this source without user confirmation. |
| `pinVersion` | string | No | | Pin this source to a specific git ref (tag or commit SHA). Branches are not allowed. |

---

### spec.modules

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `registries` | list | No | `[]` | Git repositories that act as module registries. See [spec.modules.registries[]](#specmodulesregistries). |
| `security` | object | No | | Module-level signature enforcement. See [spec.modules.security](#specmodulessecurity). |

---

### spec.modules.registries[]

A module registry is a git repository with modules stored under `modules/<name>/module.yaml`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Short alias for this registry (defaults to GitHub org name when cloned). |
| `url` | string | Yes | | Git clone URL of the registry repository. |

**Example:**
```yaml
modules:
  registries:
    - name: acme
      url: git@github.com:acme-corp/cfgd-modules.git
```

---

### spec.modules.security

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `requireSignatures` | bool | No | `false` | Require GPG or SSH signatures on all remote module git tags. Unsigned modules are rejected unless `--allow-unsigned` is passed at the CLI. |

---

### spec.security

Global source security overrides. Intended for development and testing environments.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `allowUnsigned` | bool | No | `false` | Allow unsigned source content even when the source's `constraints.requireSignedCommits` is true. |

---

### FileStrategy values

Used by `spec.fileStrategy` (global default) and per-file `strategy` overrides in profile and
module file entries.

| Value | Description |
|-------|-------------|
| `Symlink` | Create a symbolic link from target to source file. **(default)** |
| `Copy` | Copy the source file content to the target path. |
| `Template` | Render the source as a Tera template and write the output. Auto-selected for `.tera` files. |
| `Hardlink` | Create a hard link from target to source. |

---

### spec.theme

Controls the visual output style of all cfgd commands. Can be written as a bare theme name string
or as an object with optional colour/icon overrides.

**Shorthand (string):**
```yaml
theme: dracula
```

**Full form:**
```yaml
theme:
  name: dracula
  overrides:
    success: "#50fa7b"
    error: "#ff5555"
```

#### spec.theme (object form)

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | No | `default` | Built-in theme name. |
| `overrides` | object | No | | Per-colour/icon overrides. See [spec.theme.overrides](#specthemeoverrides). |

---

### spec.theme.overrides

All fields are optional CSS-style hex colour strings (e.g. `#ff5555`) or single-character icon
strings. An omitted field inherits the value from the active theme.

| Field | Type | Description |
|-------|------|-------------|
| `success` | string | Colour for success messages and checkmarks. |
| `warning` | string | Colour for warnings. |
| `error` | string | Colour for errors. |
| `info` | string | Colour for informational output. |
| `muted` | string | Colour for de-emphasised (secondary) text. |
| `header` | string | Colour for section headers. |
| `subheader` | string | Colour for sub-section headers. |
| `key` | string | Colour for table/field key labels. |
| `value` | string | Colour for table/field values. |
| `diffAdd` | string | Colour for added lines in diffs. |
| `diffRemove` | string | Colour for removed lines in diffs. |
| `diffContext` | string | Colour for context lines in diffs. |
| `iconSuccess` | string | Icon character for success state. |
| `iconWarning` | string | Icon character for warning state. |
| `iconError` | string | Icon character for error state. |
| `iconInfo` | string | Icon character for info state. |
| `iconPending` | string | Icon character for pending/in-progress state. |
| `iconArrow` | string | Icon character for directional arrows (e.g. plan output). |

---

### spec.ai

AI assistant configuration for `cfgd generate` and the MCP server.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `provider` | string | No | `claude` | AI provider identifier. Currently `claude` (Anthropic) is supported. |
| `model` | string | No | `claude-sonnet-4-6` | Model identifier passed to the provider API. |
| `apiKeyEnv` | string | No | `ANTHROPIC_API_KEY` | Name of the environment variable that holds the API key. |

**Example:**
```yaml
ai:
  provider: claude
  model: claude-opus-4-5
  apiKeyEnv: ANTHROPIC_API_KEY
```

---

### spec.compliance

Continuous compliance snapshot configuration. When enabled, the daemon captures machine state on its own interval (independent of the reconcile interval) and writes structured snapshot files. Snapshots are content-hashed — if nothing changed since the last snapshot, no new file is written.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `enabled` | bool | No | `false` | Enable compliance snapshots. |
| `interval` | duration | No | `1h` | How often to capture a snapshot. Duration string: `30s`, `5m`, `1h`. |
| `retention` | duration | No | `720h` | How long to keep snapshots locally before the daemon deletes them. |
| `scope.files` | bool | No | `true` | Include managed file state (existence, permissions, encryption status). |
| `scope.packages` | bool | No | `true` | Include managed package state (installed version per manager). |
| `scope.system` | bool | No | `true` | Include system configurator state (covers `sshKeys`, `gpgKeys`, `git`, and all other configurators). |
| `scope.secrets` | bool | No | `true` | Include secret target existence and permissions. Secret values are never recorded. |
| `scope.watchPaths` | list | No | `[]` | Additional unmanaged paths to audit for existence, permissions, and ownership. |
| `scope.watchPackageManagers` | list | No | `[]` | Package managers from which to capture a full installed-package inventory. Runs in parallel across managers. |
| `export.format` | enum | No | `json` | Snapshot output format: `json` or `yaml`. |
| `export.path` | string | No | `~/.local/share/cfgd/compliance/` | Directory where snapshot files are written. |

**Example:**
```yaml
compliance:
  enabled: true
  interval: 1h
  retention: 720h
  scope:
    files: true
    packages: true
    system: true
    secrets: true
    watchPaths:
      - ~/.ssh
      - ~/.gnupg
      - ~/.aws
    watchPackageManagers:
      - brew
      - apt
  export:
    format: json
    path: ~/.local/share/cfgd/compliance/
```

Snapshot summaries are included in device checkin payloads to the operator gateway. The fleet dashboard shows per-device compliance scores. Use `cfgd compliance` to run a snapshot on demand, `cfgd compliance history` to list past snapshots, and `cfgd compliance diff <id1> <id2>` to compare two snapshots.
