<div align="center">

<img src=".github/gear.svg" width="96" alt="cfgd gear icon">

# cfgd

Declare your entire machine — packages, dotfiles, system settings, secrets — with composable profiles and shareable, cross-platform modules.

[![AutoTag](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/auto-tag.yml)
[![CI](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml)
[![E2E](https://github.com/tj-smith47/cfgd/actions/workflows/e2e.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/e2e.yml)
[![Release](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml/badge.svg)](https://github.com/tj-smith47/cfgd/actions/workflows/release.yml)
[![Coverage](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/tj-smith47/cfgd/badges/coverage.json)](https://github.com/tj-smith47/cfgd/actions/workflows/ci.yml)
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
- [Distribution](#distribution)

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

**For developers:**
- [One-command bootstrap](docs/bootstrap.md) — `cfgd init --from <repo> --apply` on a new machine, done
- [AI-guided generation](docs/ai-generate.md) — `cfgd generate` scans your system and builds profiles/modules; MCP server for AI editor integration
- [Shareable modules](docs/modules.md) — cross-platform dev environment packages with dependency resolution and registries
- [15 package managers](docs/packages.md) — brew, apt, dnf, pacman, cargo, npm, pipx, snap, and more, with automatic platform-aware resolution
- [Secrets](docs/secrets.md) — SOPS/age encryption + 1Password, Bitwarden, HashiCorp Vault; secret-backed environment variables
- [Tera templates](docs/templates.md) — render dotfiles with variables, OS detection, custom functions
- [Continuous drift detection](docs/daemon.md) — daemon watches for changes, auto-syncs, notifies or auto-corrects

**For platform & infrastructure engineers:**
- [Multi-source config](docs/sources.md) — publish team baselines with policy tiers (locked/required/recommended/optional)
- [Kubernetes operator](docs/operator.md) — CRDs for MachineConfig, ConfigPolicy, DriftAlert; admission webhook; device gateway with fleet dashboard
- [Node configuration](docs/system-configurators.md) — sysctl, kernel modules, containerd, kubelet, AppArmor, seccomp, certificates
- [CSI driver](docs/operator.md) — OCI-based module injection into pods via volumes
- [Crossplane integration](docs/team-config.md) — TeamConfig XR for self-service team environment distribution
- [kubectl plugin](docs/operator.md) — `kubectl cfgd debug/exec/inject/status` for node inspection

**For security & compliance:**
- [Compliance snapshots](docs/spec/config.md#speccompliance) — continuous machine state capture with JSON/YAML export for Vanta, Drata, or custom integrations
- [Key provisioning](docs/system-configurators.md) — declarative SSH key generation, GPG key management, and git signing configuration
- [Encryption enforcement](docs/spec/profile.md) — per-file encryption requirements with SOPS/age backend validation
- [Policy enforcement](docs/sources.md) — locked files, required packages, encryption constraints on target paths
- [Drift remediation](docs/daemon.md) — daemon detects and auto-corrects configuration drift with per-module policies
- [Fleet visibility](docs/operator.md) — device gateway aggregates compliance scores across enrolled machines

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

## Distribution

In addition to publishing binaries to [GitHub Releases](https://github.com/tj-smith47/cfgd/releases) (Linux, macOS, Windows — x86_64 + aarch64), each release also publishes to:

| Channel | Artifact |
|---|---|
| [Homebrew](https://github.com/tj-smith47/homebrew-tap) | `brew install tj-smith47/tap/cfgd` |
| [crates.io](https://crates.io/crates/cfgd) | `cargo install cfgd` |
| [GHCR](https://ghcr.io/tj-smith47) | Docker images: `cfgd`, `cfgd-operator`, `cfgd-csi` |
| [Helm](chart/cfgd/) | `helm install cfgd oci://ghcr.io/tj-smith47/charts/cfgd` |
| [Krew](manifests/krew/) | `kubectl krew install cfgd` — the [kubectl plugin](docs/operator.md) for debugging nodes, exec'ing into agent pods, and inspecting fleet status |
| [OLM](ecosystem/olm/) | Operator bundle for OLM-managed clusters |
| [Crossplane](function-cfgd/) | `function-cfgd` composition function for [team config distribution](docs/team-config.md) |

**CI/CD integrations:**

| Integration | Description |
|---|---|
| [cfgd Setup](ecosystem/github-actions/setup/) | GitHub Action — bootstrap a runner with a module from your config repo |
| [cfgd Plan](ecosystem/github-actions/plan/) | GitHub Action — run `cfgd plan` on PRs, post the diff as a comment |
| [GitLab CI](ecosystem/gitlab/) | Includable `.cfgd-ci.yml` template with `.cfgd-plan` and `.cfgd-apply` jobs |
| [Tekton](ecosystem/tekton/) | `cfgd-apply` Task for Tekton Pipelines |

```yaml
# Example: set up your dev tools on a GitHub Actions runner
- uses: tj-smith47/cfgd/ecosystem/github-action-setup@master
  with:
    source: git@github.com:you/machine-config.git
    module: dev-tools
```

Modules can also be exported as [DevContainer Features](https://containers.dev/implementors/features/) for injection into devcontainers:

```sh
cfgd module export my-tool --format devcontainer
```

**Building from source:**

```sh
git clone https://github.com/tj-smith47/cfgd.git && cd cfgd
cargo build --release
```

## License

MIT
