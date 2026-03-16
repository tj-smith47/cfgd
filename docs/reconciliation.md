# Reconciliation Model

cfgd follows the same pattern as Kubernetes controllers: declare desired state, diff against actual state, generate a plan, apply it, watch for drift. You never tell cfgd "install ripgrep" — you declare "ripgrep should be installed" and cfgd figures out what needs to change.

## Phases

Apply runs in a fixed phase order:

1. **Modules** — resolve module dependencies, install module packages, deploy module files
2. **System** — shell, macOS defaults, launch agents, systemd units, sysctl, kernel-modules, containerd, kubelet
3. **Packages** — install/uninstall across all package managers (profile-level packages)
4. **Files** — copy, template, set permissions (profile-level files)
5. **Secrets** — decrypt SOPS files, resolve external provider references
6. **Scripts** — run pre/post-reconcile scripts

Each phase can be applied independently with `cfgd apply --phase <name>`.

## Plan Output

`cfgd apply --dry-run` shows the full plan before any changes:

```
Modules:
  nvim (depends: node, python)
    + neovim — snap install nvim (0.10.2, min: 0.9)
    + ripgrep — apt install ripgrep
    → deploy: ~/.config/nvim/ (12 files)
    → post-apply: nvim --headless "+Lazy! sync" +qa

Packages:
  + brew install extra-tool
  = apt: 3 packages up to date

Files:
  ~ ~/.gitconfig (modified)
  = 4 files up to date

System:
  ~ macos-defaults: com.apple.dock.autohide: false → true
```

## Filtering

```sh
cfgd apply --phase packages           # single phase
cfgd apply --module nvim              # single module + deps
cfgd apply --only packages.brew       # dot-notation filter
cfgd apply --skip system.sysctl       # skip specific items
```

## Failure Handling

Failed actions within a phase don't abort the entire apply. They're logged, skipped, and reported at the end. A broken Homebrew tap won't prevent your SSH config from being placed.

## State Store

cfgd tracks state in a SQLite database at `~/.local/share/cfgd/state.db`. This is what lets cfgd detect drift, show history, and know what it's responsible for.

**What cfgd tracks:**

| Category | What's stored | Used for |
|---|---|---|
| **Apply history** | Timestamp, profile, status (success/partial/failed), summary | `cfgd log`, rollback context |
| **Drift events** | What changed, expected vs actual value, whether it was resolved | `cfgd status`, daemon notifications |
| **Managed resources** | Every file, package, and setting cfgd is responsible for | Knowing what to diff on next reconcile |
| **Module state** | Per-module install time, package/file hashes, git source commits | Detecting when a module is outdated |
| **Source tracking** | Per-source fetch time, commit, version, sync status | Multi-source sync and conflict history |
| **Pending decisions** | Unresolved recommended/optional items from source updates | `cfgd decide`, daemon policy |

## Provenance Tracking

When using [multi-source config](sources.md), every action carries an `origin` field ("local" or source name) so the plan output shows where each change comes from:

```
  + brew install git-secrets  <- acme-corp (required)
  ~ EDITOR = "nvim"           <- local (overrides acme-corp)
```
