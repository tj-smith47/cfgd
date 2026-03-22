# Configuration

cfgd config files follow a structure inspired by the [Kubernetes Resource Model](https://github.com/kubernetes/design-proposals-archive/blob/main/architecture/resource-management.md): every document has `apiVersion`, `kind`, `metadata`, and `spec` fields. This gives a consistent shape across configs, profiles, modules, and sources. TOML is also supported (use `.toml` extension).

For the complete field-by-field reference, see the [Config spec reference](spec/config.md).

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
    type: Git
    url: git@github.com:me/machine-config.git
    branch: master

  daemon:
    enabled: true
    reconcile:
      interval: 5m
      onChange: true
    sync:
      autoPull: true
      autoPush: false
      interval: 5m
    notify:
      drift: true
      method: Desktop
      webhookUrl: https://...

  secrets:
    backend: sops
    sops:
      ageKey: ~/.config/cfgd/age-key.txt
    integrations:
      - name: 1password
      - name: bitwarden
      - name: vault

  sources:
    - name: acme-corp
      origin:
        type: Git
        url: git@github.com:acme-corp/dev-config.git
        branch: master
      subscription:
        profile: acme-backend
        priority: 500
        acceptRecommended: true
```

## Fields

| Field | Required | Default | Description |
|---|---|---|---|
| `spec.profile` | yes | — | Name of the profile YAML file to activate (without `.yaml`) |
| `spec.origin.type` | no | — | `Git` or `Server` |
| `spec.origin.url` | no | — | Repository URL |
| `spec.origin.branch` | no | `master` | Git branch |
| `spec.daemon.reconcile.interval` | no | `5m` | Drift check interval (e.g. `1m`, `5m`, `1h`) |
| `spec.daemon.reconcile.onChange` | no | `false` | Reconcile immediately on file change |
| `spec.daemon.reconcile.patches` | no | `[]` | Per-module/profile reconcile overrides (see [daemon.md](daemon.md#reconcile-patches)) |
| `spec.daemon.sync.autoPull` | no | `false` | Auto-pull from remote |
| `spec.daemon.sync.autoPush` | no | `false` | Auto-commit and push local changes |
| `spec.daemon.notify.method` | no | `Desktop` | `Desktop`, `Stdout`, or `Webhook` |
| `spec.secrets.backend` | no | `sops` | `sops` or `age` (see [secrets.md](secrets.md) for when to use which) |
| `spec.theme` | no | `default` | Theme name (string) or object with `name` + `overrides` |
| `spec.fileStrategy` | no | `Symlink` | `Symlink`, `Copy`, `Template`, or `Hardlink` |
| `spec.aliases.<name>` | no | — | CLI command aliases (e.g. `add: "profile update --file"`) |

All fields can be read and written programmatically via `cfgd config get <key>` and `cfgd config set <key> <value>`. See the [CLI reference](cli-reference.md) for details.

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
└── scripts/               # lifecycle hook scripts
    ├── pre-setup.sh
    └── post-setup.sh
```

## File Strategies

Profile files support four deployment strategies:

- **Symlink** (default) — creates a symbolic link from target to source. Changes to the source are immediately reflected.
- **Copy** — copies the source file to the target path. The target is independent of the source after apply.
- **Template** — renders the file through [Tera](templates.md) before copying. Auto-detected for `.tera` extension.
- **Hardlink** — creates a hard link. Both paths share the same inode.

```yaml
files:
  managed:
    - source: shell/.zshrc
      target: ~/.zshrc
      # strategy defaults to Symlink
    - source: git/.gitconfig
      target: ~/.gitconfig
      strategy: Copy
    - source: shell/.zshrc.tera   # .tera triggers template rendering
      target: ~/.zshrc
```

Files can be marked `private: true` to exclude them from git (added to `.gitignore`).

## Aliases

Define command aliases in `cfgd.yaml`. `cfgd init` scaffolds default aliases — edit or remove them as needed.

```yaml
spec:
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
    up: "apply --yes"
    s: "status"
    pkg: "profile update --package"
```

Default aliases (scaffolded by `cfgd init`):
- `add <path>` → `profile update --file <path>`
- `remove -<path>` → `profile update --file -<path>` (prefix with `-` to remove)

These are not hardcoded — they live in your cfgd.yaml and can be changed or removed.

## AI Configuration

Configure the AI provider for `cfgd generate`:

```yaml
spec:
  ai:
    provider: claude              # AI provider (default: claude)
    model: claude-sonnet-4-6      # Model ID (default: claude-sonnet-4-6)
    apiKeyEnv: ANTHROPIC_API_KEY # Env var containing API key (default: ANTHROPIC_API_KEY)
```

API keys are never stored in config files. The `apiKeyEnv` field names the environment variable to read. CLI flags `--model` and `--provider` override config values.

## Global Flags

These flags work with any subcommand:

| Flag | Short | Env Var | Description |
|---|---|---|---|
| `--config <path>` | | `CFGD_CONFIG` | Path to cfgd.yaml |
| `--profile <name>` | | `CFGD_PROFILE` | Override the active profile |
| `--verbose` | `-v` | `CFGD_VERBOSE` | Show debug output |
| `--quiet` | `-q` | `CFGD_QUIET` | Suppress all non-error output |
| `--no-color` | | `NO_COLOR` | Disable colored terminal output |
| `--output <format>` | `-o` | | Output format: `table` (default), `wide`, `json`, `yaml`, `name`, `jsonpath=EXPR`, `template=TMPL`, `template-file=PATH` |
