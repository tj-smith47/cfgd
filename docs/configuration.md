# Configuration

cfgd config files follow a structure inspired by the [Kubernetes Resource Model](https://github.com/kubernetes/design-proposals-archive/blob/main/architecture/resource-management.md): every document has `apiVersion`, `kind`, `metadata`, and `spec` fields. This gives a consistent shape across configs, profiles, modules, and sources. TOML is also supported (use `.toml` extension).

## Root Config — `cfgd.yaml`

The entry point. Tells cfgd which profile to activate, where config is stored, and how the daemon behaves.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work

  origin:
    type: git
    url: git@github.com:me/machine-config.git
    branch: main

  daemon:
    enabled: true
    reconcile:
      interval: 5m
      on-change: true
    sync:
      auto-pull: true
      auto-push: false
      interval: 5m
    notify:
      drift: true
      method: desktop
      webhook-url: https://...

  secrets:
    backend: sops
    sops:
      age-key: ~/.config/cfgd/age-key.txt
    integrations:
      - name: 1password
      - name: bitwarden
      - name: vault

  sources:
    - name: acme-corp
      origin:
        type: git
        url: git@github.com:acme-corp/dev-config.git
        branch: main
      subscription:
        profile: acme-backend
        priority: 500
        accept-recommended: true
```

## Fields

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
| `spec.secrets.backend` | no | `sops` | `sops` or `age` (see [secrets.md](secrets.md) for when to use which) |

## Repository Layout

```
my-config/
├── cfgd.yaml              # root config
├── profiles/
│   ├── base.yaml          # base profile — shared across machines
│   ├── work.yaml          # inherits base, adds work config
│   └── personal.yaml
├── modules/               # reusable config modules
│   ├── nvim/
│   │   ├── module.yaml
│   │   └── config/
│   └── tmux/
│       ├── module.yaml
│       └── config/
├── files/                 # source files for profiles
│   ├── shell/
│   │   ├── .zshrc
│   │   └── .zshrc.tera
│   ├── git/
│   │   └── .gitconfig
│   └── ssh/
│       └── config
├── secrets/               # SOPS-encrypted files
│   └── api-keys.yaml
└── scripts/               # pre/post-reconcile scripts
    ├── pre-setup.sh
    └── post-setup.sh
```

## File Strategies

Profile files support four deployment strategies:

- **symlink** (default) — creates a symbolic link from target to source. Changes to the source are immediately reflected.
- **copy** — copies the source file to the target path. The target is independent of the source after apply.
- **template** — renders the file through [Tera](templates.md) before copying. Auto-detected for `.tera` extension.
- **hardlink** — creates a hard link. Both paths share the same inode.

```yaml
files:
  managed:
    - source: shell/.zshrc
      target: ~/.zshrc
      # strategy defaults to symlink
    - source: git/.gitconfig
      target: ~/.gitconfig
      strategy: copy
    - source: shell/.zshrc.tera   # .tera triggers template rendering
      target: ~/.zshrc
```

Files can be marked `private: true` to exclude them from git (added to `.gitignore`).

## Aliases

Define custom command aliases in cfgd.yaml:

```yaml
spec:
  aliases:
    up: "apply --yes"
    s: "status"
    pkg: "profile update --active --add-package"
```

Built-in aliases:
- `add` → `profile update --active --add-file`
- `remove` → `profile update --active --remove-file`

## Global Flags

These flags work with any subcommand:

| Flag | Short | Env Var | Description |
|---|---|---|---|
| `--config <path>` | | `CFGD_CONFIG` | Path to cfgd.yaml |
| `--profile <name>` | | `CFGD_PROFILE` | Override the active profile |
| `--verbose` | `-v` | `CFGD_VERBOSE` | Show debug output |
| `--quiet` | `-q` | `CFGD_QUIET` | Suppress all non-error output |
| `--no-color` | | `NO_COLOR` | Disable colored terminal output |
