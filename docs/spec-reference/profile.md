# Profile Spec Reference

A Profile document declares everything cfgd should manage on a machine: packages, files,
environment variables, shell aliases, system configurators, secrets, and lifecycle scripts.
Profiles are stored under `profiles/` in your config directory and referenced by name.

## Document Structure

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: string

spec:
  inherits:
    - string

  modules:
    - string

  env:
    - name: string
      value: string

  aliases:
    - name: string
      command: string

  packages:
    brew:
      file: string
      taps:
        - string
      formulae:
        - string
      casks:
        - string

    apt:
      file: string
      packages:
        - string

    cargo:
      file: string
      packages:
        - string
    # or list shorthand:
    # cargo:
    #   - bat
    #   - ripgrep

    npm:
      file: string
      global:
        - string

    pipx:
      - string

    dnf:
      - string

    apk:
      - string

    pacman:
      - string

    zypper:
      - string

    yum:
      - string

    pkg:
      - string

    nix:
      - string

    go:
      - string

    snap:
      packages:
        - string
      classic:
        - string

    flatpak:
      packages:
        - string
      remote: string

    custom:
      - name: string
        check: string
        listInstalled: string
        install: string
        uninstall: string
        update: string
        packages:
          - string

  files:
    managed:
      - source: string
        target: string
        strategy: Symlink | Copy | Template | Hardlink
        private: bool
    permissions:
      "path": "octal-mode"

  system:
    shell: string
    # other configurator keys and values

  secrets:
    - source: string
      target: string
      template: string
      backend: string

  scripts:
    preApply:
      - string | { run: string, timeout: string, continueOnError: bool }
    postApply:
      - string | { run: string, timeout: string, continueOnError: bool }
    preReconcile:
      - string | { run: string, timeout: string, continueOnError: bool }
    postReconcile:
      - string | { run: string, timeout: string, continueOnError: bool }
    onDrift:
      - string | { run: string, timeout: string, continueOnError: bool }
    onChange:
      - string | { run: string, timeout: string, continueOnError: bool }
```

---

## Fields

### metadata

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Name of this profile. Must match the filename (without extension). |

---

### spec

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `inherits` | list of string | No | `[]` | Parent profiles to inherit from. Resolved depth-first, left-to-right. |
| `modules` | list of string | No | `[]` | Module names to activate. Modules are resolved and applied before profile-level items. |
| `env` | list | No | `[]` | Environment variables to export. See [spec.env[]](#specenv). |
| `aliases` | list | No | `[]` | Shell aliases to install. See [spec.aliases[]](#specaliases). |
| `packages` | object | No | | Package declarations by manager. See [spec.packages](#specpackages). |
| `files` | object | No | | Managed files and permissions. See [spec.files](#specfiles). |
| `system` | map | No | `{}` | System configurator settings. Keys map to configurator names; values are configurator-specific. See [spec.system](#specsystem). |
| `secrets` | list | No | `[]` | Secret references to decrypt and place on disk. See [spec.secrets[]](#specsecrets). |
| `scripts` | object | No | | Lifecycle scripts (pre/post apply, pre/post reconcile, onChange, onDrift). See [spec.scripts](#specscripts). |

---

### spec.inherits

A list of profile names to inherit from. Inheritance is resolved depth-first, left-to-right: the
earliest ancestor is merged first, the current profile last. Later layers win on conflicts (env,
aliases), union on sets (packages, modules), and deep-merge on `system`.

Circular inheritance is detected at load time and reported as an error.

**Example:**
```yaml
spec:
  inherits:
    - base
    - security-hardening
```

---

### spec.env[]

Environment variables to export into the shell environment during reconciliation. Managed in the
shell rc file (`.zshrc`, `.bashrc`, etc.) by the shell system configurator.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Environment variable name (e.g. `EDITOR`). |
| `value` | string | Yes | | Value to assign. |

When profiles are merged via `inherits`, a variable defined in a child profile overrides the same
variable from a parent.

**Example:**
```yaml
env:
  - name: EDITOR
    value: nvim
  - name: GOPATH
    value: ~/go
