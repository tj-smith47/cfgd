# Bootstrap

`cfgd init` is designed to be the only command you run on a brand-new machine. It scaffolds a configuration repository — creating `cfgd.yaml`, directory structure, and optionally cloning an existing config repo. With `--apply`, it also reconciles the machine in one shot.

## From a Config Repo

```sh
cfgd init --from git@github.com:you/machine-config.git
```

The flow:

1. **Check prerequisites** — verifies git is installed
2. **Clone** the config repo into the target directory (or pull if already cloned)
3. **Generate** release workflow if profiles/modules are present
4. **Init git** if the directory isn't already a repository

### Specifying a Branch

```sh
cfgd init --from git@github.com:you/machine-config.git --branch dev
```

If the branch isn't `master`, cfgd checks out the specified branch after cloning.

## Fresh Start

```sh
cfgd init
```

Creates `cfgd.yaml`, `profiles/`, `modules/`, `files/`, and `.gitignore` in the current directory. No profile is set — create one afterward:

```sh
cfgd profile create base
cfgd profile switch base
cfgd apply
```

### Custom Directory and Name

```sh
cfgd init ~/dotfiles
cfgd init ~/dotfiles --name my-config
```

The `path` argument specifies the target directory (default: current directory). `--name` sets the metadata name in `cfgd.yaml` (default: directory name).

## Apply After Init

Add `--apply` to reconcile the machine immediately after scaffolding:

```sh
cfgd init --from git@github.com:you/machine-config.git --apply
```

cfgd loads the active profile from the cloned repo, builds a reconciliation plan, shows it, and prompts for confirmation before applying. If no profile is configured, cfgd presents an interactive picker (or auto-selects if only one profile exists).

Use `--yes` to skip the confirmation prompt (useful for automated setups):

```sh
cfgd init --from git@github.com:you/machine-config.git --apply --yes
```

### Choosing a Profile

If the cloned repo has multiple profiles and none is set in `cfgd.yaml`, use `--apply-profile` to specify which one:

```sh
cfgd init --from git@github.com:you/machine-config.git --apply-profile work-mac
```

This sets the profile as active in `cfgd.yaml` and applies it. Errors if the profile doesn't exist.

Without `--apply-profile`, cfgd falls back to:
1. The `spec.profile` already set in cfgd.yaml
2. Interactive picker if multiple profiles exist
3. Auto-select if exactly one profile exists

### Applying Specific Modules

Use `--apply-module` (repeatable) to apply specific modules — with or without a profile:

```sh
# Apply just the nvim and tmux modules (no profile needed)
cfgd init --from git@github.com:you/machine-config.git --apply-module nvim --apply-module tmux

# Apply a profile plus additional modules
cfgd init --from git@github.com:you/machine-config.git --apply-profile work --apply-module nvim
```

When used without `--apply-profile` and no profile is configured, only the specified modules are applied. When used with a profile, the modules are applied in addition to whatever the profile already includes. Errors if any module isn't found.

### Install Daemon

Add `--install-daemon` to install the background sync service after init:

```sh
cfgd init --from git@github.com:you/machine-config.git --apply --yes --install-daemon
```

### One-Liner Bootstrap

Clone, apply a specific profile without prompts, and start continuous reconciliation:

```sh
cfgd init --from git@github.com:you/machine-config.git --apply-profile work-mac --yes --install-daemon
```

### Theme Selection

Set the output theme during init:

```sh
cfgd init --from git@github.com:you/machine-config.git --theme minimal
```

For fresh repos, the theme is written into `cfgd.yaml`. For cloned repos, the theme is injected into the existing `cfgd.yaml`.

## Team Onboarding

When `cfgd init --from` clones a repository that contains a `cfgd-source.yaml` at its root, cfgd enters a source-aware onboarding flow instead of treating it as a plain config repo.

```sh
cfgd init --from git@github.com:acme/dev-config.git
```

The flow:

1. **Clone and detect** — clones the repo and checks for `cfgd-source.yaml`. If found, continues with the team flow below. If not found, falls back to the standard config repo flow.

2. **Platform auto-detection** — detects your OS and distribution. If the source provides platform-specific profiles (macOS, Debian, Fedora, etc.), the matching one is automatically selected as a layer. If no match exists, the platform layer is skipped and you're informed.

