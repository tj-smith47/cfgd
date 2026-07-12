# Configuration

cfgd config files follow a structure inspired by the [Kubernetes Resource Model](https://github.com/kubernetes/design-proposals-archive/blob/main/architecture/resource-management.md): every document has `apiVersion`, `kind`, `metadata`, and `spec` fields. This gives a consistent shape across configs, profiles, modules, and sources. TOML is also supported (use `.toml` extension).

The only supported `apiVersion` is `cfgd.io/v1alpha1`. Any other value (e.g. a future `cfgd.io/v1alpha2`) is rejected at parse time with an error naming the supported version, rather than being silently loaded under the current schema.

For the complete field-by-field reference, see the [Config spec reference](spec/config.md).

## Editor Support

cfgd publishes JSON Schemas for each config document — `cfgd.yaml`, modules
(`modules/<name>/module.yaml`), profiles (`profiles/<name>/profile.yaml`), and
config sources (`cfgd-source.yaml`) — so editors with a YAML language server
(VS Code, Neovim, JetBrains, …) can offer completion and inline validation.

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

cfgd's scaffolders (`cfgd init`, `cfgd profile create`, `cfgd module create`, and
AI generate) emit this modeline as the first line of every manifest they write, so
generated files validate immediately even where the SchemaStore catalog does not
match — including legacy flat profiles (`profiles/<name>.yaml`), files reached
through a dot-directory, and hand-renamed manifests. The SchemaStore catalog
associates the canonical bundle path `profiles/<name>/profile.yaml`; the modeline
covers everything else.