```

---

### spec.aliases[]

Shell aliases to install.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Alias name (the command you type). |
| `command` | string | Yes | | Shell command the alias expands to. |

**Example:**
```yaml
aliases:
  - name: ll
    command: ls -la
  - name: gs
    command: git status
```

---

### spec.packages

Package declarations grouped by package manager. All managers are optional; omit any that do not
apply to the target machine. During reconciliation, cfgd installs any listed package that is not
already present. When multiple profiles are merged, package lists are unioned (no duplicates).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `brew` | object | No | | Homebrew packages. See [spec.packages.brew](#specpackagesbrew). |
| `apt` | object | No | | APT packages (Debian/Ubuntu). See [spec.packages.apt](#specpackagesapt). |
| `cargo` | object or list | No | | Cargo (Rust) packages. See [spec.packages.cargo](#specpackagescargo). |
| `npm` | object | No | | npm global packages. See [spec.packages.npm](#specpackagesnpm). |
| `pipx` | list of string | No | `[]` | pipx packages (isolated Python tools). |
| `dnf` | list of string | No | `[]` | DNF packages (Fedora/RHEL). |
| `apk` | list of string | No | `[]` | apk packages (Alpine Linux). |
| `pacman` | list of string | No | `[]` | pacman packages (Arch Linux). |
| `zypper` | list of string | No | `[]` | zypper packages (openSUSE). |
| `yum` | list of string | No | `[]` | yum packages (older RHEL/CentOS). |
| `pkg` | list of string | No | `[]` | pkg packages (FreeBSD). |
| `nix` | list of string | No | `[]` | Nix packages (nix-env). |
| `go` | list of string | No | `[]` | Go packages installed via `go install`. |
| `snap` | object | No | | Snap packages (Ubuntu). See [spec.packages.snap](#specpackagessnap). |
| `flatpak` | object | No | | Flatpak packages. See [spec.packages.flatpak](#specpackagesflatpak). |
| `custom` | list | No | `[]` | Custom package managers. See [spec.packages.custom[]](#specpackagescustom). |

---

### spec.packages.brew

Homebrew packages for macOS (and Linux Homebrew).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `file` | string | No | | Path to a `Brewfile` to install from. When set, cfgd runs `brew bundle`. |
| `taps` | list of string | No | `[]` | Homebrew taps to add before installing formulae/casks. |
| `formulae` | list of string | No | `[]` | Homebrew formulae to install. |
| `casks` | list of string | No | `[]` | Homebrew casks to install (macOS GUI apps). |

**Example:**
```yaml
packages:
  brew:
    taps:
      - homebrew/cask-fonts
    formulae:
      - git
      - ripgrep
      - kubectl
    casks:
      - visual-studio-code
      - wezterm
```

---

### spec.packages.apt

APT packages for Debian and Ubuntu.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `file` | string | No | | Path to a file listing packages (one per line). |
| `packages` | list of string | No | `[]` | APT package names to install. |

---

### spec.packages.cargo

Cargo (Rust crates installed as binaries) packages. Accepts both a list shorthand and an object
form.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `file` | string | No | | Path to a `Cargo.toml` (installs all `[dependencies]`). |
| `packages` | list of string | No | `[]` | Crate names to install via `cargo install`. |

**List shorthand** — when no `file` is needed:
```yaml
packages:
  cargo:
    - bat
    - eza
    - ripgrep
```

**Object form** — when mixing a file with additional packages:
```yaml
packages:
  cargo:
    file: Cargo.toml
    packages:
      - cargo-edit
