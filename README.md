# cfgd

Declarative, GitOps-style machine configuration state management. Written in Rust.

cfgd brings Kubernetes-style reconciliation to machine configuration. You declare your desired state — packages, files, secrets, system settings — in version-controlled YAML, and cfgd continuously ensures your machines match. Drift is detected, reported, and optionally corrected automatically.

Pronounced "see-eff-gee-dee", like `etcd` but for config. The `d` is for daemon.

## Quick Start

### Install

```sh
# One-liner (macOS / Linux)
curl -fsSL https://raw.githubusercontent.com/TODO/cfgd/main/install.sh | sh

# Or build from source
cargo install --path .
```

### Bootstrap a New Machine

```sh
# Clone your config repo and set everything up interactively
cfgd init --from git@github.com:you/machine-config.git

# Or initialize a new config in the current directory
cfgd init
```

The bootstrap flow walks you through profile selection, secrets setup, shows a full plan of what will change, asks for confirmation, applies everything, runs verification, and optionally installs the daemon. If interrupted, re-running `cfgd init` resumes where it left off.

### Day-to-Day Usage

```sh
cfgd plan              # preview what would change
cfgd apply             # apply the plan (with confirmation)
cfgd apply --yes       # skip confirmation
cfgd apply --phase packages   # apply only one phase
cfgd status            # show drift and managed resources
cfgd diff              # show file diffs with syntax highlighting
cfgd verify            # check all resources match desired state
cfgd log               # show apply history
cfgd doctor            # check system health and dependencies
```

### Managing Resources

```sh
# Add a file to be managed
cfgd add ~/.config/starship.toml

# Add/remove packages
cfgd add --package brew ripgrep
cfgd remove --package brew ripgrep

# Switch profiles
cfgd profile list
cfgd profile switch work
cfgd profile show      # show resolved profile (all layers merged)
```

### Secrets

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt secrets.yaml    # encrypt values in place (SOPS)
cfgd secret decrypt secrets.yaml    # decrypt to stdout
cfgd secret edit secrets.yaml       # decrypt, open $EDITOR, re-encrypt
```

### Daemon

```sh
cfgd daemon                # run in foreground
cfgd daemon --install      # install as launchd (macOS) or systemd (Linux) service
cfgd daemon --status       # check if running, last reconcile time
cfgd daemon --uninstall    # remove the service
```

### Sync

```sh
cfgd sync    # pull from remote, show changes
cfgd pull    # pull remote changes
```

---

## Repository Layout

A cfgd config repo has this structure:

```
my-config/
├── cfgd.yaml              # root config — which profile to use, origin, daemon settings
├── profiles/
│   ├── base.yaml          # base profile — shared across machines
│   ├── work.yaml          # work profile — inherits base, adds work-specific config
│   └── personal.yaml      # personal profile
├── files/                 # source files to be placed on the machine
│   ├── shell/
│   │   ├── .zshrc
│   │   └── .zshrc.tera    # Tera templates get rendered with profile variables
│   ├── git/
│   │   └── .gitconfig
│   └── ssh/
│       └── config
├── secrets/               # SOPS-encrypted files (keys visible, values encrypted)
│   └── api-keys.yaml
└── scripts/               # pre/post-apply scripts
    ├── pre-setup.sh
    └── post-setup.sh
```

---

## Configuration Reference

cfgd uses a KRM-inspired YAML format. All config files have `apiVersion`, `kind`, `metadata`, and `spec` fields. TOML is also supported (use `.toml` extension).

### Root Config — `cfgd.yaml`

This is the entry point. It tells cfgd which profile to activate, where the config is stored, and how the daemon should behave.

```yaml
api-version: cfgd/v1
kind: Config
metadata:
  name: my-workstation        # identifier for this config set
spec:
  profile: work               # active profile name

  origin:                      # where this config is stored (optional)
    type: git                  # git | server
    url: git@github.com:me/machine-config.git
    branch: main              # default: main

  daemon:                      # daemon behavior (optional, all fields have defaults)
    enabled: true
    reconcile:
      interval: 5m            # how often to check for drift (default: 5m)
      on-change: true          # reconcile immediately when files change
    sync:
      auto-pull: true          # pull from remote on interval
      auto-push: false         # push local changes to remote
      interval: 5m            # sync interval (default: 5m)
    notify:
      drift: true              # send notifications on drift
      method: desktop          # desktop | stdout | webhook
      webhook-url: https://...  # required if method is webhook

  secrets:                     # secrets configuration (optional)
    backend: sops              # sops (default) | age
    sops:
      age-key: ~/.config/cfgd/age-key.txt   # path to age key file
    integrations:              # external secret providers (optional)
      - name: 1password
      - name: bitwarden
      - name: vault