CLI commands that rewrite a manifest in place (`cfgd config set`, `cfgd module
update`, `cfgd profile switch`, `cfgd profile update`, source mutations, …)
preserve the file's **leading** comment block — the modeline and any banner
comments above the first YAML key survive the rewrite. Comments elsewhere in
the document are not preserved.

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
    - type: Git
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

  update:
    policy: Prompt         # cfgd binary self-update behavior (default: Prompt)
    interval: 24h          # check cadence when policy != Manual (default: 24h)
    channel: stable        # release channel (default: cfgd's built-in channel)
    skills:
      policy: Inherit      # follows spec.update.policy unless overridden (default: Inherit)

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
| `spec.update.policy` | no | `Prompt` | cfgd binary self-update behavior: `Auto`, `Prompt`, `Notify`, or `Manual` (see [Update behavior](#update-behavior-specupdate)) |
| `spec.update.interval` | no | `24h` | Update-check cadence when `policy != Manual` (e.g. `30m`, `24h`, `7d`) |
| `spec.update.channel` | no | — | Release channel to track (e.g. `stable`, `prerelease`); unset uses cfgd's built-in default channel |
| `spec.update.skills.policy` | no | `Inherit` | Authored-skill refresh policy: `Inherit` (follow `spec.update.policy`), `Auto`, `Prompt`, `Notify`, or `Manual` |
| `spec.secrets.backend` | no | `sops` | `sops` or `age` (see [secrets.md](secrets.md) for when to use which) |
| `spec.theme` | no | `default` | Theme name (string) or object with `name` + `overrides` |
| `spec.fileStrategy` | no | `Symlink` | `Symlink`, `Copy`, `Template`, or `Hardlink` (Windows: `Symlink` requires Developer Mode or elevation) |
| `spec.aliases.<name>` | no | — | CLI command aliases (e.g. `add: "profile update --file"`) |
| `spec.compliance` | no | — | Continuous compliance snapshot settings. Reports the effective desired state (profile + modules), and file checks are content-aware (see [spec/config.md](spec/config.md#speccompliance)) |

All fields can be read and written programmatically via `cfgd config get <key>` and `cfgd config set <key> <value>`. See the [CLI reference](cli-reference.md) for details.

Enum-valued fields (e.g. `spec.fileStrategy`, `spec.daemon.driftPolicy`, `spec.daemon.notify.method`, the profile-level `spec.envScope`, `spec.compliance.export.format`) are parsed case-insensitively — `Symlink`, `symlink`, and `SYMLINK` are all accepted. The documented PascalCase form is canonical and is what cfgd writes back.

## Update behavior (`spec.update`)

cfgd can check for its own updates (it doesn't by default — `cfgd upgrade` is
otherwise purely manual), and separately decide whether installed [authoring
skills](skill.md) are re-rendered when cfgd moves. Both are governed by
`spec.update`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  update:
    policy: Prompt         # cfgd binary self-update behavior (default: Prompt)
    interval: 24h          # check cadence when policy != Manual (default: 24h)
    channel: stable        # release channel (default: cfgd's built-in channel)
    skills:
      policy: Inherit      # follows spec.update.policy unless overridden (default: Inherit)
```

`spec.update.policy` is the one posture knob; by default it governs both the
binary and skill refresh. Override `spec.update.skills.policy` only to decouple
skill refresh from the binary. "update" is the umbrella verb for keeping things
current; "upgrade" is the specific binary-replacement action (`cfgd upgrade`),
which `policy: Auto`/`Prompt` drives.

### Update policies

The binary policy (`spec.update.policy`) is an `UpdatePolicy`:

| Policy | Meaning |
|---|---|
| `Auto` | on a detected newer version, apply it unattended |
| `Prompt` | check, then ask before applying (interactive CLI); non-interactive falls back to `Notify` |
| `Notify` | check and surface/record availability; never apply, never prompt |
| `Manual` | cfgd does nothing automatically — no check, no notice; you drive it |

The skill policy (`spec.update.skills.policy`) is a `SkillUpdatePolicy` — the
same four values **plus** `Inherit`, which is its default:

| Skill policy | Meaning |
|---|---|
| `Inherit` *(default)* | use the binary `spec.update.policy` value |
| `Auto` / `Prompt` / `Notify` / `Manual` | as above, but for skill refresh only |

### At most one update surface, ever

Skill staleness is a *consequence* of a binary version change (a skill is stale
only when the running cfgd is newer than its stamp), so the two surfaces are
naturally serialized — binary first, skills after. Three rules dedup the only
collision (skills left stale from a past skipped refresh *and* a newer binary
now available), so you'll never see two update prompts:

1. **Binary outranks skills.** While a binary update is pending/available, the
   skill surface is suppressed — only the binary surface shows. (Refreshing
   skills against a binary you're about to replace is wasted work.)
2. **Ride-along.** When a binary upgrade actually happens (`Auto`, an accepted
   `Prompt`, or a manual `cfgd upgrade`), the user-scope skill refresh is part of
   **that same action and output block** — never a second prompt.
3. **One consolidated skill surface.** When skills are surfaced standalone
   (binary current, skills stale), a single notice covers both user- and
   project-scope staleness — never one notice per scope.

### Scope governs auto vs manual (the git-safety invariant)

> **cfgd never auto-rewrites tracked project files.** Ride-along and
> `Auto`/`Inherit→Auto` refresh touch **user-scope (home) skills only**.
> **Project-scope skills are always manual** — regardless of policy — because
> they are committed, and a surprise diff is unacceptable. The consolidated
> surface (rule 3) tells you project skills are stale so you can run
> `cfgd skill update` and commit deliberately.

| Effective skills policy | User-scope on version change | Project-scope |
|---|---|---|
| `Auto` (incl. `Inherit→Auto`) | re-render (ride-along if same action) | notice only, never written |
| `Prompt` / `Inherit→Prompt` | refresh rides along with the accepted binary upgrade; no separate prompt | notice only |
| `Notify` / `Inherit→Notify` | a single stale notice (rule 3); no write | notice only |
| `Manual` / `Inherit→Manual` | nothing (silent); you run `cfgd skill update` | nothing |

In daemon context, `Notify` records a structured event rather than prompting.

## Repository Layout

```
my-config/
├── cfgd.yaml              # root config
├── profiles/              # each profile is a bundle: <name>/profile.yaml + payload
│   ├── base/
│   │   └── profile.yaml   # base profile — shared across machines
│   ├── work/
│   │   ├── profile.yaml   # inherits base, adds work config
│   │   └── files/         # profile-owned file payload (created by --file)
│   └── personal/
│       └── profile.yaml
├── modules/               # reusable config modules
│   ├── nvim/
│   │   ├── module.yaml
│   │   └── files/
│   └── tmux/
│       ├── module.yaml
│       └── files/
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

Each profile is a self-contained bundle: a fixed-name `profiles/<name>/profile.yaml`
manifest alongside its own `files/` payload directory (mirroring the
`modules/<name>/module.yaml` shape). The legacy flat form `profiles/<name>.yaml`
remains fully supported — both forms load, and existing flat profiles keep working
untouched. Run `cfgd profile migrate <name>` (or `--all`) to move a flat profile
into the canonical bundle form. Having both `profiles/work/profile.yaml` and
`profiles/work.yaml` on disk is a hard error (ambiguous); migrate or delete one.

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

cfgd stores four kinds of data, each resolved independently. Every root can be
relocated explicitly (see [Overriding a directory root](#overriding-a-directory-root)
below), and `cfgd paths` prints the resolved values on any host.

| Data | Default location |
|---|---|
| **Config** (`cfgd.yaml`, `profiles/`, `files/`, `modules.lock`) | `$XDG_CONFIG_HOME/cfgd` if set, else the platform default below |
| **State** (`state.db`, history, drift, apply journal, `apply.lock`, compliance exports, device credential) | platform-native state dir — Linux `$XDG_STATE_HOME/cfgd` or `~/.local/state/cfgd`, macOS `~/Library/Application Support/cfgd/state`, Windows `%LOCALAPPDATA%\cfgd\state` |
| **Cache** (source cache, module cache) | platform-native cache dir — Linux `$XDG_CACHE_HOME/cfgd` or `~/.cache/cfgd`, macOS `~/Library/Caches/cfgd`, Windows `%LOCALAPPDATA%\cfgd`. Sources live under `<cache>/sources`, modules under `<cache>/modules`. |
| **Runtime** (daemon socket, pid files) | Linux `$XDG_RUNTIME_DIR/cfgd` (else `~/.cache/cfgd/runtime`), macOS `~/Library/Application Support/cfgd/runtime`, Windows `%LOCALAPPDATA%\cfgd` |

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

### System scope

Pass `--scope system` (or `CFGD_SCOPE=system`) to switch all four roots to their
machine-wide FHS / `/Library` equivalents:

| Root | Linux system | macOS system |
|---|---|---|
| Config | `/etc/cfgd` | `/Library/Application Support/cfgd` |
| State | `/var/lib/cfgd` | `/Library/Application Support/cfgd/state` |
| Cache | `/var/cache/cfgd` | `/Library/Caches/cfgd` |
| Runtime | `/run/cfgd` | `/Library/Application Support/cfgd/runtime` |

Windows is always system-scope; `--scope system` is a no-op there.

```console
$ cfgd --scope system paths
cfgd directories (scope: system)

Config
  dir    /etc/cfgd
  source default

State
  dir    /var/lib/cfgd
  source default

Cache
  dir    /var/cache/cfgd
  source default

Runtime
  dir    /run/cfgd
  source default
```

### Overriding a directory root

Each root has a dedicated flag and environment variable. The resolution
precedence for every root is:

```text
--<role>-dir flag  >  CFGD_<ROLE>_DIR env  >  $*_DIRECTORY (systemd, system scope)  >  scope default  >  platform default
```

The `$*_DIRECTORY` tier applies only under system scope on Linux: when cfgd runs
as a systemd system service, systemd injects `$CONFIGURATION_DIRECTORY`,
`$STATE_DIRECTORY`, `$CACHE_DIRECTORY`, and `$RUNTIME_DIRECTORY`; cfgd reads the
first `:`-separated entry from each and prefers it over the FHS defaults. This
means any systemd override (e.g. `StateDirectory=/srv/cfgd-state`) is honored
without any extra cfgd configuration.

The XDG base per role (`XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`,
`XDG_RUNTIME_DIR`) applies under user scope only.

| Root | Flag | Env var |
|---|---|---|
| Config | `--config-dir <dir>` (or `--config <file>`, which wins) | `CFGD_CONFIG_DIR` (or `CFGD_CONFIG`) |
| State | `--state-dir <dir>` | `CFGD_STATE_DIR` |
| Cache | `--cache-dir <dir>` | `CFGD_CACHE_DIR` |
| Runtime | `--runtime-dir <dir>` | `CFGD_RUNTIME_DIR` |

The roots are independent — overriding one does not move the others. `--config`
names the config *file* (or a directory cfgd searches for `cfgd.yaml`/`cfgd.toml`)
and takes precedence over `--config-dir`. `--cache-dir` relocates **both** the
source and module caches (they share one root). `--runtime-dir` relocates the
daemon socket and lock files, and is honored by both `cfgd daemon` and
`cfgd daemon status` so they always agree on the socket path.

### `cfgd paths`

`cfgd paths` reports the four resolved roots, the effective source of each
(`flag`, `env`, or `default`), and the files cfgd owns in each — so you never
have to guess where a host is reading or writing:

```console
$ cfgd paths
cfgd directories

Config
  dir     /home/you/.config/cfgd
  source  default
  file    /home/you/.config/cfgd/cfgd.yaml

State
  dir       /home/you/.local/state/cfgd
  source    default
  db        /home/you/.local/state/cfgd/state.db
  applyLock /home/you/.local/state/cfgd/apply.lock

Cache
  dir     /home/you/.cache/cfgd
  source  default
  sources /home/you/.cache/cfgd/sources
  modules /home/you/.cache/cfgd/modules

Runtime
  dir     /run/user/1000/cfgd
  source  default
  socket  /run/user/1000/cfgd/cfgd.sock
```

`cfgd paths -o json` (or `-o yaml`) emits the same data as a structured object
for scripts; the `source` field reflects any override in effect:

```console
$ cfgd --cache-dir /srv/cfgd-cache paths -o json
{
  "cache": {
    "dir": "/srv/cfgd-cache",
    "modules": "/srv/cfgd-cache/modules",
    "source": "flag",
    "sources": "/srv/cfgd-cache/sources"
  },
  ...
}
```

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

### Silent state & cache migration

Earlier builds kept the state DB and the source cache together in one data dir
(`~/.local/share/cfgd` on Linux, `~/Library/Application Support/cfgd` on macOS,
`%LOCALAPPDATA%\cfgd` on Windows). cfgd now resolves **state** and **cache** to
their own roots (the table above). On the first run after upgrading, cfgd
relocates that data to the new defaults automatically — **no prompt**. Unlike the
config dir, state and cache are app-managed (not hand-authored, not git-tracked),
so there is nothing to ask: the state DB (with its WAL sidecars and the device
credential), the queued server config, and the `sources/` cache move to their
new homes, while the module cache — already in the cache root — stays put.

The migration is safe by construction:

- **Per-artifact, never whole-dir.** Only cfgd's own files move; anything else in
  the legacy directory (including a co-located config dir on macOS) is left
  untouched.
- **Crash-safe state DB.** The SQLite WAL is folded into the DB before the file
  is moved; if that step can't run (a locked or degraded DB) the WAL/SHM sidecars
  are carried across so no committed data is lost. An existing state DB at the new
  location is authoritative and never overwritten.
- **Idempotent.** Re-running is a no-op once everything is in place.
- **Override-aware.** The migration runs **only** when both the state and cache
  roots are at their defaults. If you pass `--state-dir`/`--cache-dir` or set
  `CFGD_STATE_DIR`/`CFGD_CACHE_DIR`, cfgd assumes you are driving (e.g. a
  throwaway location) and never moves data into an overridden root.

Run `cfgd paths` afterward to confirm the new locations.

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
| `--config-dir <dir>` | | `CFGD_CONFIG_DIR` | Override the config directory (`--config` wins over it) |
| `--state-dir <dir>` | | `CFGD_STATE_DIR` | Override the state directory (`state.db`, history, `apply.lock`) |
| `--cache-dir <dir>` | | `CFGD_CACHE_DIR` | Override the cache directory (source, module, and update-check caches) |
| `--runtime-dir <dir>` | | `CFGD_RUNTIME_DIR` | Override the runtime directory (daemon socket, locks) |
| `--profile <name>` | | `CFGD_PROFILE` | Override the active profile |
| `--verbose` | `-v` | `CFGD_VERBOSE` | Show debug output (`-vv` = trace) |
| `--quiet` | `-q` | `CFGD_QUIET` | Suppress all non-error output |
| `--no-color` | | `NO_COLOR` | Disable colored terminal output |
| `--output <format>` | `-o` | | Output format: `table` (default), `wide`, `json`, `yaml`, `name`, `jsonpath=EXPR`, `template=TMPL`, `template-file=PATH` |
| `--list-envelope` | | `CFGD_LIST_ENVELOPE` | Under `-o json`/`-o yaml`, wrap a top-level array in a KRM `List` envelope (`{apiVersion, kind: List, items}`) |
| `--scope <user\|system>` | | `CFGD_SCOPE` | Installation scope: `user` (default) or `system`. `system` switches all four directory roots to system/FHS defaults (`/etc/cfgd`, `/var/lib/cfgd`, …). See [System scope](configuration.md#system-scope). |

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

##### KRM `List` envelope (`--list-envelope`)

If you'd rather consume list output as a Kubernetes-style `List` object, pass
the global `--list-envelope` flag (or set `CFGD_LIST_ENVELOPE=1`). It wraps the
top-level array under an `apiVersion: cfgd.io/v1alpha1`, `kind: List`, and an
`items` array carrying the original elements. The default (flag absent) stays a
bare array — this is purely opt-in:

```sh
cfgd source list -o json
# [ { "name": "base", ... }, ... ]

cfgd source list -o json --list-envelope
# {
#   "apiVersion": "cfgd.io/v1alpha1",
#   "items": [ { "name": "base", ... }, ... ],
#   "kind": "List"
# }

cfgd source list -o yaml --list-envelope
# apiVersion: cfgd.io/v1alpha1
# items:
# - name: base
#   ...
# kind: List
```

(Object keys serialize alphabetically — `apiVersion`, `items`, `kind` — as with
every cfgd JSON/YAML payload; key order is not semantically meaningful.)

The envelope shifts the path of every element: a bare-array `[0].name` becomes
`.items[0].name` under the envelope. It applies **only** to `-o json` and
`-o yaml`. The projecting formats (`-o name`, `-o jsonpath=…`, `-o template=…`,
`-o template-file=…`) ignore it and keep operating on the bare data, so your
existing jsonpath/template expressions are never reshaped:

```sh
cfgd source list -o 'jsonpath={[0].name}' --list-envelope   # still indexes the bare array
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