```

---

### spec.packages.npm

npm global packages.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `file` | string | No | | Path to a `package.json` to install from. |
| `global` | list of string | No | `[]` | npm package names to install globally (`npm install -g`). |

---

### spec.packages.snap

Snap packages (Ubuntu and derivatives).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `packages` | list of string | No | `[]` | Snap packages to install in strict confinement. |
| `classic` | list of string | No | `[]` | Snap packages to install with `--classic` confinement (e.g. `code`, `go`). |

---

### spec.packages.flatpak

Flatpak packages.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `packages` | list of string | No | `[]` | Flatpak application IDs to install. |
| `remote` | string | No | | Flatpak remote to use (e.g. `flathub`). Defaults to system remote when omitted. |

---

### spec.packages.custom[]

A custom package manager defined entirely by shell commands. Useful for tools without a standard
package manager backend.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Identifier for this custom manager (used in plan output). |
| `check` | string | Yes | | Shell command to verify the manager itself is installed. Exit code 0 = present. |
| `listInstalled` | string | Yes | | Shell command that prints one installed package name per line. |
| `install` | string | Yes | | Shell command to install a package. The package name is appended. |
| `uninstall` | string | Yes | | Shell command to uninstall a package. The package name is appended. |
| `update` | string | No | | Shell command to update a package. When omitted, updates are skipped. |
| `packages` | list of string | No | `[]` | Package names managed by this custom manager. |

**Example:**
```yaml
packages:
  custom:
    - name: mise
      check: command -v mise
      listInstalled: mise list --current --quiet
      install: mise use -g
      uninstall: mise uninstall
      update: mise upgrade
      packages:
        - node@lts
        - python@3.12
```

---

### spec.files

Managed file deployment and permission settings.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `managed` | list | No | `[]` | Files to deploy from the config directory to target paths. See [spec.files.managed[]](#specfilesmanaged). |
| `permissions` | map | No | `{}` | Filesystem permissions to enforce. Keys are paths, values are octal mode strings. |

---

### spec.files.managed[]

Each entry declares one file (or directory) to deploy from the config repository to a target path
on the machine.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `source` | string | Yes | | Path to the source file or directory, relative to the config root. |
| `target` | string | Yes | | Absolute destination path on the machine. Supports `~/` expansion. |
| `strategy` | enum | No | Global `fileStrategy` | Deployment strategy for this file. Overrides the global default. See [FileStrategy values](#filestrategy-values). |
| `private` | bool | No | `false` | When `true`, the source file is local-only: automatically added to `.gitignore` and silently skipped on machines where it does not exist. |

**Example:**
```yaml
files:
  managed:
    - source: shell/.zshrc
      target: ~/.zshrc

    - source: git/.gitconfig.tera
      target: ~/.gitconfig

    - source: ssh/config.local
      target: ~/.ssh/config
      strategy: Copy
      private: true
```

#### FileStrategy values

| Value | Description |
|-------|-------------|
| `Symlink` | Create a symbolic link from `target` to the source file. **(default)** |
| `Copy` | Copy source content to `target`. The target is an independent file; changes to source are not reflected until the next reconcile. |
| `Template` | Render the source as a Tera template and write the output to `target`. Automatically selected for `.tera` source files. |
| `Hardlink` | Create a hard link from `target` to source. Changes to either file are immediately visible in both. |

---

### spec.files.permissions

A map of filesystem paths to octal permission mode strings. cfgd enforces these permissions during
each reconcile, correcting any drift.

```yaml
files:
  permissions:
    "~/.ssh":        "700"
    "~/.ssh/config": "600"
    "~/.gnupg":      "700"
```

Paths support `~/` expansion. Modes are standard octal strings (`600`, `700`, `755`, etc.).

---

### spec.system

A freeform map from system configurator name to configurator-specific settings. Keys must match
registered configurator identifiers. Values are passed directly to the configurator.

Common configurators:

| Key | Platform | Description |
|-----|----------|-------------|
| `shell` | All | Default login shell path (e.g. `/bin/zsh`). |
| `systemd` | Linux | systemd unit management. |
| `launchd` | macOS | launchd plist management. |
| `environment` | All | System-level environment file management. |
| `macosDefaults` | macOS | macOS `defaults write` settings. |
| `sysctl` | Linux | sysctl kernel parameter tuning. |
| `kernelModules` | Linux | Kernel module loading. |
| `containerd` | Linux | containerd runtime configuration. |
| `kubelet` | Linux | kubelet configuration for Kubernetes nodes. |
| `apparmor` | Linux | AppArmor profile management. |
| `seccomp` | Linux | seccomp filter deployment. |
| `certificates` | All | CA certificate installation. |

**Example:**
```yaml
system:
  shell: /bin/zsh
  macosDefaults:
    NSGlobalDomain:
      AppleInterfaceStyle: Dark
      KeyRepeat: 2