```

**Fields:**

| Field | Required | Default | Description |
|---|---|---|---|
| `spec.profile` | yes | — | Name of the profile YAML file to activate (without `.yaml`) |
| `spec.origin.type` | no | — | `git` or `server` |
| `spec.origin.url` | no | — | Repository URL |
| `spec.origin.branch` | no | `main` | Git branch |
| `spec.daemon.reconcile.interval` | no | `5m` | Drift check interval (e.g. `1m`, `5m`, `1h`) |
| `spec.daemon.reconcile.on-change` | no | `false` | Reconcile immediately on file change |
| `spec.daemon.sync.auto-pull` | no | `false` | Auto-pull from remote |
| `spec.daemon.sync.auto-push` | no | `false` | Auto-commit and push local changes |
| `spec.daemon.notify.method` | no | `desktop` | `desktop`, `stdout`, or `webhook` |
| `spec.secrets.backend` | no | `sops` | `sops` or `age` |

### Profile — `profiles/<name>.yaml`

Profiles declare the desired state of a machine. They can inherit from other profiles to share common configuration.

```yaml
api-version: cfgd/v1
kind: Profile
metadata:
  name: work
spec:
  inherits:                    # ordered list — later overrides earlier
    - base
    - macos

  variables:                   # arbitrary key-value pairs, available in templates
    EDITOR: "code --wait"
    GIT_AUTHOR_NAME: "Jane Doe"
    GIT_AUTHOR_EMAIL: jane@work.com
    dotfiles_theme: gruvbox

  packages:
    brew:
      taps:                    # Homebrew taps to add
        - homebrew/cask-fonts
      formulae:                # Homebrew formulae to install
        - git
        - ripgrep
        - fd
        - jq
        - kubectl
        - helm
      casks:                   # Homebrew casks to install
        - 1password
        - wezterm
        - visual-studio-code
    apt:
      install:                 # apt packages (Debian/Ubuntu)
        - build-essential
        - curl
    cargo:                     # cargo install (Rust)
      - bat
      - eza
      - cargo-watch
    npm:
      global:                  # npm install -g (Node)
        - typescript
        - prettier
    pipx:                      # pipx install (Python)
      - httpie
      - ruff
    dnf:                       # dnf install (Fedora/RHEL)
      - gcc
      - make

  files:
    managed:                   # files to copy/template from source to target
      - source: shell/.zshrc
        target: ~/.zshrc
      - source: git/.gitconfig.tera       # .tera files are rendered as templates
        target: ~/.gitconfig
      - source: ssh/config
        target: ~/.ssh/config
      - source: k8s/kubeconfig.tera
        target: ~/.kube/config
    permissions:               # set file permissions (octal mode string)
      "~/.ssh/config": "600"
      "~/.ssh": "700"

  system:
    shell: /bin/zsh            # set default shell via chsh

    macos-defaults:            # macOS `defaults write` settings
      NSGlobalDomain:
        AppleShowAllExtensions: true
        NSAutomaticSpellingCorrectionEnabled: false
      com.apple.dock:
        autohide: true
        tilesize: 48
      com.apple.screensaver:
        askForPassword: 1
        askForPasswordDelay: 0

    launch-agents:             # macOS LaunchAgents
      - name: com.example.myservice
        program: /usr/local/bin/myservice
        args: ["--config", "/etc/myservice.conf"]
        run-at-load: true

    systemd-units:             # systemd user units (Linux)
      - name: myservice.service
        unit-file: systemd/myservice.service
        enabled: true

  secrets:
    - source: secrets/api-keys.yaml       # SOPS-encrypted file
      target: ~/.config/api-keys.yaml
    - source: 1password://Work/GitHub/token   # external provider reference
      target: ~/.config/gh/token
      template: "token: ${secret:value}"       # optional template wrapping
    - source: secrets/tls-cert.pem
      target: /etc/ssl/certs/my-cert.pem
      backend: age                             # override backend for this file

  scripts:
    pre-apply:                 # run before applying changes
      - scripts/pre-setup.sh
    post-apply:                # run after applying changes
      - scripts/post-setup.sh
