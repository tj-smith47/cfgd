<div align="center">

<img src=".github/gear.svg" width="96" alt="cfgd gear icon">

# cfgd

Declare your entire machine — packages, dotfiles, system settings, secrets — with composable profiles and shareable, cross-platform modules.

[![CI](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml)
[![E2E – CLI](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-cli.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-cli.yml)
[![E2E – k8s](https://github.com/tj-smith47/cfgd/actions/workflows/e2e.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e.yml)

[![AutoTag](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml)
[![Release](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

</div>

> **Status:** Alpha — APIs may change.

---

- [What is cfgd](#what-is-cfgd)
- [How It Works](#how-it-works)
- [Quick Start](#quick-start)
- [Why cfgd exists](#why-cfgd-exists)
- [Shareable Modules](#shareable-modules)
- [How cfgd compares](#how-cfgd-compares)
- [Features](#features)
- [Documentation](#documentation)
- [Building from Source](#building-from-source)

---

## What is cfgd

Most dotfile managers track files. `cfgd` enables you to manage your entire machine. You declare packages, files, secrets, and system settings in version-controlled YAML. `cfgd` diffs what you want against what you have, builds a plan, and reconciles — continuously. If something drifts, it's detected and corrected.

## How It Works

**Profiles** declare your machine's desired state — packages, files, system settings. They compose via inheritance — share a common base across machines, then specialize per context. See [docs/profiles.md](docs/profiles.md).

```
             base
            ╱    ╲
        work    personal
       ╱    ╲
  laptop    devcontainer
```

**Modules** are shareable, self-contained config packages. Install someone else's dev environment or publish your own. Cross-platform package resolution picks the right manager automatically. See [docs/modules.md](docs/modules.md).

**Reconciliation** continuously ensures machines match their declared state. Drift is detected, reported, and optionally auto-corrected. Failed actions don't abort — they're logged and skipped. See [docs/reconciliation.md](docs/reconciliation.md).

## Quick Start

```sh
# Install via Homebrew
brew install tj-smith47/tap/cfgd

# Or via install script
curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh

# Or via cargo
cargo install cfgd

# Bring your config to a new machine in seconds
cfgd init --from git@github.com:you/machine-config.git

# Or start fresh
cfgd init

# Or let AI scan your system and generate config for you
export ANTHROPIC_API_KEY=sk-...
cfgd generate

# Set up shell completions (add to your shell's rc file)
source <(cfgd completions bash)  # .bashrc
source <(cfgd completions zsh)   # .zshrc
cfgd completions fish | source   # config.fish
```

## Why cfgd exists

I recently switched jobs, and spent the last week of my old job backing up scripts and dotfiles, parsing out company specific info, and composing a tarball to transfer. At the new job, I spent another few days getting my new machine reconfigured. Over time, I gradually discovering things I'd forgotten, as well as some things (e.g., System Settings) that I thought would have been nice to have included in the backup. This all felt very manual and incomplete, and I thought there needed to be a better way; I should just be able to clone a repo and have my entire workstation — packages, scripts, dotfiles, system settings - feel familiar again. And even better, to keep aspects of that feeling in sync betweeen my home and work laptops (parts of it, at least).

Another inspiring aspect had to do with working in devcontainers. At my previous company I had set up custom scripts to inject dotfiles into the devcontainer so a user could replicate their dev environment inside the container once they shell in. At minimum, I wanted my full neovim editor setup available in any ephemeral container without having to modify the devcontainer config in every team's repository I worked in just to accommodate my setup. I needed something that could bootstrap my config into any environment from the outside, regardless of which / whose repo I was working in. Plus, I had some coworkers in need of education about the superiority of vim-motions, and wanted a quick and easy way to share my exact setup, down to the alias.

`cfgd` was architected by a platform / infrastructure engineer, and borrows from the best ideas across practices:

- **Kubernetes** — declarative reconciliation loop, KRM resource model
- **Terraform** — plan/apply workflow, state tracking, drift detection
- **Puppet** — continuous enforcement via daemon, module ecosystem
- **Nix** — reproducible machine state from a single source of truth
- **Ansible** — YAML-driven config management, idempotent task execution
- **Kustomize** — layered overrides and patches
- **chezmoi** — dotfile management

## Shareable Modules

This is my favorite feature; a single, packaged, works anywhere in no time at all config file for a tool.

```sh
cfgd module create my-dev-env
cfgd profile update --module community/nvim
```

A module declares packages with cross-platform resolution, config files, shell env's and aliases, and lifecycle scripts:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  description: Neovim editor configuration
spec:
  depends: [node, python]
  packages:
    - name: neovim
      minVersion: "0.10"
      prefer: [brew, snap]
      deny: [apt]
    - name: ripgrep
    - name: fd
      aliases:
        apt: fd-find
        dnf: fd-find
    - name: gcc
      aliases:
        apt: build-essential
        dnf: "@development-tools"
      platforms: [linux]
  files:
    - source: files/init.lua
      target: ~/.config/nvim/init.lua
    - source: files/lua
      target: ~/.config/nvim/lua
  env:
    - name: EDITOR
      value: nvim
  aliases:
    - name: v
      command: nvim
  scripts:
    postApply:
      - nvim --headless '+Lazy! sync' '+MasonToolsInstallSync' +qa
```

See [docs/modules.md](docs/modules.md) for the full spec including git file sources, registries, and dependency resolution.

## How cfgd compares

| | **cfgd** | [chezmoi](https://chezmoi.io) | [Nix Home Manager](https://nix-community.github.io/home-manager/) | [Ansible](https://docs.ansible.com/) | [Puppet](https://www.puppet.com/) |
|---|---|---|---|---|---|
| **Focus** | Full machine state | Dotfiles | Dotfiles + packages (Nix) | General automation | Server/infra state |
| **Packages** | **15 managers** | None | Nix only | Any (via tasks) | Any (via providers) |
| **Drift detection** | **Continuous (daemon)** | Manual | On rebuild | Manual | Continuous (agent) |
| **Cross-platform resolution** | **Per-package manager mapping** | N/A | Nix-only | Per-task conditionals | Per-OS Hiera data |
| **Shareable modules** | **First-class** | Templates only | Flakes | Roles (Galaxy) | Forge (server-oriented) |
| **Team config** | **Policy tiers + Crossplane** | N/A | Flake inputs | N/A | Puppet Enterprise |
| **Infrastructure** | **Single binary, zero servers** | Single binary | Nix daemon | SSH (or AWX) | PuppetServer + PuppetDB + CA |
| **Learning curve** | YAML + CLI | Go templates | Nix language | YAML + Jinja2 | Puppet DSL (Ruby) |

Puppet is the closest philosophical match — declarative state, continuous enforcement, module ecosystem. If that model clicked for you but standing up a JVM server and writing a Ruby-era DSL to manage your dotfiles in 2026 doesn't, `cfgd` is what that idea looks like rebuilt from scratch for developer workstations.

`cfgd` is a good fit when you want: one-liners for cross-platform machine bootstrapping, shareable dev environment modules, continuous reconciliation between machines or subscribed sources, or team config distribution with policy enforcement.

## Features

**For individuals:**
- [AI-guided generation](docs/ai-generate.md) — `cfgd generate` scans your system and builds profiles/modules interactively; MCP server for AI editor integration
- [Daemon](docs/daemon.md) — file watching, drift detection, auto-sync, desktop/webhook notifications
- [Secrets](docs/secrets.md) — SOPS/age encryption + 1Password, Bitwarden, HashiCorp Vault
- [System configurators](docs/system-configurators.md) — shell, macOS defaults, systemd, launchd, sysctl, kubelet, containerd, apparmor, seccomp, certificates
- [Tera templates](docs/templates.md) — render dotfiles with variables, OS detection, custom functions
- [15 package managers](docs/packages.md) — brew, apt, dnf, yum, pacman, apk, zypper, pkg, cargo, npm, pipx, snap, flatpak, nix, go (plus custom script-based managers)

**For teams and fleet:**
- [Multi-source config](docs/sources.md) — subscribe to team baselines with policy tiers (locked/required/recommended/optional)
- [Operator](docs/operator.md) — CRD-based machine management, admission webhook, device gateway, DaemonSet node agent
- [Team Config](docs/team-config.md) — Crossplane-powered team config distribution

## Documentation

| Document | Description |
|---|---|
| [Configuration](docs/configuration.md) | Root config (cfgd.yaml), file strategies, aliases, themes |
| [Profiles](docs/profiles.md) | Profile YAML, inheritance, merge rules, variables |
| [Modules](docs/modules.md) | Module spec, cross-platform packages, dependencies, git file sources, registries |
| [Reconciliation](docs/reconciliation.md) | Phase ordering, failure handling, state store |
| [Packages](docs/packages.md) | All package managers, skip behavior, dry-run |
| [Templates](docs/templates.md) | Tera template system, context variables, custom functions |
| [Secrets](docs/secrets.md) | SOPS/age backends, 1Password, Bitwarden, Vault |
| [System Configurators](docs/system-configurators.md) | Shell, macOS defaults, systemd, sysctl, kubelet, and more |
| [Sources](docs/sources.md) | Multi-source config, policy tiers, composition, subscriptions |
| [Daemon](docs/daemon.md) | File watching, reconciliation loop, sync, notifications, service install |
| [Operator](docs/operator.md) | CRD-based machine management, device gateway, DaemonSet node agent |
| [Team Config](docs/team-config.md) | Crossplane-powered team config distribution |
| [Safety](docs/safety.md) | Atomic writes, backups, rollback, apply locking, path safety |
| [CLI Reference](docs/cli-reference.md) | Complete command reference with flags and examples |
| [Bootstrap](docs/bootstrap.md) | `cfgd init` flow, apply options, install script |
| [AI Generate](docs/ai-generate.md) | AI-guided config generation, MCP server setup |

## Building from Source

```sh
git clone https://github.com/tj-smith47/cfgd.git && cd cfgd
cargo build --release
# binary at target/release/cfgd
```

## License

MIT
