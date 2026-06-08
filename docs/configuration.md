# Configuration

cfgd config files follow a structure inspired by the [Kubernetes Resource Model](https://github.com/kubernetes/design-proposals-archive/blob/main/architecture/resource-management.md): every document has `apiVersion`, `kind`, `metadata`, and `spec` fields. This gives a consistent shape across configs, profiles, modules, and sources. TOML is also supported (use `.toml` extension).

For the complete field-by-field reference, see the [Config spec reference](spec/config.md).

## Editor Support

cfgd publishes JSON Schemas for each config document — `cfgd.yaml`, modules
(`modules/<name>/module.yaml`), profiles (`profiles/*.yaml`), and config sources
(`cfgd-source.yaml`) — so editors with a YAML language server (VS Code, Neovim,
JetBrains, …) can offer completion and inline validation.

The schemas are self-hosted at `https://cfgd.io/schemas/` and registered with
[SchemaStore](https://www.schemastore.org/) on each release, so for the standard
file names above no setup is needed once your editor's YAML extension picks up
the SchemaStore catalog. To pin a schema explicitly (or for non-standard file
names), add a modeline to the top of the file:

```yaml
# yaml-language-server: $schema=https://cfgd.io/schemas/cfgd-config.schema.json
apiVersion: cfgd.io/v1alpha1
kind: Config
# ...
```

Swap the URL for `cfgd-module`, `cfgd-profile`, or `cfgd-source` as appropriate.

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
| `spec.origin.sshStrictHostKeyChecking` | no | `AcceptNew` | SSH host key policy: `AcceptNew` (accept first-seen), `Yes` (require known_hosts), `No` (insecure) |
| `spec.daemon.reconcile.interval` | no | `5m` | Drift check interval (e.g. `1m`, `5m`, `1h`) |
| `spec.daemon.reconcile.onChange` | no | `false` | Reconcile immediately on file change |
| `spec.daemon.reconcile.patches` | no | `[]` | Per-module/profile reconcile overrides (see [daemon.md](daemon.md#reconcile-patches)) |
| `spec.daemon.sync.autoPull` | no | `false` | Auto-pull from remote |
| `spec.daemon.sync.autoPush` | no | `false` | Auto-commit and push local changes |
| `spec.daemon.notify.method` | no | `Desktop` | `Desktop`, `Stdout`, or `Webhook` |
| `spec.secrets.backend` | no | `sops` | `sops` or `age` (see [secrets.md](secrets.md) for when to use which) |
| `spec.theme` | no | `default` | Theme name (string) or object with `name` + `overrides` |
| `spec.fileStrategy` | no | `Symlink` | `Symlink`, `Copy`, `Template`, or `Hardlink` (Windows: `Symlink` requires Developer Mode or elevation) |
| `spec.aliases.<name>` | no | — | CLI command aliases (e.g. `add: "profile update --file"`) |
| `spec.compliance` | no | — | Continuous compliance snapshot settings. Reports the effective desired state (profile + modules), and file checks are content-aware (see [spec/config.md](spec/config.md#speccompliance)) |

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

## File locations

cfgd stores three kinds of per-user data, each resolved independently. Pass
`--config <path>` / `CFGD_CONFIG` and `--state-dir <dir>` / `CFGD_STATE_DIR` to
override the config file and state directory explicitly.

| Data | Default location |
|---|---|
| **Config** (`cfgd.yaml`, `profiles/`, `files/`, `modules.lock`) | `$XDG_CONFIG_HOME/cfgd` if set, else the platform default below |
| **State** (`state.db`, history, drift, apply journal) | platform-native data dir — Linux `$XDG_DATA_HOME/cfgd` or `~/.local/share/cfgd`, macOS `~/Library/Application Support/cfgd`, Windows `%LOCALAPPDATA%\cfgd` |
| **Runtime** (daemon socket, pid files) | Linux `$XDG_RUNTIME_DIR/cfgd` (else `~/.cache/cfgd`), macOS `~/Library/Application Support/cfgd`, Windows `%LOCALAPPDATA%\cfgd` |

The **config** platform default per OS (used only when `XDG_CONFIG_HOME` is
unset):

| Platform | Config default | Notes |
|---|---|---|
| Linux | `~/.config/cfgd` | the XDG config base |
| macOS | `~/Library/Application Support/cfgd` | the native macOS location — shares one root with state and runtime (see migration below) |
| Windows | `%APPDATA%\cfgd` | the roaming app-data base |

`XDG_CONFIG_HOME` is honored on **every** platform (including macOS and Windows)
when it is set to a non-empty, absolute path; an empty or relative value is
ignored per the XDG Base Directory spec. Setting `XDG_CONFIG_HOME` relocates the
config dir on any platform — and is the supported way to keep config under
`~/.config` on macOS.

### macOS: legacy `~/.config/cfgd` migration

Earlier builds stored macOS config at `~/.config/cfgd`. A config dir already
there is **always preferred and read in place**, so upgrading never strands it.
On the first interactive run after the default changed, cfgd prompts once:

```text
Your cfgd config is at ~/.config/cfgd, but the native macOS location is now
~/Library/Application Support/cfgd. How would you like to proceed?
> Move it to ~/Library/Application Support/cfgd
  Keep it at ~/.config (set XDG_CONFIG_HOME in your shell config)
```

- **Move** relocates the directory to the native location (symlinked entries are
  preserved; cfgd refuses if the destination already exists).
- **Keep** sets `XDG_CONFIG_HOME` for the current session and persists it so all
  future shells resolve there. The export is written to the file your shell
  sources for **every** invocation (not just interactive ones): `~/.zshenv` for
  zsh, `~/.profile` for bash, `~/.config/fish/conf.d/cfgd-xdg.fish` for fish. A
  symlinked rc (e.g. into a dotfiles repo) is followed and edited in place, and
  an existing `XDG_CONFIG_HOME` assignment is left untouched. Unrecognized shells
  get printed instructions instead of a guessed file.

The prompt is suppressed when `XDG_CONFIG_HOME` or `--config`/`CFGD_CONFIG`
already pins the location, after you've chosen **Keep** once, for `cfgd daemon`,
and in non-interactive sessions (`--yes`/`CFGD_YES`, no TTY, or structured `-o`
output) — there cfgd silently keeps reading the legacy dir in place. Only the
config dir is affected; **state** and **runtime** data stay under
`~/Library/Application Support/cfgd`. That split is intentional: managed-file
symlink targets are declared explicitly in each file entry, so they don't depend
on where the config dir resides.

## Linux

On Linux, cfgd supports desktop environment-specific system configurators in addition to the cross-platform features:

| Feature | Linux behavior |
|---|---|
| Desktop configurators | `gsettings` (GNOME/GTK), `kdeConfig` (KDE Plasma), `xfconf` (XFCE) — each active only when its CLI tool is installed |
| System configurators | `systemdUnits`, `environment`; plus node-level configurators (`sysctl`, `kernelModules`, `containerd`, `kubelet`, `apparmor`, `seccomp`, `certificates`) |
| `spec.env` reach | `envScope: All` (default) writes `~/.config/environment.d/cfgd.conf` (read by `systemd --user` + Wayland GUI sessions) and refreshes the live session via `systemctl --user set-environment` |
| Daemon service | Registered as a systemd user service; starts at login |

## Windows

On Windows, cfgd supports the same configuration structure with these platform-specific behaviors:

| Feature | Windows behavior |
|---|---|
| Package managers | `winget`, `chocolatey`, `scoop` (in addition to cross-platform managers like `cargo`, `npm`, `pipx`) |
| System configurators | `windowsRegistry`, `windowsServices`; `shell` targets Windows Terminal; `environment` writes to `HKCU\Environment` via `setx` |
| `spec.env` reach | Writes `~/.cfgd-env.ps1` dot-sourced from the PowerShell profiles (and Git Bash rc when present); `envScope: All` (default) also persists vars to `HKCU\Environment` via `setx` |
| File strategy | `Symlink` requires Developer Mode or an elevated prompt; `Copy` is a safe default |
| Daemon service | Registered as a Windows Service via `sc.exe`; starts at boot; logs to `%LOCALAPPDATA%\cfgd\daemon.log` |
| Config directory | `%APPDATA%\cfgd` (equivalent to `~/.config/cfgd` on Unix) |

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
| `--config <path>` | | `CFGD_CONFIG` | Path to `cfgd.yaml` (or a directory — cfgd infers `cfgd.yaml`, then `cfgd.toml`, inside it) |
| `--profile <name>` | | `CFGD_PROFILE` | Override the active profile |
| `--verbose` | `-v` | `CFGD_VERBOSE` | Show debug output (`-vv` = trace) |
| `--quiet` | `-q` | `CFGD_QUIET` | Suppress all non-error output |
| `--no-color` | | `NO_COLOR` | Disable colored terminal output |
| `--output <format>` | `-o` | | Output format: `table` (default), `wide`, `json`, `yaml`, `name`, `jsonpath=EXPR`, `template=TMPL`, `template-file=PATH` |

Boolean env vars accept shell-truthy spellings, not just `true`/`false`. The
accept-set matches `CFGD_YES`: `1`/`y`/`yes`/`t`/`true`/`on` (case-insensitive)
enable, `0`/`n`/`no`/`f`/`false`/`off` disable.

```sh
CFGD_QUIET=1   cfgd profile list -o name   # same as -q
CFGD_VERBOSE=on cfgd plan                  # same as -v; bare integers still work (CFGD_VERBOSE=2 = trace)
```

#### Structured output shapes (`jsonpath` / `template`)

List commands emit a **bare top-level array**, not a kubectl-style `{"items": [...]}`
envelope. Index into it directly — `[0]`, not `.items[0]`:

```sh
cfgd profile list -o json                       # [ { "name": "base", ... }, ... ]
cfgd profile list -o 'jsonpath={[0].name}'      # base
cfgd profile list -o 'jsonpath={[*].name}'      # one name per line
cfgd profile list -o 'jsonpath={.items[0]}'     # empty — no `items` key on a bare array
```

Single-object commands (e.g. `cfgd status`) expose their fields directly, so
`jsonpath={.field}` works against them:

```sh
cfgd status -o 'jsonpath={.drift}'              # extract drift events
```

A malformed `jsonpath` or `template` expression is rejected at parse time with a
usage error (exit `2`); a template that fails to render against the data, or a
`template-file` that cannot be read, writes the error to `stderr` and exits non-zero
(exit `1`) — the structured data channel on `stdout` is never polluted with an error
message, and a failure never reports exit `0`.

The standalone `--jsonpath EXPR` flag is **deprecated** in favor of
`-o jsonpath=EXPR`. It still works but prints a deprecation notice to `stderr`
(the `stdout` data channel stays pure), so scripts piping `stdout` are unaffected:

```sh
cfgd profile list --jsonpath '{[0].name}'   # stdout: base; stderr: deprecation notice
cfgd profile list -o 'jsonpath={[0].name}'  # canonical — no notice
```
