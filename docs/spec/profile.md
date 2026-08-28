# Profile Spec Reference

A Profile document declares everything cfgd should manage on a machine: packages, files,
environment variables, shell aliases, system configurators, secrets, and lifecycle scripts.
Profiles are stored under `profiles/` in your config directory and referenced by name.
The canonical layout is a bundle: `profiles/<name>/profile.yaml` alongside an optional
`files/` payload directory (mirroring `modules/<name>/module.yaml`). The legacy flat form
`profiles/<name>.yaml` remains fully supported; `cfgd profile migrate` moves a flat profile
into the canonical bundle.

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

    winget:
      - string

    chocolatey:
      - string

    scoop:
      - string

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
        strategy: Symlink | Copy | Template | Hardlink | Patch
        private: bool
        patch:
          format: Ini | Json | Yaml | Toml
          ensure: {}
          script: string
    permissions:
      "path": "octal-mode"

  system:
    shell: string
    windowsRegistry:
      "HIVE\\Key\\Subkey":
        ValueName: string | integer
    windowsServices:
      - name: string
        displayName: string
        binaryPath: string
        startType: auto | manual | disabled
        state: running | stopped
    # other configurator keys and values

  secrets:
    - source: string
      target: string
      template: string
      backend: string

  scripts:
    preApply:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
    postApply:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
    preReconcile:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
    postReconcile:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
    onDrift:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
    onChange:
      - string | { run: string, shell: string, timeout: string, idleTimeout: string, continueOnError: bool, onlyIf: string, unless: string, creates: string, interactive: bool, workdir: string }
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
| `env` | list | No | `[]` | Environment variables to export, each optionally gated to named platforms. See [spec.env[]](#specenv). |
| `envScope` | string | No | `All` | How far `spec.env` exports reach for the current user. See [spec.envScope](#specenvscope). |
| `aliases` | list | No | `[]` | Shell aliases to install, each optionally gated to named platforms. See [spec.aliases[]](#specaliases). |
| `packages` | object | No | | Package declarations by manager. See [spec.packages](#specpackages). |
| `files` | object | No | | Managed files and permissions. See [spec.files](#specfiles). |
| `system` | map | No | `{}` | System configurator settings. Keys map to configurator names; values are configurator-specific. See [spec.system](#specsystem). |
| `secrets` | list | No | `[]` | Secret references to decrypt and place on disk. See [spec.secrets[]](#specsecrets). |
| `scripts` | object | No | | Lifecycle scripts (pre/post apply, pre/post reconcile, onChange, onDrift). See [spec.scripts](#specscripts). |
| `backups` | list | No | `[]` | Declarative file/directory snapshot backups. See [spec.backups[]](#specbackups). |

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

Environment variables to export for the **current user**. cfgd writes a managed env file
(`~/.cfgd.env`) and wires it into the user's shells and session managers according to
[`spec.envScope`](#specenvscope): by default every standard user context. For **system-wide**
(all-users, privileged) variables, use [`spec.system.environment`](../system-configurators.md)
instead; the two differ by *scope of affected users*, not by which shells.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Environment variable name (e.g. `EDITOR`). |
| `value` | string | Yes | | Value to assign. |
| `platforms` | list of string | No | `[]` | Platform tags gating this entry alone. Same vocabulary as a module's [`spec.platforms`](module.md#specplatforms). |

When profiles are merged via `inherits`, a variable defined in a child profile overrides the same
variable from a parent.

`platforms` gates one entry rather than the whole profile: when it is non-empty and the current
platform matches none of the tags, the entry is not part of this machine's desired state at all
(it appears on no surface, exactly as a platform-filtered package does). Omit it to export the
variable everywhere.

`PATH` is the one name whose surviving declarations **concatenate** rather than replace: a common
entry and a gated one both apply on a machine that matches both, and the folded value keeps them
in declaration order with `$PATH` written once. Every other name is last-writer-wins.

**Example:**
```yaml
env:
  - name: EDITOR
    value: nvim
  - name: GOPATH
    value: ~/go
  - name: PATH
    value: $HOME/.local/bin:$PATH
  # only on macOS: these Homebrew prefixes do not exist elsewhere
  - name: PATH
    value: /opt/homebrew/opt/ruby/bin:$PATH
    platforms: [macos]
  - name: BROWSER
    value: xdg-open
    platforms: [linux, freebsd]
```

On macOS the two `PATH` declarations fold into one line
(`$HOME/.local/bin:/opt/homebrew/opt/ruby/bin:$PATH`) and `BROWSER` is absent; on Linux the second
`PATH` declaration is absent and `BROWSER` is exported.

---

### spec.envScope

Controls how far [`spec.env`](#specenv) exports reach across the current user's environment. Omit
to inherit a parent layer's value (resolves to `All` when no layer sets it). Aliases are always
interactive-only regardless of scope; fish `conf.d` always covers every fish session.

| Value | Reaches |
|-------|---------|
| `All` *(default)* | Everything in `Login`, **plus** session managers — `~/.config/environment.d/cfgd.conf` (systemd `--user` + Wayland GUI, Linux), `~/Library/LaunchAgents/com.cfgd.user-environment.plist` (macOS GUI), and an immediate **live-session refresh** (`launchctl setenv` / `systemctl --user set-environment` / `setx`). |
| `Login` | Everything in `Interactive`, **plus** login shells — `~/.zshenv` (zsh, all contexts), `~/.profile` (sh/bash login), and `~/.bash_profile`/`~/.bash_login` *only if one already exists*. |
| `Interactive` | Interactive shells only — `~/.cfgd.env` sourced from `~/.bashrc`/`~/.zshrc` (and fish `conf.d`). The historical behavior. |

cfgd never overwrites a user-owned dotfile: it owns the standalone `~/.cfgd.env` (and the
`environment.d`/plist files) outright, and only appends an idempotent `source` line into shell rc
files. It will **not create** a `~/.bash_profile` that didn't exist, because bash reads the first
existing of `~/.bash_profile`, `~/.bash_login`, `~/.profile` and stops; creating one would shadow
your `~/.profile`.

> `~/.config/environment.d` is read by `systemd --user` and Wayland sessions started through it;
> classic X11 display managers that don't import the systemd user environment won't see it. File
> targets take effect in new sessions; the live-session refresh applies immediately.

`~/.cfgd.env` is written even when `spec.env` and `spec.aliases` are both empty, as long as cfgd
**itself bootstrapped** a package manager the profile still names. When a bootstrap succeeds, cfgd
records the `PATH` directories that install contributed and exports them from `~/.cfgd.env`. A
manager you installed yourself contributes nothing here: cfgd never claims ownership of a machine
change it did not make, so your rc files are left alone. Drop the manager from the profile and its
directories age out of the file.

Those directories are exported **first**, ahead of your own variables, so a `spec.env` value may
reference a binary that manager's bootstrap installed. cfgd knows most managers' install locations before
the bootstrap even runs, so the plan folds a to-be-provisioned manager's declared directories into
the `Env` phase's write up front: the first apply on a bare machine is already correct. The one
exception is a manager whose install location is only knowable once its bootstrap finishes (npm's
global prefix depends on which Node install method wins); that manager still converges inside the
same run: cfgd re-derives the file once every phase completes and the real directory is recorded.
cfgd prints a reminder after any apply that wrote the file or injected a source line: your
already-running shell does not pick either up until you `source ~/.cfgd.env` or open a new one.

**Example:**
```yaml
spec:
  env:
    - name: EDITOR
      value: nvim
  envScope: All        # default; narrow to Login or Interactive to opt out of broader reach
```

---

### spec.aliases[]

Shell aliases to install.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Alias name (the command you type). |
| `command` | string | Yes | | Shell command the alias expands to. |
| `platforms` | list of string | No | `[]` | Platform tags gating this entry alone. Same vocabulary as a module's [`spec.platforms`](module.md#specplatforms). |

`platforms` gates one alias rather than the whole profile, exactly as [`spec.env[]`](#specenv)'s
does: an alias gated off the current host is not installed and appears on no surface.

**Example:**
```yaml
aliases:
  - name: ll
    command: ls -la
  - name: gs
    command: git status
  # Linux has no pbcopy; this stands in for it
  - name: pbcopy
    command: xclip -selection clipboard
    platforms: [linux]
```

---

### spec.packages

Package declarations grouped by package manager. All managers are optional; omit any that do not
apply to the target machine. During reconciliation, cfgd installs any listed package that is not
already present. When multiple profiles are merged, package lists are unioned (no duplicates).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `brew` | object or list | No | | Homebrew packages. See [spec.packages.brew](#specpackagesbrew). |
| `apt` | object or list | No | | APT packages (Debian/Ubuntu). See [spec.packages.apt](#specpackagesapt). |
| `cargo` | object or list | No | | Cargo (Rust) packages. See [spec.packages.cargo](#specpackagescargo). |
| `npm` | object or list | No | | npm global packages. See [spec.packages.npm](#specpackagesnpm). |
| `pipx` | list of string or object | No | `[]` | pipx packages (isolated Python tools). |
| `dnf` | list of string or object | No | `[]` | DNF packages (Fedora/RHEL). |
| `apk` | list of string or object | No | `[]` | apk packages (Alpine Linux). |
| `pacman` | list of string or object | No | `[]` | pacman packages (Arch Linux). |
| `zypper` | list of string or object | No | `[]` | zypper packages (openSUSE). |
| `yum` | list of string or object | No | `[]` | yum packages (older RHEL/CentOS). |
| `pkg` | list of string or object | No | `[]` | pkg packages (FreeBSD). |
| `nix` | list of string or object | No | `[]` | Nix packages (nix-env). |
| `go` | list of string or object | No | `[]` | Go packages installed via `go install`. |
| `snap` | object or list | No | | Snap packages (Ubuntu). See [spec.packages.snap](#specpackagessnap). |
| `flatpak` | object or list | No | | Flatpak packages. See [spec.packages.flatpak](#specpackagesflatpak). |
| `winget` | list of string or object | No | `[]` | winget packages (Windows). |
| `chocolatey` | list of string or object | No | `[]` | Chocolatey packages (Windows). |
| `scoop` | list of string or object | No | `[]` | Scoop packages (Windows). |
| `custom` | list | No | `[]` | Custom package managers. See [spec.packages.custom[]](#specpackagescustom). |

Every manager but `custom` accepts two shapes. A bare list is the short form; the object form
carries the manager's other fields. For the twelve `list of string or object` managers the object
form has one key, `packages`, and the two spellings are interchangeable:

```yaml
packages:
  brew: [ripgrep, fzf]          # short form: folds into `formulae`
  apt:
    file: packages.txt          # object form: the manager's own fields
    packages: [curl, git]
  pipx: [black, ruff]           # short form
  dnf:
    packages: [htop]            # object form: `packages` is the only key
```

The published JSON schemas (`schemas/cfgd-profile.schema.json`, `schemas/cfgd-source.schema.json`)
and `cfgd explain profile.spec.packages.<manager>` state both shapes; the `Variants` section of
`explain` lists them.

---

### spec.packages.brew

Homebrew packages for macOS (and Linux Homebrew). A bare list of names is the short form and folds into `formulae`.

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

APT packages for Debian and Ubuntu. A bare list of names is the short form and folds into `packages`.

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

**List shorthand** (when no `file` is needed):
```yaml
packages:
  cargo:
    - bat
    - eza
    - ripgrep
```

**Object form** (when mixing a file with additional packages):
```yaml
packages:
  cargo:
    file: Cargo.toml
    packages:
      - cargo-edit
```

---

### spec.packages.npm

npm global packages. A bare list of names is the short form and folds into `global`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `file` | string | No | | Path to a `package.json` to install from. |
| `global` | list of string | No | `[]` | npm package names to install globally (`npm install -g`). |

---

### spec.packages.snap

Snap packages (Ubuntu and derivatives). A bare list of names is the short form and folds into `packages`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `packages` | list of string | No | `[]` | Snap packages to install in strict confinement. |
| `classic` | list of string | No | `[]` | Snap packages to install with `--classic` confinement (e.g. `code`, `go`). |

---

### spec.packages.flatpak

Flatpak packages. A bare list of names is the short form and folds into `packages`.

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
| `source` | string | Only when `strategy` is not `Patch` | | Path to the source file or directory, relative to the config root. Not required when `strategy: Patch`. |
| `target` | string | Yes | | Absolute destination path on the machine. Supports `~/` expansion. |
| `strategy` | enum | No | Global `fileStrategy` | Deployment strategy for this file. Overrides the global default. See [FileStrategy values](#filestrategy-values). |
| `private` | bool | No | `false` | When `true`, the source file is local-only: automatically added to `.gitignore` and silently skipped on machines where it does not exist. |
| `permissions` | string | No | | Octal permission mode to enforce on the deployed target file (e.g. `"600"`). Distinct from `files.permissions`, which enforces permissions on paths not managed as file entries. |
| `encryption` | object | No | | Encryption enforcement for this file. Has `backend` (`"sops"` or `"age"`) and `mode` (`InRepo` or `Always`). Rejected with `strategy: Patch`, which has no source to enforce it on. See [encryption fields](#managed-file-encryption-fields). |
| `patch` | object | Only when `strategy: Patch` | | Structured merge or script configuration, used only when `strategy: Patch`. Has `format` (`Ini`/`Json`/`Yaml`/`Toml`, inferred from `target`'s extension when omitted), `ensure` (keys/values to deep-merge into the target), and `script` (a script that receives the target's current content on stdin and writes the new content to stdout). Exactly one of `ensure` or `script` must be set. See [FileStrategy values](#filestrategy-values). |

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

    - target: ~/.gitconfig
      strategy: Patch
      patch:
        format: Ini
        ensure:
          user:
            name: "Example User"
```

#### FileStrategy values

| Value | Description |
|-------|-------------|
| `Symlink` | Create a symbolic link from `target` to the source file. **(default)** |
| `Copy` | Copy source content to `target`. The target is an independent file; changes to source are not reflected until the next reconcile. |
| `Template` | Render the source as a Tera template and write the output to `target`. Automatically selected for `.tera` source files. |
| `Hardlink` | Create a hard link from `target` to source. Changes to either file are immediately visible in both. |
| `Patch` | Merge structured keys/values into the target, or pipe it through a script, leaving everything else untouched. Requires a `patch` block; `source` is not required. |

#### Managed file encryption fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `encryption.backend` | string | Yes (when `encryption` present) | | Encryption backend: `"sops"` or `"age"`. Same values as `spec.secrets.backend` in `cfgd.yaml`. |
| `encryption.mode` | enum | No | `InRepo` | `InRepo`: source must be encrypted in the repo, deployed decrypted. `Always`: encrypted in repo and encrypted at the target path. `Always` is incompatible with `strategy: Symlink` and `strategy: Hardlink`; the whole `encryption` block is incompatible with `strategy: Patch`. |

**Example:**
```yaml
files:
  managed:
    - source: ssh/config
      target: ~/.ssh/config
      permissions: "600"
      encryption:
        backend: sops
        mode: InRepo

    - source: shell/.zshrc
      target: ~/.zshrc
      # no encryption block = no enforcement
```

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
| `gsettings` | Linux | GNOME/GTK desktop settings via gsettings. |
| `kdeConfig` | Linux | KDE Plasma settings via kwriteconfig. |
| `xfconf` | Linux | XFCE desktop settings via xfconf-query. |
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
| `windowsRegistry` | Windows | Registry key/value management. |
| `windowsServices` | Windows | Windows Service lifecycle management. |
| `sshKeys` | All | SSH key pair provisioning and permission enforcement. |
| `gpgKeys` | All | GPG key provisioning and validity tracking. |
| `git` | All | Global git configuration (`git config --global`). |

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
| `source` | string | Yes | | Secret reference URI. Format depends on backend: SOPS file path, `1password://vault/item/field` (`op://`), `bitwarden://item/field` (`bw://`), `lastpass://folder/item/field` (`lpass://`, `lp://`), or `vault://path/key`. |
| `target` | string | No | | Absolute path to write the decrypted secret. Supports `~/` expansion. At least one of `target` or `envs` must be set; both may be set. |
| `envs` | list | No | | Environment variable names to inject with the resolved secret value. At least one of `target` or `envs` must be set; both may be set. See [Environment variable injection from secrets](#environment-variable-injection-from-secrets). |
| `template` | string | No | | Wraps a provider-resolved value: `${secret:value}` is replaced with the secret, the rest is written verbatim, for `target` and `envs` alike. Must contain `${secret:value}`; refused on an encrypted-file `source`. |
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

#### Environment variable injection from secrets

When `envs` is set, cfgd resolves the secret and writes the value to the managed shell environment file alongside regular `env:` entries. `target` and `envs` can both be set on the same entry: the secret is placed as a file and injected as an env var.

```yaml
secrets:
  # Inject into the shell environment only
  - source: 1password://Work/GitHub/token
    envs:
      - GITHUB_TOKEN

  # Write to a file and inject as an env var
  - source: vault://secret/data/api#key
    target: ~/.config/api-key
    envs:
      - API_KEY

  # Multiple env vars from one provider: use explicit field references
  - source: vault://secret/data/aws#aws_access_key_id
    envs:
      - AWS_ACCESS_KEY_ID
  - source: vault://secret/data/aws#aws_secret_access_key
    envs:
      - AWS_SECRET_ACCESS_KEY
```

When `envs` has multiple entries and the source resolves to a single value, all named env vars receive that value. The daemon refreshes secret-backed env vars on every reconcile cycle. Compliance snapshots record that the env var exists and its source, never the value.

---

### spec.scripts

Lifecycle scripts run at different points during apply and reconciliation. Scripts are executed in the order listed. Each entry can be a simple string (command or file path) or an object with `run`, `shell`, `timeout`, `idleTimeout`, `continueOnError`, `interactive`, `workdir`, and the idempotency guards `onlyIf`, `unless`, and `creates`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `preApply` | list | No | `[]` | Scripts to run before user-initiated apply. Failure aborts the apply. |
| `postApply` | list | No | `[]` | Scripts to run after user-initiated apply completes. |
| `preReconcile` | list | No | `[]` | Scripts to run before daemon-initiated reconciliation. Failure aborts the reconcile. |
| `postReconcile` | list | No | `[]` | Scripts to run after daemon-initiated reconciliation completes. |
| `onDrift` | list | No | `[]` | Scripts to run when drift is detected, before any remediation. Profile-level only. |
| `onChange` | list | No | `[]` | Scripts to run after apply/reconcile only if resources actually changed. |

The `shell` field selects the interpreter for inline commands: `bash`, `zsh`, `sh`, `pwsh`, `cmd`, or `auto` (default). `auto` uses `sh` on Unix and `cmd.exe` on Windows. `shell` only applies to inline commands; file scripts use their shebang.

When `shell` is `bash` or `zsh`, the script automatically sources `~/.cfgd.env` before execution, making all resolved `spec.env` vars and `spec.aliases` available (with alias expansion enabled). See [Lifecycle Scripts](../lifecycle-scripts.md) for details.

The idempotency guards `onlyIf`, `unless`, and `creates` make a script re-run-safe by construction. They are evaluated **before** the script body, in this order; any guard that says "skip" skips the body and reports `changed=false` with a `Skipped` status line naming the guard:

| Field | Type | Skips the body when… |
|---|---|---|
| `creates` | string (path) | the path already exists |
| `onlyIf` | string (command) | the command exits **non-zero** (the condition to run is not met) |
| `unless` | string (command) | the command exits **zero** (the guarded state already holds) |

When more than one guard is set, **all** must permit running for the body to run. `onlyIf`/`unless` commands run with the same shell, working directory, and environment as the body, bounded by a timeout so a guard can never hang; a guard command that fails to spawn (e.g. a missing interpreter) is a hard error. For `creates`, a leading `~` expands to the home directory and a relative path resolves against the script's working directory (the home directory by default; see below); existence follows symlinks.

### Working directory

Scripts run in the user's **home directory** by default, never the config source tree, so a relative write can't pollute the config repo. Reach the config root via the injected `$CFGD_CONFIG_DIR` variable. Set `workdir` to override (a leading `~` expands to home; `$VAR` / `${VAR}` expand against the script environment):

```yaml
scripts:
  postApply:
    - run: ./bootstrap.sh
      workdir: $CFGD_CONFIG_DIR
```

See [Lifecycle Scripts](../lifecycle-scripts.md#working-directory) for the full contract and the injected-variable table.

### Interactive scripts

Set `interactive: true` on a script entry that must prompt the user (for example, pausing a `postApply` step until a manual install is done). The script runs **attached to the terminal** (inherited stdin/stdout/stderr, no spinner, no output capture) and is **not** subject to the idle timeout, since an interactive step is attended by definition.

An interactive script requires a TTY. When stdin is **not** a terminal (CI, piped input, or any run by the `cfgd daemon`, which never has a TTY), the script is **skipped with a warning** instead of hanging on instant EOF, and reports `changed=false`. Interactive steps therefore run only during an attended `cfgd apply`, never under unattended reconcile.

The child shares cfgd's own process group instead of getting a new detached one, so the terminal's foreground group still includes it: a Ctrl-C typed at the terminal reaches the script directly, and a raw-mode TUI or a `sudo` password prompt behaves normally. By default an interactive script has **no timeout at all**: force-killing a step that's mid-raw-mode or waiting on a password would be worse than an unbounded wait. Set `timeout:` on the entry when a step does need a ceiling; once it elapses cfgd terminates the script (SIGTERM, then SIGKILL after a grace period).

```yaml
scripts:
  postApply:
    - run: |
        echo "Install Azure VPN from Self Service, then press Enter"
        read
      interactive: true
```

See [Lifecycle Scripts](../lifecycle-scripts.md#interactive-scripts) for the
full contract, including the process-group-sharing and opt-in-timeout
rationale.

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
    - run: echo "applied at $(date)"
      shell: bash
    # Idempotent: only clone when the checkout is missing.
    - run: git clone https://example.com/repo ~/.local/share/repo
      creates: ~/.local/share/repo
    # Idempotent: only install when the tool is absent.
    - run: ./install.sh
      unless: command -v mytool
  onChange:
    - run: systemctl restart myservice
      timeout: 60s
```

Default timeouts: 5 minutes for profile scripts, 2 minutes for module scripts. `idleTimeout` kills scripts that produce no stdout/stderr output for the specified duration (e.g. `30s`, `2m`), preventing silent hangs. Default `continueOnError`: `false` for pre-hooks, `true` for post-hooks and event hooks.

Paths are relative to the config root directory. If the path resolves to an existing file, it is executed directly (the OS uses the shebang to select the interpreter). If not, it is passed through the selected shell (`sh -c` by default).

---

### spec.backups[]

Declarative snapshot backups of a file or directory. See [Declarative Backups](../backups.md) for
run semantics (hook ordering, atomicity, retention counting) and [restoring](../backups.md#restoring).
A schedule-less entry runs during `cfgd apply`; a scheduled one runs on the
[daemon's timers](../backups.md#daemon-scheduling). Either can be run on demand with
`cfgd backup run [name]`, and any snapshot put back with `cfgd backup restore <name>`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | Yes | | Unique identifier for this backup within `spec.backups`. Keys the `destination` default, run records, and CLI selection. It becomes the directory component `<state_dir>/backups/<name>/` and the lock file `<state_dir>/locks/backup-<name>.lock`, so it must be unique across the list, non-empty/non-blank, a single segment (no `/` or `\`), not a directory reference (`.`, `..`), not rooted (`/daily`, `C:/daily`), and free of `:` anywhere — a drive and NTFS data-stream separator on Windows. Windows shapes are rejected on every platform so a name written on one OS stays valid on the others. Validated at parse time. |
| `source` | string (path) | Yes | | File or directory to snapshot; a leading `~` expands to the home directory. Must not contain, or sit inside, the resolved `destination` — a nested pair is rejected before any copy, with symlinks resolved on both sides. Its filename is what `{filename}` interpolates, so a source whose filename contains `:` (legal on Unix, a drive/data-stream separator on Windows) needs an explicit `namePattern` that omits `{filename}`. |
| `destination` | string (path) | No | `<state_dir>/backups/<name>/` | Where snapshots are written; a leading `~` expands to the home directory. The default is resolved by the backup engine at run time, not at parse time. |
| `namePattern` | string | No | `"{filename}.{timestamp}"` | Filename template for each snapshot. Supports `{name}`, `{filename}`, and `{timestamp}` (UTC, `%Y%m%dT%H%M%SZ`). Unknown `{var}` tokens are rejected at parse time. A literal `/` nests the snapshot under the destination; the rendered value must be relative and every segment must name something (`.`, `..`, empty segments, rooted values like `/daily` or `C:/daily`, and `:` anywhere are rejected at run time — the rejection names the `{filename}` it interpolated so a colon in the source filename points at itself). |
| `schedule` | string | No | | When to run this backup: a duration interval (e.g. `6h`) or a cron expression, validated at parse time. Cron accepts 5-field (`minute hour day month weekday`, e.g. `0 3 * * *`) or 6-field with a leading seconds field (`second minute hour day month weekday`, e.g. `30 0 3 * * *`), evaluated in the machine's **local** timezone like a crontab entry. Setting it hands the backup to the daemon's timers and takes it out of apply; omitted means "run on every apply". |
| `retention` | integer | No | `10` | Number of newest snapshots to keep; older snapshots are pruned from disk and from the run history. Counted per outcome, so failed runs never evict good snapshots. Must be at least 1 — `0` is rejected at parse time as a misconfiguration, not an "unlimited" mode. |
| `preBackup` | list | No | `[]` | Scripts run before the snapshot is taken. Same shape as [spec.scripts](#specscripts) entries. A failure skips the copy and records a failed run; `postBackup` still runs. |
| `postBackup` | list | No | `[]` | Scripts run after the copy step, and after a failed `preBackup` — always attempted, so whatever `preBackup` stopped gets restarted. Same shape as [spec.scripts](#specscripts) entries. |

**Example:**
```yaml
backups:
  - name: notes-db
    source: ~/.local/share/notes/notes.db # file or directory
    destination: ~/backups/notes          # optional; default <state_dir>/backups/<name>/
    namePattern: "{filename}.{timestamp}" # optional; vars {name} {filename} {timestamp}
    schedule: "0 3 * * *"                 # optional; cron (local time) OR interval ("6h"); set → daemon timer, omitted → every apply
    retention: 7                          # optional; default 10; newest N kept per backup
    preBackup:                            # optional; existing ScriptEntry shape
      - run: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
    postBackup:
      - run: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
```

`spec.backups[]` exists only in the YAML/TOML profile config path; the `MachineConfig` CRD does not
carry it.

Every run is recorded in the state database's `backup_runs` table (source, destination, size,
status, error, start/finish timestamps), and retention pruning walks those records rather than
globbing the destination.

---

## Profile Inheritance and Merge Semantics

When a profile lists `inherits`, cfgd resolves the full ancestor chain depth-first, then merges
all layers in resolution order (earliest ancestor first, current profile last).

| Field | Merge rule |
|-------|-----------|
| `modules` | Union — a module listed in any layer is activated. |
| `env` | Override by name — a child variable replaces the parent's variable of the same name. |
| `envScope` | Last layer that *specifies* it wins; a layer that omits it inherits the value resolved so far (defaults to `All`). |
| `aliases` | Override by name — same rule as `env`. |
| `packages` | Union per manager — package lists across layers are combined, duplicates removed. |
| `files.managed` | Overlay by `target` — a child entry for the same target replaces the parent's. |
| `files.permissions` | Merge — child entries are added; conflicts resolved in favour of child. |
| `system` | Deep merge — child keys overwrite parent keys at the leaf level. |
| `secrets` | Append, deduplicated by `target`. |
| `scripts` | Append in order — parent scripts run before child scripts. |
| `backups` | Append, deduplicated by `name`; later layer overrides. |
