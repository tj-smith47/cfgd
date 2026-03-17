# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-15

Initial release.

### Core
- Declarative machine configuration with YAML profiles and inheritance
- Continuous reconciliation: desired state diffing, plan generation, phased apply
- SQLite state store for apply history, drift events, and managed resources
- Daemon with file watching, reconciliation loop, auto-sync, and desktop/webhook notifications
- Bootstrap flow: `cfgd init --from <url>` for one-command machine setup
- Self-update via GitHub releases with SHA256 verification

### Modules
- Self-contained, portable configuration packages (`kind: Module`)
- Cross-platform package resolution with per-manager aliases and version constraints
- Dependency resolution (topological sort, cycle detection)
- Git file sources with tag/branch pinning and subdirectory support
- Module registries for discovering and sharing modules
- Lockfile (`modules.lock`) for reproducible remote module installations
- Shell aliases in profiles and modules — `alias name="command"` in bash/zsh, `abbr -a` in fish

### Package Managers
- 15 built-in managers: brew (formulae/casks/taps), apt, dnf, yum, pacman, apk, zypper, pkg, cargo, npm, pipx, snap, flatpak, nix, go
- Custom script-based package managers
- Automatic bootstrapping for managers that can self-install
- Version querying for module resolution

### Files
- File deployment strategies: symlink (default), copy, template, hardlink
- Tera template rendering with profile variables, OS/arch detection, custom functions
- Source:target mapping, private files (auto-gitignored), conflict detection

### Secrets
- SOPS encryption (structured YAML/JSON — keys visible, values encrypted)
- age encryption (opaque file encryption)
- External providers: 1Password, Bitwarden, HashiCorp Vault
- Secret references in templates via `${secret:ref}` syntax

### System Configurators
- Workstation: shell, macOS defaults, launch agents, systemd units, environment
- Node: sysctl, kernel modules, containerd, kubelet, apparmor, seccomp, certificates

### Multi-Source Config
- Subscribe to team config sources with policy tiers (locked/required/recommended/optional)
- Composition engine merges multiple sources with priority-based conflict resolution
- Security sandboxing: path constraints, variable isolation, script opt-in
- Auto-apply decision handling with `cfgd decide` command
- GPG/SSH signature verification on git sources

### Kubernetes
- cfgd-operator with CRDs: MachineConfig, ConfigPolicy, DriftAlert
- Admission webhook for CRD validation
- Device gateway: checkin API, bootstrap token and SSH/GPG key enrollment, fleet dashboard, SSE streaming
- DaemonSet mode for cluster node configuration
- Crossplane TeamConfig XRD and composition function (`function-cfgd`) for team config distribution

### CLI
- Commands: init, apply, status, diff, log, verify, doctor, sync, pull, upgrade, explain, checkin, enroll
- Profile management: list, show, switch, create, update, edit, delete
- Module management: list, show, create, update, edit, delete, upgrade, search, registry
- Source management: add, list, show, remove, update, override, priority, replace, create, edit
- Secret management: init, encrypt, decrypt, edit
- Shell completions for bash, zsh, fish
- Custom command aliases
- `--alias` flag on profile and module create/update (prefix value with `-` to remove)
- `cfgd explain` with dot-notation field drilling for all resource types
