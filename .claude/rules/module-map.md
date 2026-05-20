---
paths: ["**/*.rs"]
---
# cfgd Module Map

```
crates/
├── cfgd-core/src/          # Core library crate
│   ├── config/             # YAML config loading, profile resolution, layer merging
│   ├── output/             # CENTRALIZED theming, styled output, progress, syntax highlighting
│   ├── errors/             # Error types (thiserror), result aliases
│   ├── providers/          # Provider traits + ProviderRegistry
│   ├── reconciler/         # Diff engine: actual vs desired state, plan generation
│   ├── state/              # SQLite state store: history, drift events, apply log
│   ├── daemon/             # File watchers, reconciliation loop, sync, notifications
│   ├── modules/            # Module loading, dependency resolution, package resolution, git file sources
│   ├── platform/           # OS/distro/arch detection, native package manager mapping
│   ├── sources/            # Multi-source config management (Phase 9)
│   ├── composition/        # Multi-source merge engine (Phase 9)
│   ├── generate/           # AI-guided generation: schema export, YAML validation, session state
│   ├── server_client.rs    # Device gateway HTTP client (checkin, enrollment, device flow)
│   └── upgrade.rs          # Self-upgrade: GitHub release detection, download, checksum verify
├── cfgd/src/               # Unified binary crate (workstation + node)
│   ├── main.rs             # Entry point, clap dispatch, kubectl plugin argv[0] detection
│   ├── cli/                # Clap command definitions, argument parsing
│   │   └── plugin.rs       # kubectl cfgd plugin: debug, exec, inject, status, version
│   ├── files/              # File management: copy, template, diff, permissions
│   ├── packages/           # PackageManager implementations (brew, apt, cargo, npm, pipx, dnf, winget, chocolatey, scoop)
│   ├── secrets/            # SOPS/age backends, 1Password/Bitwarden/Vault providers
│   ├── system/             # SystemConfigurator trait impls — workstation (shell, macosDefaults, systemd, launchd, gsettings, kdeConfig, xfconf, environment, windowsRegistry, windowsServices) + node (sysctl, kernelModules, containerd, kubelet, apparmor, seccomp, certificates)
│   ├── generate/           # AI generate tools: system scanning, tool inspection, file access
│   ├── ai/                 # Anthropic API client, tool dispatch, conversation management
│   └── mcp/                # MCP server: JSON-RPC transport, tool/resource/prompt definitions
├── cfgd-operator/src/      # k8s operator binary crate
│   ├── main.rs             # Operator entry point (controllers + optional gateway)
│   ├── lib.rs              # Crate root, module declarations
│   ├── crds/               # CRD definitions (MachineConfig, ConfigPolicy, DriftAlert, ClusterConfigPolicy)
│   ├── controllers/        # kube-rs reconciliation controllers (4 controllers)
│   ├── webhook.rs          # Admission webhook server (TLS, 4 validation + 1 mutation endpoints)
│   ├── health.rs           # Dedicated health probe server (/healthz, /readyz)
│   ├── leader.rs           # Lease-based leader election
│   ├── metrics.rs          # Prometheus metrics registry + HTTP endpoint
│   ├── gen_crds.rs         # CRD JSON schema generation utility
│   ├── errors.rs           # Operator-specific error types
│   └── gateway/            # Device gateway (optional, enabled via DEVICE_GATEWAY_ENABLED)
│       ├── mod.rs          # Gateway setup, Axum router assembly
│       ├── api.rs          # REST API: checkin, enrollment, devices, drift, admin, SSE
│       ├── db.rs           # SQLite: devices, credentials, tokens, challenges, events
│       ├── fleet.rs        # Fleet status aggregation
│       ├── web.rs          # Web dashboard (HTML/CSS/JS)
│       └── errors.rs       # GatewayError with IntoResponse
└── cfgd-csi/src/           # CSI Node plugin binary crate
    ├── main.rs             # Entry point: gRPC server on unix socket, metrics HTTP
    ├── lib.rs              # Crate root, proto include
    ├── identity.rs         # CSI Identity service (GetPluginInfo, Probe)
    ├── node.rs             # CSI Node service (Publish/Unpublish/Stage/Unstage)
    ├── cache.rs            # LRU module cache with atomic population
    ├── metrics.rs          # Prometheus CSI metrics
    └── errors.rs           # CsiError enum
chart/
└── cfgd/                   # Unified Helm chart (operator + agent + CSI driver)
```