```

See `docs/system-configurators.md` for full documentation of each configurator.

---

### spec.secrets[]

Secrets to decrypt and place on disk during reconciliation. Secrets are never committed to the
config repository in plaintext.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `source` | string | Yes | | Secret reference URI. Format depends on backend: SOPS file path, `1password://vault/item/field`, `bitwarden://item/field`, or `vault://path/key`. |
| `target` | string | Yes | | Absolute path to write the decrypted secret. Supports `~/` expansion. |
| `template` | string | No | | Inline template string. When set, the secret value is injected into this template before writing to `target`. |
| `backend` | string | No | | Override the secret backend for this entry. Defaults to `spec.secrets.backend` in `cfgd.yaml`. |

**Example:**
```yaml
secrets:
  - source: 1password://Work/GitHub/token
    target: ~/.config/gh/token

  - source: secrets/aws-credentials.yaml
    target: ~/.aws/credentials
    backend: sops
```

---

### spec.scripts

Lifecycle scripts run at different points during apply and reconciliation. Scripts are executed in the order listed. Each entry can be a simple string (command or file path) or an object with `run`, `timeout`, and `continueOnError` fields.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `preApply` | list | No | `[]` | Scripts to run before user-initiated apply. Failure aborts the apply. |
| `postApply` | list | No | `[]` | Scripts to run after user-initiated apply completes. |
| `preReconcile` | list | No | `[]` | Scripts to run before daemon-initiated reconciliation. Failure aborts the reconcile. |
| `postReconcile` | list | No | `[]` | Scripts to run after daemon-initiated reconciliation completes. |
| `onDrift` | list | No | `[]` | Scripts to run when drift is detected, before any remediation. Profile-level only. |
| `onChange` | list | No | `[]` | Scripts to run after apply/reconcile only if resources actually changed. |

Each entry can be a string or an object:

```yaml
scripts:
  preApply:
    - scripts/check-vpn.sh                     # simple form
    - run: scripts/notify-slack.sh              # full form
      continueOnError: true
      timeout: 30s
  postApply:
    - scripts/reload-shell.sh
  onChange:
    - run: systemctl restart myservice
      timeout: 60s
```

Default timeouts: 5 minutes for profile scripts, 2 minutes for module scripts. Default `continueOnError`: `false` for pre-hooks, `true` for post-hooks and event hooks.

Paths are relative to the config root directory. If the path resolves to an existing file, it is executed directly (the OS uses the shebang to select the interpreter). If not, it is passed through `sh -c`.

---

## Profile Inheritance and Merge Semantics

When a profile lists `inherits`, cfgd resolves the full ancestor chain depth-first, then merges
all layers in resolution order (earliest ancestor first, current profile last).

| Field | Merge rule |
|-------|-----------|
| `modules` | Union — a module listed in any layer is activated. |
| `env` | Override by name — a child variable replaces the parent's variable of the same name. |
| `aliases` | Override by name — same rule as `env`. |
| `packages` | Union per manager — package lists across layers are combined, duplicates removed. |
| `files.managed` | Overlay by `target` — a child entry for the same target replaces the parent's. |
| `files.permissions` | Merge — child entries are added; conflicts resolved in favour of child. |
| `system` | Deep merge — child keys overwrite parent keys at the leaf level. |
| `secrets` | Append, deduplicated by `target`. |
| `scripts` | Append in order — parent scripts run before child scripts. |
