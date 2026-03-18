<p align="center">
<img src=".github/gear.svg" width="96" alt="cfgd gear icon">
</p>

<h1 align="center">cfgd</h1>

<p align="center">
Declarative, GitOps-inspired machine configuration — bootstrap entire machine profiles or self-contained tool modules. Written in Rust.
</p>

<p align="center">

[![AutoTag](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml)
[![CI](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml)
[![E2E – CLI](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-cli.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-cli.yml)

[![E2E – Full Stack](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-full-stack.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-full-stack.yml)
[![E2E – Node](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-node.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-node.yml)
[![E2E – Operator](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-operator.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e-operator.yml)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Release](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml)

</p>

---

## What is cfgd

Most dotfile managers track files. cfgd manages your entire machine. You declare packages, files, secrets, and system settings in version-controlled YAML. cfgd diffs what you want against what you have, builds a plan, and reconciles — continuously. If something drifts, it's detected and corrected.

Think of it as a personal Terraform for your workstation, but purpose-built for machine config rather than infrastructure.

### How cfgd compares

| | cfgd | [chezmoi](https://chezmoi.io) | [Nix Home Manager](https://nix-community.github.io/home-manager/) | [Ansible](https://docs.ansible.com/) |
|---|---|---|---|---|
| **Focus** | Full machine state | Dotfiles | Dotfiles + packages (Nix) | General automation |
| **Packages** | 15 native managers | None | Nix only | Any (via tasks) |
| **Drift detection** | Continuous (daemon) | Manual | On rebuild | Manual |
| **Cross-platform resolution** | Per-package manager mapping | N/A | Nix-only | Per-task conditionals |
| **Shareable modules** | First-class | Templates only | Flakes | Roles (Galaxy) |
| **Team config** | Policy tiers + Crossplane | N/A | Flake inputs | N/A |
| **Learning curve** | YAML + CLI | Go templates | Nix language | YAML + Jinja2 |

cfgd is a good fit when you want: continuous reconciliation (not just one-shot apply), cross-platform package management without learning Nix, shareable dev environment modules, or team config distribution with policy enforcement.

## Shareable Modules

Self-contained, portable configuration packages. Install someone's complete dev environment — or share your own — in one command.

```sh
# Install a complete neovim setup — binary, plugins, config, env vars, post-install
cfgd module add --url https://github.com/jane/nvim-module

# Or create your own reusable module
cfgd module create --name my-dev-env
```

A module captures everything: packages (cross-platform), config files, and lifecycle scripts.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  depends: [node, python]
  packages:
    - name: neovim
      min-version: "0.9"
      prefer: [brew, snap, apt]
    - name: ripgrep
    - name: fd
      aliases:
        apt: fd-find
        dnf: fd-find
  files:
    - source: config/init.lua
      target: ~/.config/nvim/init.lua
  scripts:
    post-apply:
      - scripts/install-plugins.sh
```

One module. Cross-platform. Shareable via git. Versioned.

## Quick Start

```sh
# Install
curl -fsSL https://raw.githubusercontent.com/tj-smith47/cfgd/master/install.sh | sh

# Bootstrap from an existing config repo
cfgd init --from git@github.com:you/machine-config.git

# Or start fresh
cfgd init
```

## How It Works

**Profiles** declare your machine's desired state — packages, files, system settings. They compose via inheritance, so you can layer `base → work → work-mac`. See [docs/profiles.md](docs/profiles.md).

**Modules** are shareable, self-contained config packages. Install someone else's dev environment or publish your own. Cross-platform package resolution picks the right manager automatically. See [docs/modules.md](docs/modules.md).

**Reconciliation** continuously ensures machines match their declared state. Drift is detected, reported, and optionally auto-corrected. Failed actions don't abort — they're logged and skipped. See [docs/reconciliation.md](docs/reconciliation.md).

## Features

- [15 package managers](docs/packages.md) — brew, apt, cargo, npm, pipx, dnf, pacman, snap, flatpak, nix, apk, zypper, yum, pkg, go (plus custom script-based managers)
- [Tera templates](docs/templates.md) — render dotfiles with variables, OS detection, custom functions
- [Secrets](docs/secrets.md) — SOPS/age encryption + 1Password, Bitwarden, HashiCorp Vault
- [System configurators](docs/system-configurators.md) — shell, macOS defaults, systemd, launchd, sysctl, kubelet, containerd, apparmor, seccomp, certificates
- [Multi-source config](docs/sources.md) — subscribe to team baselines with policy tiers (locked/required/recommended/optional)
- [Daemon](docs/daemon.md) — file watching, drift detection, auto-sync, desktop/webhook notifications
- [Operator](docs/operator.md) — CRD-based machine management, admission webhook, device gateway, DaemonSet node agent
- [Team Config](docs/team-config.md) — Crossplane-powered team config distribution
- [Shell completions](docs/cli-reference.md) — bash, zsh, fish

## Documentation

| Document | Description |
|---|---|
| [Configuration](docs/configuration.md) | Root config (cfgd.yaml), file strategies, aliases, themes |
| [Profiles](docs/profiles.md) | Profile YAML, inheritance, merge rules, variables |
| [Modules](docs/modules.md) | Module spec, cross-platform packages, dependencies, git file sources, registries |
| [Templates](docs/templates.md) | Tera template system, context variables, custom functions |
| [Secrets](docs/secrets.md) | SOPS/age backends, 1Password, Bitwarden, Vault |
| [Packages](docs/packages.md) | All package managers, skip behavior, dry-run |
| [System Configurators](docs/system-configurators.md) | Shell, macOS defaults, systemd, sysctl, kubelet, and more |
| [Sources](docs/sources.md) | Multi-source config, policy tiers, composition, subscriptions |
| [Daemon](docs/daemon.md) | File watching, reconciliation loop, sync, notifications, service install |
| [Reconciliation](docs/reconciliation.md) | Phase ordering, failure handling, state store |
| [Operator](docs/operator.md) | CRD-based machine management, device gateway, DaemonSet node agent |
| [Team Config](docs/team-config.md) | Crossplane-powered team config distribution |
| [CLI Reference](docs/cli-reference.md) | Complete command reference with flags and examples |
| [Bootstrap](docs/bootstrap.md) | `cfgd init` flow, apply options, install script |

## Shell Completions

```sh
cfgd completions bash > ~/.local/share/bash-completion/completions/cfgd
cfgd completions zsh > ~/.zfunc/_cfgd
cfgd completions fish > ~/.config/fish/completions/cfgd.fish
```

## Why cfgd exists

I switched jobs and spent a day setting up a new machine, forgetting half my config and gradually rediscovering things I'd lost over the next few weeks. I wanted to clone a repo and have my entire workstation — packages, dotfiles, system settings — just be there.

The other thing was devcontainers. I use neovim, and I wanted my full editor setup available in any ephemeral container without having to modify the devcontainer config in team repositories to accommodate my personal preferences. I needed something that could bootstrap my config into any environment from the outside, regardless of which repo I was working in.

cfgd started as a solution to those two problems and grew from there.

## Building from Source

```sh
git clone https://github.com/tj-smith47/cfgd.git && cd cfgd
cargo build --release
# binary at target/release/cfgd
```

## License

MIT
