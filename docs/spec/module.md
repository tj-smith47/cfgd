# Module Spec Reference

A Module document is a self-contained, portable configuration package for one tool or capability.
It bundles cross-platform package declarations, config files, environment variables, shell aliases,
and lifecycle scripts into a single deployable unit. Modules live in `modules/` in your
config directory, or in a remote module registry.

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: string
  description: string

spec:
  depends:
    - string

  packages:
    - name: string
      minVersion: string
      prefer:
        - string
      aliases:
        manager-name: package-name
      script: string
      deny:
        - string
      platforms:
        - string

  files:
    - source: string
      target: string
      strategy: Symlink | Copy | Template | Hardlink
      private: bool

  env:
    - name: string
      value: string

  aliases:
    - name: string
      command: string

  scripts:
    preApply:
      - string | { run: string, shell: string, timeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool }
    postApply:
      - string | { run: string, shell: string, timeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool }
    preReconcile:
      - string | { run: string, shell: string, timeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool }
    postReconcile:
      - string | { run: string, shell: string, timeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool }
    onChange:
      - string | { run: string, shell: string, timeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool }
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Module name. Must be unique within a registry. Referenced by profiles via `spec.modules`. |
| `description` | string | No | | Human-readable description of what this module provides. |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `depends` | list of string | No | `[]` | Other module names this module depends on. Dependency modules are applied first. |
| `platforms` | list of string | No | `[]` | Platform filter for the whole module. When set and the current platform matches none, the entire module is skipped. See [spec.platforms[]](#specplatforms). |
| `packages` | list | No | `[]` | Cross-platform package declarations. See [spec.packages[]](#specpackages). |
| `files` | list | No | `[]` | Files to deploy from the module directory to the machine. See [spec.files[]](#specfiles). |
| `env` | list | No | `[]` | Environment variables to export. See [spec.env[]](#specenv). |
| `aliases` | list | No | `[]` | Shell aliases to install. See [spec.aliases[]](#specaliases). |
| `system` | map | No | `{}` | System configurator settings. Keys are configurator names, values are configurator-specific config. Same schema as profile `spec.system`. See [spec.system](#specsystem). |
| `scripts` | object | No | | Lifecycle scripts. See [spec.scripts](#specscripts). |

---

### spec.depends[]

A list of module names that must be applied before this module. cfgd resolves the full dependency
graph, detects cycles, and applies modules in topological order.

**Example:**
```yaml
spec:
  depends:
    - node
    - python
```

---

### spec.platforms[]

A platform filter gating the **whole module**. When `platforms` is non-empty and the current
platform matches none of the listed tags, the module is skipped in its entirety — its packages,
files, scripts, env, and aliases are all omitted. Tags match against the platform's OS
(`linux`, `macos`, `freebsd`, `windows`), distro (`ubuntu`, `fedora`, `arch`, ...), or
architecture (`x86_64`, `aarch64`). The canonical macOS token is `macos` (not `darwin`). Omit
the field to apply the module on every platform.

This is the module-level analogue of per-package [`spec.packages[].platforms`](#specpackages):
use `spec.platforms` when an entire module is platform-specific, and the per-package filter when
only some packages within an otherwise cross-platform module are.

**Skip behavior:** a platform-skipped module is not silently dropped. It appears in the plan as a
**Skipped** action, so it is always visible that the module was gated out rather than missing.

**Dependency rule:** an active module may not `depends` on a module that is skipped on the current
platform. Doing so is a configuration error (the dependency would never be applied). Gate the
dependent module with the same `platforms` if it should also be platform-specific.

**Example — a macOS-only module:**
```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: mac-desktop
spec:
  platforms: [macos]
  packages:
    - name: rectangle
  system:
    macosDefaults:
      com.apple.dock:
        autohide: true
```

---

### spec.packages[]

Cross-platform package declarations. Each entry describes one logical package and how to install it
across different package managers. cfgd selects the best available manager for the current machine
based on `prefer` order and platform availability.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Canonical package name. Used as the install name unless overridden by `aliases`. |
| `minVersion` | string | No | | Minimum acceptable installed version (semver, 1-3 part). cfgd skips installation if a newer version is already present. |
| `prefer` | list of string | No | | Ordered list of package managers to try. cfgd tries each in order and uses the first available. The special value `"script"` directs cfgd to run the `script` field. When omitted, the platform's default manager is used. |
| `aliases` | map | No | `{}` | Per-manager name overrides. Key is the manager name, value is the package name to use with that manager. Use when a package has different names across managers. |
| `script` | string | No | | Inline shell script or path to a script. Executed when `"script"` appears in `prefer`. |
| `deny` | list of string | No | `[]` | Package manager names that must not be used for this package, even if available. |
| `platforms` | list of string | No | `[]` | Platform filter. When set, this entry is skipped on non-matching platforms. Values: OS (`linux`, `macos`), distro (`ubuntu`, `fedora`, `arch`), or architecture (`x86_64`, `aarch64`). Omit to match all platforms. |

**Example — cross-platform tool with manager aliases:**
```yaml
packages:
  - name: neovim
    minVersion: "0.9"
    prefer: [brew, snap, apt]
    aliases:
      snap: nvim

  - name: fd
    aliases:
      apt: fd-find
      dnf: fd-find

  - name: pynvim
    prefer: [pipx]
```

**Example — platform-specific entry:**
```yaml
packages:
  - name: xdg-utils
    platforms: [linux]

  - name: open
    platforms: [macos]
```

**Example — custom install script:**
```yaml
packages:
  - name: my-tool
    prefer: [script]
    script: |
      curl -fsSL https://example.com/install.sh | sh
```

---

### spec.files[]

Files (or directories) to deploy from the module directory to the machine. Module files use the
same deployment strategies as profile files. Paths are resolved relative to the module directory.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `source` | string | Yes | | Path to the source file or directory, relative to the module directory. Also accepts a git URL with `@ref` suffix (e.g. `https://github.com/user/nvim-config.git@v2.1.0`) to clone a remote source. |
| `target` | string | Yes | | Absolute destination path on the machine. Supports `~/` expansion. |
| `strategy` | enum | No | Global `fileStrategy` | Deployment strategy for this file. Overrides the global default from `cfgd.yaml`. See [FileStrategy values](#filestrategy-values). |
| `private` | bool | No | `false` | When `true`, the source file is local-only: automatically added to `.gitignore` and silently skipped on machines where it does not exist. |
| `permissions` | string | No | | Octal permission mode to enforce on the deployed target file (e.g. `"755"`). Applied after deployment; ignored on Windows (NTFS uses inherited ACLs). |

**Example:**
```yaml
files:
  - source: config/
    target: ~/.config/nvim/

  - source: https://github.com/user/nvim-config.git@v2.1.0
    target: ~/.config/nvim/

  - source: local-overrides.lua
    target: ~/.config/nvim/local.lua
    strategy: Copy
    private: true

  - source: bin/git-helper
    target: ~/.local/bin/git-helper
    strategy: Copy
    permissions: "755"
```

#### FileStrategy values

| Value | Description |
|-------|-------------|
| `Symlink` | Create a symbolic link from `target` to the source file. **(default)** |
| `Copy` | Copy source content to `target`. The target is independent; changes to source are not reflected until the next reconcile. |
| `Template` | Render the source as a Tera template and write the output to `target`. Automatically selected for `.tera` source files. |
| `Hardlink` | Create a hard link from `target` to source. Changes to either file are immediately visible in both. |

---

### spec.env[]

Environment variables to export into the shell environment. These are merged with the activating
profile's env vars during reconciliation. On a name conflict, the module's value takes precedence
over the profile's value.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Environment variable name. |
| `value` | string | Yes | | Value to assign. |

**Example:**
```yaml
env:
  - name: EDITOR
    value: nvim
  - name: NVIM_APPNAME
    value: my-nvim
```

---

### spec.aliases[]

Shell aliases to install. Merged with profile aliases during reconciliation; module values win on
name conflicts.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Alias name (the command you type). |
| `command` | string | Yes | | Shell command the alias expands to. |

**Example:**
```yaml
aliases:
  - name: vim
    command: nvim
  - name: vi
    command: nvim
```

---

### spec.system

System configurator settings for this module. Keys map to configurator names; values are passed directly to the configurator. Follows the same schema as `spec.system` in profiles — see `docs/system-configurators.md` for the full list of available configurators.

Module system values are deep-merged into the activating profile's system config during reconciliation. Module values win on conflict, consistent with other merge rules.

Note: system configurator values do not support Tera template expansion. Use literal values in module system config. Dynamic values (such as email addresses) should be set via profile-level system config.

**Example:**
```yaml
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

---

### spec.scripts

Lifecycle scripts executed at different points during module apply and reconciliation.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `preApply` | list | No | `[]` | Run before the module's packages and files are applied. Failure aborts. |
| `postApply` | list | No | `[]` | Run after the module is fully applied. |
| `preReconcile` | list | No | `[]` | Run before daemon-initiated reconciliation of this module. |
| `postReconcile` | list | No | `[]` | Run after daemon-initiated reconciliation of this module. |
| `onChange` | list | No | `[]` | Run after apply/reconcile only if this module's resources changed. |
| `onDrift` | list | No | `[]` | Run in the daemon when drift is detected in this module's own resources, before the drift policy decides how to respond. Observability, not remediation. Fires on both whole-profile and per-module reconcile ticks. |

Each entry can be a simple string or a full object with `run`, `shell`, `timeout`, `continueOnError`, `interactive`, and the idempotency guards `onlyIf`, `unless`, and `creates`.

The `shell` field selects the interpreter for inline commands: `bash`, `zsh`, `sh`, `pwsh`, `cmd`, or `auto` (default). `auto` uses `sh` on Unix and `cmd.exe` on Windows. `shell` only applies to inline commands; file scripts use their shebang.

When `shell` is `bash` or `zsh`, the script automatically sources `~/.cfgd.env` before execution, making all resolved `spec.env` vars and `spec.aliases` available (with alias expansion enabled). See [Lifecycle Scripts](../lifecycle-scripts.md) for details.

### Idempotency guards

The guards make a script re-run-safe by construction, so authors no longer need to hand-roll `command -v x && exit 0`. They are evaluated **before** the script body, in this order; any guard that says "skip" skips the body and reports `changed=false` with a `Skipped` status line naming the guard:

| Field | Type | Skips the body when… |
|---|---|---|
| `creates` | string (path) | the path already exists |
| `onlyIf` | string (command) | the command exits **non-zero** (the condition to run is not met) |
| `unless` | string (command) | the command exits **zero** (the guarded state already holds) |

When more than one guard is set, **all** must permit running for the body to run. `onlyIf`/`unless` commands run with the same shell, working directory, and environment as the body, bounded by a timeout so a guard can never hang. A guard command that fails to spawn (e.g. a missing interpreter) is a hard error, distinct from a non-zero exit.

`creates` path resolution: a leading `~` expands to the home directory; a relative path resolves against the script's working directory (the module directory for module scripts); an absolute path is used as-is. Existence follows symlinks.

### Interactive scripts

Set `interactive: true` on a script entry that needs to prompt the user — for example, pausing until a manual step is done. The script runs **attached to the terminal** (inherited stdin/stdout/stderr, no spinner, no output capture) and is **not** subject to the idle timeout, because an interactive step is attended by definition.

An interactive script requires a TTY. When stdin is **not** a terminal — CI, piped input, or any run by the `cfgd daemon` (the daemon never has a TTY) — the script is **skipped with a warning** rather than hanging on instant EOF, and reports `changed=false`. This is the intended daemon-safe behavior: interactive steps run only during an attended `cfgd apply`, never under unattended reconcile.

```yaml
scripts:
  postApply:
    - run: |
        echo "Install Azure VPN from Self Service, then press Enter"
        read
      interactive: true
```

**Example:**
```yaml
scripts:
  postApply:
    - nvim --headless "+Lazy! sync" +qa
    - run: echo "BASH_VERSION=$BASH_VERSION"
      shell: bash
    - run: scripts/rebuild-index.sh
      timeout: 60s
      continueOnError: true
    # Only clone if the checkout doesn't exist yet.
    - run: git clone https://example.com/repo ~/.local/share/repo
      creates: ~/.local/share/repo
    # Only run the installer when the tool is missing.
    - run: ./install.sh
      unless: command -v mytool
    # Only rebuild when a marker says we must.
    - run: make rebuild
      onlyIf: test -f .needs-rebuild
```

Default timeout: 2 minutes. Scripts run in the module directory.

---

## Complete Example

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  description: Neovim editor with plugins, LSP, and config files

spec:
  depends:
    - node
    - python

  packages:
    - name: neovim
      minVersion: "0.9"
      prefer: [brew, snap, apt]
      aliases:
        snap: nvim

    - name: ripgrep

    - name: fd
      aliases:
        apt: fd-find
        dnf: fd-find

    - name: pynvim
      prefer: [pipx]

    - name: neovim
      prefer: [npm]

  files:
    - source: config/
      target: ~/.config/nvim/

    - source: https://github.com/user/nvim-config.git@v2.1.0
      target: ~/.config/nvim/

  env:
    - name: EDITOR
      value: nvim
    - name: NVIM_APPNAME
      value: nvim

  aliases:
    - name: vim
      command: nvim
    - name: vi
      command: nvim

  scripts:
    postApply:
      - nvim --headless "+Lazy! sync" +qa
      - nvim --headless -c "MasonInstallAll" -c "qa"
```

---

## Module Resolution and Merge Semantics

When a profile activates a module, cfgd merges the module's declarations on top of the profile's
merged spec using the following rules:

| Field | Merge rule |
|-------|-----------|
| `packages` | Platform-filtered, manager-preferenced resolution per entry. Module packages extend the profile's package list. |
| `files.managed` | Overlay by `target` — the module's entry for a given target replaces any profile entry for the same target. |
| `env` | Override by name — module variable wins over profile variable of the same name. |
| `aliases` | Override by name — same rule as `env`. |
| `system` | Deep merge — module keys overwrite profile keys at the leaf level. |
| `scripts` | Each hook list is appended after the profile's corresponding hook list. |

---

## Remote Modules

Modules can be pulled from a remote registry (a git repository with modules under
`modules/<name>/module.yaml`). Register a registry in `cfgd.yaml`:

```yaml
spec:
  modules:
    registries:
      - name: acme
        url: git@github.com:acme-corp/cfgd-modules.git
```

Then reference modules from that registry in a profile:

```yaml
spec:
  modules:
    - acme/nvim
    - acme/kubectl
```

Remote module versions are pinned in `modules.lock` at the config root. Run `cfgd module update`
to fetch new versions. Unpinned modules always resolve to the registry's default branch HEAD.
