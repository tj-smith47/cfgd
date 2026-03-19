# CLI Reference

Complete command reference for `cfgd`. All commands respect [global flags](configuration.md#global-flags).

## Core Commands

### `cfgd generate`

AI-guided configuration generation. Interactively scans your system and generates organized cfgd profiles and modules.

#### Usage

```sh
cfgd generate                      # Full flow: scan, propose structure, generate all
cfgd generate module <name>        # Generate a module for a specific tool
cfgd generate profile <name>       # Generate a profile
```

#### Flags

| Flag | Description |
|---|---|
| `--model <model-id>` | Override AI model (default: from config or `claude-sonnet-4-6`) |
| `--provider <name>` | Override AI provider (default: claude) |
| `--yes`, `-y` | Skip confirmation prompts |
| `--scan-only` | Only scan system, don't start AI conversation |

The AI scans your installed packages, dotfiles, shell config, and system settings, then proposes a cfgd module and profile structure. Each generated file is shown to you for review before it is written. You can accept, reject, or give feedback. The session ends when all modules and profiles have been written or you exit.

Requires `ANTHROPIC_API_KEY` set in your environment, or `spec.ai.api-key-env` in `cfgd.yaml` to name the environment variable holding the key.

See [ai-generate.md](ai-generate.md) for the full walkthrough, MCP server setup, and troubleshooting.

### `cfgd mcp-server`

Start the MCP server for AI editor integration. Exposes cfgd's scan, inspect, and write tools over the Model Context Protocol (JSON-RPC stdin/stdout).

```sh
cfgd mcp-server
```

The server runs until stdin is closed. Configure your AI client to launch it automatically rather than running it directly. See [ai-generate.md](ai-generate.md#mcp-server-setup) for Claude Code and Cursor setup.

### `cfgd init`

Initialize a new cfgd configuration repository.

```sh
cfgd init                                          # interactive setup in current directory
cfgd init ~/dotfiles                               # scaffold in specific directory
cfgd init --from git@github.com:you/config.git     # clone and scaffold
cfgd init --from <url> --branch dev                # specify branch
cfgd init --from <url> --apply-profile work-mac    # clone, activate profile, apply
cfgd init --from <url> --apply-module nvim         # clone, apply just one module
cfgd init --from <url> --apply --yes --install-daemon  # full one-liner bootstrap
```

| Flag | Description |
|---|---|
| `[path]` | Target directory (default: current directory) |
| `--from <url>` | Clone from a remote git repository |
| `--branch <name>` | Git branch (default: master) |
| `--name <name>` | Config name in metadata (default: directory name) |
| `--apply` | Apply configuration after scaffolding |
| `--apply-profile <name>` | Activate and apply a specific profile (implies --apply, errors if not found) |
| `--apply-module <name>` | Apply a specific module (repeatable, implies --apply, errors if not found) |
| `--yes`, `-y` | Skip confirmation prompts (used with --apply) |
| `--install-daemon` | Install daemon service after init |
| `--theme <name>` | Theme name (default, dracula, solarized-dark, solarized-light, minimal) |

See [bootstrap.md](bootstrap.md) for the full init flow.

### `cfgd apply`

Apply the configuration plan.

```sh
cfgd apply                          # apply with confirmation
cfgd apply --dry-run                # preview without applying
cfgd apply --yes                    # skip confirmation
cfgd apply --phase packages         # single phase
cfgd apply --module nvim            # single module + deps
cfgd apply --only packages.brew     # dot-notation filter
cfgd apply --skip system.sysctl     # skip specific items
```

| Flag | Description |
|---|---|
| `--dry-run` | Preview changes without applying |
| `--phase <name>` | Apply only a specific phase |
| `--yes`, `-y` | Skip confirmation prompt |
| `--module <name>` | Apply only this module and its dependencies |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Apply only items matching dot-notation paths (repeatable) |

### `cfgd status`

Show configuration status, drift, and pending decisions.

```sh
cfgd status                                 # human-readable table
cfgd status -o json                         # full status as JSON
cfgd status -o json --jsonpath '{.drift}'   # extract drift events
```

### `cfgd diff`

Show detailed file diffs with syntax highlighting.

### `cfgd verify`

Check that all managed resources match desired state.

```sh
cfgd verify -o json   # structured pass/fail results
```

### `cfgd doctor`

Check system health: available package managers, configurators, module status, dependency versions.

```sh
cfgd doctor -o json   # structured health report
```

### `cfgd log`

Show apply history from the state store.

```sh
cfgd log              # last 20 entries
cfgd log --limit 50   # last 50 entries
cfgd log -o json      # JSON apply history
```

### `cfgd sync`

Pull from all remotes, show changes, prompt for apply.

### `cfgd pull`

Pull remote changes (git pull only, no apply).

### `cfgd upgrade`

Check for and install cfgd updates from GitHub releases.

```sh
cfgd upgrade           # download and install latest
cfgd upgrade --check   # check only (exit 0 = current, exit 1 = update available)
```

### `cfgd explain`

Show schema and field documentation for resource types.

```sh
cfgd explain module                        # show Module spec
cfgd explain profile                       # show Profile spec
cfgd explain profile.spec.packages         # show specific field
cfgd explain --recursive machineconfig     # expand all fields
```

Resource types: `module`, `profile`, `cfgdconfig`, `configsource`, `machineconfig`, `configpolicy`, `driftalert`, `teamconfig`.

## Profile Commands

### `cfgd profile list`

List available profiles. Marks the active one.

### `cfgd profile show`

Show the fully resolved profile (all inheritance layers merged).

### `cfgd profile switch <name>`

Switch the active profile in cfgd.yaml. Alias: `cfgd profile use <name>`.

### `cfgd profile create <name>`

Create a new profile. Interactive if no flags provided.

```sh
cfgd profile create work-linux \
  --inherit base \
  --module nvim --module tmux \
  --package apt:build-essential \
  --env EDITOR=vim \
  --alias vim=nvim \
  --file ~/.config/starship.toml \
  --secret secrets/api-key.enc:~/.config/app/key \
  --pre-apply scripts/setup.sh
```

| Flag | Description |
|---|---|
| `--inherit <name>` | Inherit from profile (repeatable) |
| `--module <name>` | Include module (repeatable) |
| `--package <mgr:pkg>` | Add package (repeatable) |
| `--env <key=value>` | Set env var (repeatable) |
| `--alias <name=command>` | Set shell alias (repeatable) |
| `--system <key=value>` | Set system setting (repeatable) |
| `--file <path>` | Manage file (repeatable) |
| `--private-files` | Mark files as private (gitignored) |
| `--secret <source:target>` | Add secret (repeatable) |
| `--pre-apply <path>` | Add pre-apply script (repeatable) |
| `--post-apply <path>` | Add post-apply script (repeatable) |

### `cfgd profile update [name]`

Modify an existing profile. Use `--active` to target the current profile. Prefix a value with `-` to remove it.

```sh
cfgd profile update --active --package brew:jq
cfgd profile update work --module new-tool --module -old-tool
cfgd profile update work --package brew:jq --package -brew:unused --alias vim=nvim --alias -old
```

| Flag | Description |
|---|---|
| `--inherit <name>` | Add/remove inherited profile (prefix with `-` to remove) |
| `--module <name>` | Add/remove module (prefix with `-` to remove) |
| `--package <mgr:pkg>` | Add/remove package (prefix with `-` to remove) |
| `--file <path>` | Add/remove file (prefix with `-` to remove by target) |
| `--env <KEY=VALUE>` | Add/remove env var (prefix with `-` to remove by key) |
| `--alias <name=cmd>` | Add/remove alias (prefix with `-` to remove by name) |
| `--system <key=val>` | Add/remove system setting (prefix with `-` to remove by key) |
| `--secret <src:tgt>` | Add/remove secret (prefix with `-` to remove by target) |
| `--pre-apply <path>` | Add/remove pre-apply script (prefix with `-` to remove) |
| `--post-apply <path>` | Add/remove post-apply script (prefix with `-` to remove) |

### `cfgd profile edit <name>`

Open profile in `$EDITOR` with post-save validation.

### `cfgd profile delete <name>`

Delete a profile. Refuses if it's the active profile or inherited by others.

```sh
cfgd profile delete dev --yes   # skip confirmation
```

## Module Commands

### `cfgd module list`

List all available modules with status (installed, pending, outdated, error).

### `cfgd module show <name>`

Show module details: packages, files, dependencies, resolved managers.

### `cfgd module create <name>`

Create a new local module.

```sh
cfgd module create my-tool \
  --depends node \
  --package neovim \
  --file ~/.config/tool/config.toml \
  --post-apply "tool --setup" \
  --set package.neovim.min-version=0.9 \
  --set package.neovim.prefer=brew,snap,apt
```

| Flag | Description |
|---|---|
| `--description <text>` | Module description |
| `--depends <name>` | Dependency on another module (repeatable) |
| `--package <name>` | Add package (repeatable) |
| `--file <path>` | Import file (repeatable) |
| `--private-files` | Mark files as private |
| `--env <key=value>` | Set env var (repeatable) |
| `--alias <name=command>` | Set shell alias (repeatable) |
| `--post-apply <cmd>` | Post-apply script (repeatable) |
| `--set <key=value>` | Helm-style override (repeatable) |

### `cfgd module update <name>`

Modify a local module. Prefix a value with `-` to remove it.

```sh
cfgd module update nvim --package fd --package -unused
cfgd module update nvim --depends node --env EDITOR=nvim --alias vim=nvim
```

| Flag | Description |
|---|---|
| `--package <name>` | Add/remove package (prefix with `-` to remove) |
| `--file <path>` | Add/remove file (prefix with `-` to remove by target) |
| `--env <KEY=VALUE>` | Add/remove env var (prefix with `-` to remove by key) |
| `--alias <name=cmd>` | Add/remove alias (prefix with `-` to remove by name) |
| `--depends <name>` | Add/remove dependency (prefix with `-` to remove) |
| `--post-apply <cmd>` | Add/remove post-apply script (prefix with `-` to remove) |
| `--set <key=value>` | Helm-style override (repeatable) |
| `--description <text>` | Set description |

### `cfgd module edit <name>`

Open module.yaml in `$EDITOR`.

### `cfgd module delete <name>`

Delete a local module. Any files that were adopted (moved into the module and symlinked back) are automatically restored to their original locations before the module directory is removed.

```sh
cfgd module delete nvim            # restores symlinked files, then deletes modules/nvim/
cfgd module delete nvim -y         # skip confirmation
cfgd module delete nvim --purge    # remove deployed target files instead of restoring them
```

| Flag | Description |
|---|---|
| `--yes`, `-y` | Skip confirmation prompt |
| `--purge` | Remove files deployed by this module to target locations instead of restoring symlinks |

### `cfgd module upgrade <name>`

Upgrade a remote (locked) module to a new version.

```sh
cfgd module upgrade tmux                     # latest available
cfgd module upgrade tmux --ref tmux/v2.0     # specific version
cfgd module upgrade tmux --yes               # skip confirmation
cfgd module upgrade tmux --allow-unsigned    # allow unsigned modules
```

### `cfgd module search <query>`

Search configured registries for modules matching a query.

### `cfgd module registry`

Manage module registries.

```sh
cfgd module registry add https://github.com/cfgd-community/modules.git
cfgd module registry add https://github.com/myorg/modules.git --name myorg
cfgd module registry list
cfgd module registry remove community
cfgd module registry rename community cfgd-community
```

## Source Commands

### `cfgd source add <url>`

Subscribe to a config source.

```sh
cfgd source add git@github.com:acme/dev-config.git \
  --profile acme-backend \
  --priority 500 \
  --accept-recommended \
  --sync-interval 1h
```

### `cfgd source list`

List subscribed sources.

### `cfgd source show <name>`

Show source details, provided profiles, policy breakdown, conflicts.

### `cfgd source remove <name>`

Remove a subscription.

```sh
cfgd source remove acme-corp --keep-all    # keep resources as local
cfgd source remove acme-corp --remove-all  # remove everything
```

### `cfgd source update [name]`

Fetch latest from sources (all or specific).

### `cfgd source override <source> <action> <path> [value]`

Override or reject a source's recommendation.

```sh
cfgd source override acme-corp reject packages.brew.formulae kubectx
cfgd source override acme-corp set env.EDITOR "nvim"
```

### `cfgd source priority <name> [value]`

Set or view source priority.

### `cfgd source replace <old> <new-url>`

Replace one source with another.

### `cfgd source create`

Create a new `cfgd-source.yaml` in the current directory.

### `cfgd source edit`

Open `cfgd-source.yaml` in `$EDITOR`.

## Secret Commands

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt <file>          # encrypt values in place
cfgd secret decrypt <file>          # decrypt to stdout
cfgd secret edit <file>             # decrypt, edit, re-encrypt
```

## Daemon Commands

```sh
cfgd daemon                # run in foreground
cfgd daemon --install      # install as system service
cfgd daemon --status       # check running state
cfgd daemon --uninstall    # remove service
```

## Decision Commands

### `cfgd decide <action> [resource]`

Accept or reject pending source decisions.

```sh
cfgd decide accept packages.brew.k9s       # accept one item
cfgd decide reject packages.brew.stern     # reject one item
cfgd decide accept --source acme-corp      # accept all from source
cfgd decide accept --all                   # accept everything
```

## Other Commands

### `cfgd config show`

Show the current cfgd.yaml configuration.

### `cfgd config edit`

Open cfgd.yaml in `$EDITOR`.

### `cfgd config get <key>`

Get a config value by dotted key path. Outputs raw value to stdout (suitable for scripting).

```sh
cfgd config get profile                      # → work
cfgd config get theme                        # → dracula
cfgd config get theme.name                   # → dracula
cfgd config get daemon.reconcile.interval    # → 5m
cfgd config get file-strategy                # → symlink
cfgd config get aliases.add                  # → profile update --active --file
cfgd config get daemon                       # prints full daemon YAML block
```

### `cfgd config set <key> <value>`

Set a config value by dotted key path. Creates intermediate sections as needed.

```sh
cfgd config set profile personal
cfgd config set theme dracula
cfgd config set theme.name minimal
cfgd config set daemon.reconcile.interval 10m
cfgd config set daemon.enabled true
cfgd config set file-strategy copy
cfgd config set aliases.deploy "apply --yes"
```

### `cfgd config unset <key>`

Remove a config value (resets to default).

```sh
cfgd config unset theme                          # remove entire theme section
cfgd config unset daemon.reconcile.auto-apply    # reset single field
cfgd config unset aliases.deploy                 # remove an alias
```

### `cfgd workflow generate`

Generate GitHub Actions workflows for config repo releases.

```sh
cfgd workflow generate --force   # overwrite existing
```

### `cfgd checkin`

Check in with the device gateway.

```sh
cfgd checkin --server-url https://cfgd.acme.com --api-key <key>
```

### `cfgd enroll`

Enroll with a device gateway using token or key-based verification.

```sh
cfgd enroll --server-url https://cfgd.acme.com --token <bootstrap-token>
cfgd enroll --server-url https://cfgd.acme.com --ssh-key ~/.ssh/id_ed25519
cfgd enroll --server-url https://cfgd.acme.com --gpg-key ABCD1234
```

| Flag | Description |
|---|---|
| `--server-url <url>` | Device gateway URL |
| `--token <token>` | Bootstrap token for token-based enrollment |
| `--ssh-key <path>` | SSH key for key-based enrollment |
| `--gpg-key <id>` | GPG key ID for key-based enrollment |
| `--username <name>` | Username to enroll as (default: current system user) |

#### Enrollment Methods

The server's enrollment method is configured by the administrator. cfgd auto-detects which method the server requires.

| Method | How it works | Best for |
|---|---|---|
| **Token** | Admin generates a short-lived bootstrap token, gives it to the user. User exchanges it for a permanent device credential. | Quick onboarding, automated provisioning |
| **SSH key** | Admin pre-registers the user's SSH public key. User proves possession via challenge-response signing. | Teams already using SSH keys for git access |
| **GPG key** | Admin pre-registers the user's GPG public key. User proves possession via challenge-response signing. | Teams with existing GPG infrastructure |

**Challenge-response flow (SSH/GPG):**

1. cfgd contacts the server and requests a challenge nonce
2. The server generates a random nonce with a 5-minute TTL
3. cfgd signs the nonce with your local key
4. cfgd sends the signature back to the server
5. The server verifies the signature against pre-registered public keys
6. On success, the server returns a permanent device API key

**Key auto-detection:** If neither `--ssh-key` nor `--gpg-key` is specified, cfgd checks the SSH agent first, then falls back to `~/.ssh/id_ed25519`, `~/.ssh/id_rsa`, and `~/.ssh/id_ecdsa` in order. The first available key is used.

### `cfgd completions <shell>`

Generate shell completions.

```sh
cfgd completions bash > ~/.local/share/bash-completion/completions/cfgd
cfgd completions zsh > ~/.zfunc/_cfgd
cfgd completions fish > ~/.config/fish/completions/cfgd.fish
```