```

### Profile Inheritance

Profiles can inherit from other profiles using `inherits`. The inheritance chain is resolved depth-first, left-to-right, with the active profile applied last.

Given `work` inherits `[base, macos]` and `base` inherits `[core]`, the resolution order is:

```
core → base → macos → work
```

Merge rules by resource type:

| Resource | Merge Strategy |
|---|---|
| `variables` | Override — later profile replaces earlier for same key |
| `packages` | Union — all packages from all layers combined, deduplicated |
| `files.managed` | Overlay — later profile's file wins for same target path |
| `files.permissions` | Override — later profile replaces earlier for same path |
| `system` | Deep merge — later profile overrides at the leaf key level |
| `secrets` | Append — deduplicated by target path, later wins on conflict |
| `scripts` | Append — all scripts from all layers run in resolution order |

### Tera Templates

Files with a `.tera` extension are rendered through the [Tera](https://keats.github.io/tera/) template engine before being placed at their target. The `.tera` extension is stripped from the target filename.

Available in template context:

- All profile `variables` as top-level keys
- `__os` — operating system (`linux`, `macos`, etc.)
- `__arch` — architecture (`x86_64`, `aarch64`)
- `__hostname` — machine hostname

Custom functions:

- `os()` — returns the OS name
- `hostname()` — returns the hostname
- `arch()` — returns the architecture
- `env(name="VAR")` — reads an environment variable

Example `.gitconfig.tera`:

```ini
[user]
    name = {{ GIT_AUTHOR_NAME }}
    email = {{ GIT_AUTHOR_EMAIL }}

[core]
    editor = {{ EDITOR }}

{% if __os == "macos" %}
[credential]
    helper = osxkeychain
{% endif %}
```

### Secrets

cfgd supports two encryption backends and three external secret providers.

**SOPS (primary)** — Encrypts values within structured YAML/JSON files. Keys remain in plaintext, so diffs are meaningful and files are safe to commit. Wraps the `sops` CLI. Supports age, AWS KMS, GCP KMS, and Azure Key Vault as key sources.

**age (fallback)** — Encrypts entire files opaquely. Used for binary files where SOPS doesn't apply.

**External providers:**

| Provider | Reference Format | CLI Required |
|---|---|---|
| 1Password | `1password://Vault/Item/Field` or `op://Vault/Item/Field` | `op` |
| Bitwarden | `bitwarden://folder/item` or `bw://folder/item` | `bw` |
| HashiCorp Vault | `vault://secret/path#key` | `vault` |

Secret references can be used in templates with `${secret:ref}` syntax. They are resolved at apply time and never written to the source directory.

### System Configurators

The `system:` section routes each key to a registered system configurator. Available configurators depend on the OS.

**`shell`** — Sets the default login shell via `chsh`. Value is the path to the shell binary.

**`macos-defaults`** (macOS only) — Reads and writes macOS `defaults` domains. Each key under `macos-defaults` is a domain name, and the values are key-value pairs to set.

**`launch-agents`** (macOS only) — Manages LaunchAgent plist files in `~/Library/LaunchAgents`. Each entry specifies a name, program, arguments, and whether to run at load.

**`systemd-units`** (Linux only) — Manages systemd user unit files. Each entry specifies a unit name, path to the unit file source, and whether the unit should be enabled.

Configurators that aren't available on the current OS are silently skipped.

---

## Package Managers

cfgd manages packages across six package managers. Each is implemented behind a trait, so the reconciler works the same way regardless of which managers are available.

| Manager | Platforms | Config Key | What It Does |
|---|---|---|---|
| Homebrew | macOS, Linux | `brew` | Manages taps, formulae, and casks separately |
| apt | Debian/Ubuntu | `apt` | `apt-get install` with sudo handling |
| Cargo | Any (with Rust) | `cargo` | `cargo install` |
| npm | Any (with Node) | `npm` | `npm install -g` |
| pipx | Any (with Python) | `pipx` | `pipx install` |
| dnf | Fedora/RHEL | `dnf` | `dnf install` |

