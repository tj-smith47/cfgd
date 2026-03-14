# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Declarative machine configuration with YAML profiles
- Package management across 6 managers: brew (formulae, casks, taps), apt, cargo, npm, pipx, dnf
- File management with Tera templating, permissions, and diff support
- SOPS and age secret encryption backends
- External secret providers: 1Password, Bitwarden, HashiCorp Vault
- System configurators: shell, macOS defaults, launch agents, systemd units
- SQLite state store for apply history, drift events, and managed resources
- Daemon with file watching, reconciliation loop, auto-sync, and desktop notifications
- Bootstrap flow: `cfgd init --from <url>` for one-command machine setup
- cfgd-server: fleet dashboard web UI, REST API, device check-in
- cfgd-operator: Kubernetes CRDs (MachineConfig, ConfigPolicy, DriftAlert)
- Cargo workspace: cfgd-core, cfgd, cfgd-server, cfgd-operator
