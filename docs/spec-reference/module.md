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
      - string | { run: string, timeout: string, continueOnError: bool }
    postApply:
      - string | { run: string, timeout: string, continueOnError: bool }
    preReconcile:
      - string | { run: string, timeout: string, continueOnError: bool }
    postReconcile:
      - string | { run: string, timeout: string, continueOnError: bool }
    onChange:
      - string | { run: string, timeout: string, continueOnError: bool }
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
| `packages` | list | No | `[]` | Cross-platform package declarations. See [spec.packages[]](#specpackages). |
| `files` | list | No | `[]` | Files to deploy from the module directory to the machine. See [spec.files[]](#specfiles). |
| `env` | list | No | `[]` | Environment variables to export. See [spec.env[]](#specenv). |
| `aliases` | list | No | `[]` | Shell aliases to install. See [spec.aliases[]](#specaliases). |
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

### spec.scripts

Lifecycle scripts executed at different points during module apply and reconciliation. `onDrift` is not available at the module level (profile-level only).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `preApply` | list | No | `[]` | Run before the module's packages and files are applied. Failure aborts. |
| `postApply` | list | No | `[]` | Run after the module is fully applied. |
| `preReconcile` | list | No | `[]` | Run before daemon-initiated reconciliation of this module. |
| `postReconcile` | list | No | `[]` | Run after daemon-initiated reconciliation of this module. |
| `onChange` | list | No | `[]` | Run after apply/reconcile only if this module's resources changed. |

Each entry can be a simple string or a full object with `run`, `timeout`, and `continueOnError`.

**Example:**
```yaml
scripts:
  postApply:
    - nvim --headless "+Lazy! sync" +qa
    - run: scripts/rebuild-index.sh
      timeout: 60s
      continueOnError: true
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