Package managers that aren't installed on the current system are silently skipped. `cfgd plan` shows which managers will be used and which packages will be installed or removed.

---

## Reconciliation Model

cfgd follows the Kubernetes controller pattern: declare desired state, diff against actual state, generate a plan, apply it, watch for drift.

### Phases

Apply runs in a fixed phase order:

1. **System** — shell, macOS defaults, launch agents, systemd units
2. **Packages** — install/uninstall across all package managers
3. **Files** — copy, template, set permissions
4. **Secrets** — decrypt SOPS files, resolve external references
5. **Scripts** — run pre/post-apply scripts

Each phase can be applied independently with `cfgd apply --phase <name>`.

### Failure Handling

Failed actions within a phase don't abort the entire apply. They're logged, skipped, and reported at the end. This means a broken Homebrew tap won't prevent your SSH config from being placed.

### State Store

cfgd tracks state in a SQLite database at `~/.local/share/cfgd/state.db`. This stores:

- Apply history (timestamp, profile, status, summary)
- Drift events (what changed, expected vs actual)
- Managed resource inventory (what cfgd is responsible for)

---

## Daemon

The daemon runs as a long-lived process that watches for drift and optionally auto-corrects it.

It does three things:

1. **File watching** — Uses OS-native file watchers (inotify on Linux, FSEvents on macOS) to detect when managed files change. Changes are debounced (500ms window) to avoid reacting to partial writes.

2. **Reconciliation loop** — On a configurable interval (default 5 minutes), diffs the entire desired state against actual state and reports or fixes drift depending on configuration.

3. **Sync loop** — Pulls from the git remote on interval. Optionally auto-commits and pushes local changes.

The daemon exposes a health endpoint on a Unix socket at `/tmp/cfgd.sock`. `cfgd daemon --status` queries this socket to report whether the daemon is running, last reconcile time, and drift count.

### Notifications

When drift is detected, the daemon can notify via:

- **Desktop notifications** (default) — Uses native OS notification APIs
- **Stdout** — Logs to stdout (useful when running under systemd, which captures journal output)
- **Webhook** — POSTs a JSON payload to a configured URL

---

## Bootstrap

`cfgd init --from <url>` is designed to be the only command you run on a brand-new machine. The flow:

1. Clones the config repo
2. Discovers available profiles and lets you pick one interactively
3. Offers to set up secrets (generates age key, checks for sops/providers)
4. Shows the full plan of what will change
5. Asks for confirmation, then applies
6. Runs verification to confirm everything landed
7. Offers to install the daemon

The bootstrap is **resumable** — if interrupted at any point (network failure, reboot, ctrl-c), re-running `cfgd init` picks up where it left off.

The install script handles downloading the binary:

```sh
curl -fsSL https://raw.githubusercontent.com/TODO/cfgd/main/install.sh | sh -s -- init --from git@github.com:you/config.git
```

This detects your OS and architecture, downloads the right binary from GitHub releases, verifies the SHA256 checksum, puts it in your PATH, and then runs `cfgd init --from ...` for you.

---

## Global Flags

These flags work with any subcommand:

| Flag | Short | Description |
|---|---|---|
| `--config <path>` | | Path to cfgd.yaml (default: `cfgd.yaml` in current directory) |
| `--profile <name>` | | Override the active profile |
| `--verbose` | `-v` | Show debug output |
| `--quiet` | `-q` | Suppress all non-error output |
| `--no-color` | | Disable colored terminal output |

---

## Building from Source

```sh
git clone https://github.com/TODO/cfgd.git
cd cfgd
cargo build --release
# binary is at target/release/cfgd
```

Requires Rust 1.70+. Dependencies are vendored via Cargo.

### Running Tests

```sh
cargo test
```

### Linting

```sh
cargo fmt --check
cargo clippy -- -D warnings
```

---

## Future Plans

cfgd is currently a single-binary CLI + daemon for managing individual machines. Planned additions:

- **cfgd-server** — Fleet control plane with web UI, device auth, drift dashboards, deployed as a Kubernetes operator
- **cfgd-node** — Node-level config agent for Kubernetes clusters (sysctl, kernel modules, containerd, kubelet, AppArmor, seccomp)
- **Team Config Controller** — Multi-source config management where teams publish config baselines with policy tiers and developers subscribe

## License

MIT