3. **Profile selection** — shows available profiles with their descriptions. If `--apply-profile` was passed, that profile is used without prompting. If only one profile exists, it's auto-selected with confirmation.

4. **Policy tier review** — after profile selection, cfgd loads the merged profile and groups items by policy tier:
   - **Required + Locked** — shown for transparency, applied unconditionally
   - **Recommended** — prompted with default yes
   - **Optional** — prompted with default no
   - With `--yes`: all recommended are accepted, all optional are skipped

5. **Config creation** — creates `~/.config/cfgd/cfgd.yaml` with a source subscription pointing to the team repo, and a local `profiles/default.yaml` for your personal additions.

6. **Bootstrap apply** — runs `cfgd plan`, shows the plan, confirms, applies, and verifies. Offers to install the daemon for continuous sync.

If the cloned repo contains both a `cfgd.yaml` (personal config) and a `cfgd-source.yaml` (team source), cfgd uses the `cfgd.yaml` as the base config and registers the source as a subscription within it. This supports repos that serve as both a team baseline and a ready-to-use config.

### Adding More Sources After Bootstrap

Use `cfgd source add` to subscribe to additional sources after init:

```sh
cfgd source add git@github.com:acme/security-hardening.git
```

cfgd fetches the manifest, shows the policy breakdown, lets you set a priority, and confirms before subscribing. See [sources.md](sources.md) for details.

## AI-Guided Generation

If you're starting from scratch and want cfgd to generate your initial configuration by scanning your system, use `cfgd generate` instead of (or after) `cfgd init`:

```sh
cfgd init                  # scaffold an empty repo
cfgd generate              # scan system, generate modules and profiles
cfgd profile switch base   # activate the generated profile
cfgd apply                 # apply to the machine
```

The `generate` flow:

1. **Scan** — detects installed packages, dotfiles, shell config (aliases, exports, PATH), and system settings across all available package managers.
2. **Propose** — the AI proposes a module and profile structure based on what it found. Each tool typically becomes one module (`nvim`, `tmux`, `zsh`, etc.).
3. **Review** — each generated YAML file is shown to you before it is written. You can accept, request changes, or skip individual files.
4. **Write** — accepted files are written to `modules/<name>/module.yaml` and `profiles/<name>.yaml` in the current config repo.

You can also target a single tool or profile:

```sh
cfgd generate module nvim    # generate just the nvim module
cfgd generate profile work   # generate a work profile interactively
```

Use `--scan-only` to preview what cfgd finds without starting the AI conversation:

```sh
cfgd generate --scan-only
```

Requires `ANTHROPIC_API_KEY` in your environment, or `spec.ai.api-key` set in `cfgd.yaml`. See [cli-reference.md](cli-reference.md#cfgd-generate) for all flags.

## Adding Modules

Modules can be added after init with `cfgd module create` for local ones, or referenced in profile YAML for remote modules:

```sh
cfgd init --from git@github.com:you/machine-config.git
cfgd module create my-tool
cfgd apply
```

See [modules.md](modules.md) for module details.

## Install Script

The install script handles downloading the binary and optionally bootstrapping:

```sh
# Just install the binary
curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh

# Install and bootstrap in one step
curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh -s -- init --from git@github.com:you/config.git
```

The script detects your OS and architecture, downloads the right binary from GitHub releases, verifies the SHA256 checksum, and places it in your PATH.

### Homebrew

```sh
brew install tj-smith47/tap/cfgd
```

## Server Enrollment

Enrollment is a separate command from init. For managed devices, use `cfgd enroll` after init:

```sh
cfgd enroll --server-url https://cfgd.acme.com --token <bootstrap-token>
```

Enrollment supports two methods:

- **Token-based**: exchange a bootstrap token for a device credential
- **Key-based**: challenge-response using SSH or GPG keys

```sh
cfgd enroll --server-url https://cfgd.acme.com --ssh-key ~/.ssh/id_ed25519
cfgd enroll --server-url https://cfgd.acme.com --gpg-key ABCD1234
```

After enrollment, the device receives a permanent API key and pulls configuration from the device gateway. See [operator.md](operator.md#device-gateway) for enrollment details.
