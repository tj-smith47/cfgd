# cfgd — Completed Work

Reference for the original design rationale and acceptance criteria of shipped features. All remaining work is tracked in `PLAN.md`.

---

## Windows Support — Plan 1: Platform Foundations

Cross-platform abstractions making cfgd-core and cfgd compile and work on Windows. Full design in [specs/2026-03-22-windows-support-design.md](specs/2026-03-22-windows-support-design.md).

- [x] Platform abstraction layer in `cfgd-core/src/lib.rs`: `create_symlink`, `file_permissions_mode`, `set_file_permissions`, `is_executable`, `is_same_inode`, `acquire_apply_lock` (LockFileEx), `terminate_process` (TerminateProcess), `is_root` (IsUserAnAdmin), `expand_tilde` (USERPROFILE)
- [x] Daemon IPC: Unix domain sockets / Windows named pipes abstraction with `IpcStream`, `connect_daemon_ipc`, generic `handle_health_connection`
- [x] Script execution: `cmd.exe /C` for inline commands on Windows, extension-based executable check, platform-neutral timeout termination
- [x] Self-upgrade: `.zip` extraction via `zip` crate, rename-dance binary replacement (can't overwrite running exe on Windows), `cleanup_old_binary()` on startup
- [x] Unix-only module gating: `node.rs` behind `#[cfg(unix)]`, daemon service management (launchd/systemd) gated, configurator registration platform-aware
- [x] CI: Windows build/test job in `ci.yml` (fmt, clippy, test for cfgd-core + cfgd)
- [x] Release: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` targets, `.zip` packaging
- [x] Config: info log when file permissions configured on Windows (NTFS uses inherited ACLs)
- [x] Dependencies: `windows-sys` (Win32 APIs), `zip` (archive extraction)

---

## Windows Support — Plan 2: Windows Features

Windows-specific features: package managers, system configurators, PowerShell env, Windows Service daemon, documentation. Full design in [specs/2026-03-22-windows-support-design.md](specs/2026-03-22-windows-support-design.md). Implementation plan at [plans/2026-03-22-windows-support-plan-2-features.md](plans/2026-03-22-windows-support-plan-2-features.md).

- [x] Config schema: `winget`, `chocolatey`, `scoop` fields in `PackagesSpec` + `desired_packages_for_spec` match arms + config merge (`union_extend`) + composition merge
- [x] Package managers: `WingetManager`, `ChocolateyManager`, `ScoopManager` — full `PackageManager` trait implementations with output parsing, `add_package`/`remove_package`, registration in `all_package_managers`
- [x] Module-level package aliases for Windows managers (generic `HashMap<String, String>` already supported; tests confirm)
- [x] Reconciler env: `generate_powershell_env_content` (env vars + aliases), `plan_env`/`verify_env` Windows branching (PowerShell profile injection for PS7 + PS5.1, optional Git Bash env)
- [x] System configurators: `ShellConfigurator` (Windows Terminal default profile), `EnvironmentConfigurator` (registry/setx), `WindowsRegistryConfigurator` (declarative registry settings via `reg` CLI), `WindowsServiceConfigurator` (Windows Services via `sc.exe`)
- [x] Windows daemon: Windows Service lifecycle via `sc.exe` (install/uninstall/start/stop), `windows-service` crate SCM integration, `DaemonCommand::Service` variant, file-based logging to `%LOCALAPPDATA%\cfgd\daemon.log`, graceful shutdown via `rt.shutdown_timeout`
- [x] Documentation: `packages.md`, `system-configurators.md`, `daemon.md`, `configuration.md`, `profiles.md`, `spec/profile.md` updated with Windows fields and examples
- [x] JSON schema (`schema.rs`) and `explain` command (`explain.rs`) updated with all Windows fields
- [x] Profile display (`profile.rs`) and doctor (`mod.rs`) updated with all package managers

---

## Tier 1 — Operator Hardening & CRD Enhancement

### Operator operational readiness
- [x] Leader election via `coordination.k8s.io/v1` Lease — `main.rs` acquires lease before starting controllers; Helm `replicaCount` > 1 enabled
- [x] Graceful shutdown — SIGTERM handler, drain in-flight reconciliations (2s grace), stop webhook, flush metrics
- [x] Health probes on dedicated HTTP port (8081) — `/healthz` liveness, `/readyz` readiness (503 until leader lease acquired)
- [x] Security contexts in Helm deployment template — `runAsNonRoot`, `readOnlyRootFilesystem`, `capabilities.drop: [ALL]`, UID 65532
- [x] PodDisruptionBudget (conditional on `replicaCount >= 2`)
- [x] NetworkPolicy restricting ingress to webhook/gateway/probe ports, egress to kube-apiserver

### CRD enhancements (existing 3 CRDs)
- [x] Printer columns: MachineConfig (`NAME HOSTNAME PROFILE RECONCILED DRIFT AGE`), ConfigPolicy (`NAME COMPLIANT NON-COMPLIANT ENFORCED AGE`), DriftAlert (`NAME DEVICE SEVERITY RESOLVED AGE`)
- [x] Short names: `mc`, `cpol`, `da`. Categories: `cfgd` for all
- [x] MachineConfig conditions split: `Ready` → `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`
- [x] DriftAlert: `status.conditions` array with `Acknowledged`, `Resolved`, `Escalated`
- [x] `observedGeneration` on Condition struct (per KEP-1623)
- [x] CEL validation rules on MachineConfig (non-empty hostname, files have content or source)
- [x] MachineConfig finalizer: `cfgd.io/machine-config-cleanup`
- [x] Owner references: MachineConfig → DriftAlert cascade

### CRD API design fixes
- [x] Typed `PackageRef { name, version }` replaces `Vec<String>` in MachineConfig and ConfigPolicy
- [x] `packageVersions` moved from MachineConfigSpec to MachineConfigStatus
- [x] Removed `ConfigPolicySpec.name`
- [x] Typed `LabelSelector { matchLabels, matchExpressions }` with full expression evaluation
- [x] Typed `MachineConfigReference { name, namespace }` in DriftAlertSpec
- [x] `Vec<ModuleRef>` in ConfigPolicySpec
- [x] Removed `driftDetected: bool` from MachineConfigStatus (expressed as condition)
- [x] `systemSettings: BTreeMap<String, serde_json::Value>`

### ClusterConfigPolicy CRD
- [x] Cluster-scoped CRD with `namespaceSelector`, `security.trustedRegistries`, `security.allowUnsigned`
- [x] Controller with namespace filtering, evaluates MachineConfigs across namespaces
- [x] Merge semantics: packages/modules union, settings/versions cluster-wins

### Kubernetes Events
- [x] MachineConfig controller emits: `Reconciled`, `ReconcileError`, `DriftDetected`, `PolicyViolation`
- [x] ConfigPolicy/ClusterConfigPolicy controllers emit: `Evaluated`, `NonCompliantTargets`
- [x] DriftAlert controller emits: `DriftDetected` (on MC), `DriftResolved`
- [x] Uses `kube::runtime::events::Recorder`, best-effort `.ok()`

### Observability
- [x] Prometheus `/metrics` endpoint on port 8443 — `prometheus-client` crate
- [x] 7 metrics: reconciliations_total, duration_seconds, drift_events, devices_compliant, devices_enrolled_total, webhook_requests, webhook_duration
- [x] ServiceMonitor template (conditional on `metrics.serviceMonitor.enabled`)
- [x] OpenTelemetry tracing via `tracing-opentelemetry`, `OTEL_EXPORTER_OTLP_ENDPOINT` config

### Helm chart restructure
- [x] Unified chart at `chart/cfgd/` (operator + agent)
- [x] Restructured values.yaml with operator/agent/webhook/gateway/security/metrics/probes sections
- [x] `values.schema.json`, `NOTES.txt`, test hook pod, example values files

### Multi-tenancy RBAC
- [x] Helm-templated RBAC examples: platform admin, team lead, team member, module publisher
- [x] Namespace isolation documentation (`docs/multi-tenancy.md`)

### Crossplane testing
- [x] E2E tests: TeamConfig → MachineConfig generation, member add/remove
- [x] CI pipeline for function-cfgd (in release.yml)
- [x] XRD uses `apiextensions.crossplane.io/v2`

### Server-side apply
- [x] Field manager constants: FIELD_MANAGER_OPERATOR (spec), FIELD_MANAGER_STATUS (status)
- [x] Structured merge diff annotations on CRD schemas (conditions, packages, moduleRefs, files, driftDetails)

---

## Idiomatic Naming Audit (from Tier 1)

- [x] Cross-references use `moduleRef`/`configRef` style
- [x] Enum values use PascalCase (`IfNotPresent`, `Always`, `Symlink`)
- [x] CLI flags and config fields align with k8s conventions (camelCase serde, PascalCase enums)
- [x] All CRD field names use camelCase

---

## Phases 1-8 (Original Implementation)

## Phase Overview

```
Phase 1: Foundation ──────────────────────────► Phase 2: Files
    │                                               │
    ├── Project skeleton                            ├── File copy/symlink
    ├── CLI scaffolding (clap)                      ├── Tera templating
    ├── Config parsing (YAML)                       ├── Profile-aware file resolution
    ├── Output/theme system                         ├── Permissions management
    ├── Error types                                 └── File diffing
    └── Profile resolution                              │
                                                        ▼
Phase 4: State & Reconciliation ◄──────────── Phase 3: Packages
    │                                               │
    ├── SQLite state store                          ├── PackageManager trait
    ├── Plan generation                             ├── Homebrew impl
    ├── Diff engine (actual vs desired)             ├── Apt impl
    ├── Apply execution                             ├── Cargo impl
    └── Verify command                              ├── npm impl
                                                    └── pipx/dnf impl
        │
        ▼
Phase 5: Secrets ─────────────────────────────► Phase 6: Daemon
    │                                               │
    ├── SOPS integration (primary)                  ├── File watchers (notify)
    ├── age fallback (opaque files)                 ├── Reconciliation loop
    ├── Secret references (1Password, etc.)         ├── Auto-pull/sync
    ├── Secret templating                           ├── Drift detection & notification
    └── cfgd secret edit workflow                   ├── Health endpoint (unix socket)
                                                    └── systemd/launchd install

Phase 7: Bootstrap                              Phase 8: Server + Workspace Split
    │                                               │
    ├── cfgd init --from <url>                      ├── Cargo workspace conversion
    ├── One-command install script                   ├── cfgd-core library crate
    ├── Interactive profile selection                ├── Self-hosted cfgd-server
    ├── Phased apply with progress                  ├── Device auth, Web UI, fleet
    └── Post-bootstrap verification                 └── k8s operator (CRDs)

Phase 9: Team Config Controller                 Phase 10: cfgd-node
    │                                               │
    ├── Multi-source config                         ├── Node agent binary
    ├── Composition engine                          ├── SystemConfigurator impls:
    ├── Policy tiers (locked/required/etc.)         │   sysctl, kernel modules,
    ├── Subscription management                     │   containerd, kubelet,
    └── Security model                              │   apparmor, seccomp, certs
                                                    ├── DaemonSet deployment
                                                    └── cfgd-server integration

Phase 11: Linux System Pkg Managers              Phase 12: Universal & Platform Pkg Managers
    │                                               │
    ├── apk (Alpine Linux)                          ├── snap (Ubuntu/cross-distro)
    ├── pacman (Arch Linux/Manjaro)                  ├── flatpak (cross-distro GUI apps)
    ├── zypper (openSUSE/SLES)                      ├── nix (cross-platform declarative)
    └── yum (RHEL/CentOS 7)                         ├── pkg (FreeBSD)
                                                    └── go install (Go toolchain)

Phase 13: Self-Update
    │
    ├── cfgd upgrade command
    ├── Version check against GitHub releases
    ├── In-place binary replacement
    └── Daemon restart after upgrade

Phase 14: Release Readiness
    │
    ├── Documentation sweep
    ├── Release infrastructure
    ├── JSON Schema generation
    ├── Shell completions
    └── Final audit
```

---

## Phase 1: Foundation

**Goal**: A working binary that can parse config, resolve profiles, and print styled output.

### Tasks

0. **Module skeleton**
   - Create module files for all modules in the module map: `cli/mod.rs`, `config/mod.rs`, `output/mod.rs`, `errors/mod.rs`, `providers/mod.rs`, `files/mod.rs`, `packages/mod.rs`, `secrets/mod.rs`, `reconciler/mod.rs`, `state/mod.rs`, `daemon/mod.rs`, `sources/mod.rs` (empty, Phase 9), `composition/mod.rs` (empty, Phase 9)
   - Ensures `cargo check` passes from the start

1. **Error types** (`src/errors/`)
   - Define `CfgdError` enum with variants for each domain: Config, File, Package, Secret, State, Daemon, Git
   - Implement `thiserror::Error` for all variants
   - Define `type Result<T> = std::result::Result<T, CfgdError>`

1b. **Provider model preparation** (see `architecture.md` "Provider Model")
   - Define provider traits in `src/providers/mod.rs`: `PackageManager`, `SystemConfigurator`, `FileManager`, `SecretBackend`, `SecretProvider`, and `ProviderRegistry` struct
   - `ProviderRegistry` holds `Vec<Box<dyn PackageManager>>`, `Vec<Box<dyn SystemConfigurator>>`, etc. — the reconciler depends on this, not concrete impls
   - `system:` config parsed as `HashMap<String, serde_yaml::Value>` — each `SystemConfigurator` deserializes its own key
   - Workstation binary assembles registry with workstation providers; future cfgd-node assembles different ones

1c. **Multi-source preparation** (see `.claude/team-config-controller.md` "Changes Required NOW")
   - Add `#[serde(default)] pub sources: Vec<SourceSpec>` to `CfgdConfig` (empty vec = single-source)
   - Design `ResolvedProfile` with layer provenance: `Vec<ProfileLayer>` + `MergedProfile`
   - Each `ProfileLayer` carries `source: String` (defaults to `"local"`)
   - Internally normalize `origin:` as `Vec<OriginSpec>` (primary at index 0)

2. **Output system** (`src/output/`)
   - `Theme` struct: define color palette, icons, border styles
     - Support light/dark terminal detection via `console::colors_enabled()`
     - Default theme ships built-in; custom themes are a future concern
   - `Printer` struct with methods:
     - `header(text)`, `subheader(text)`
     - `success(text)`, `warning(text)`, `error(text)`, `info(text)`
     - `key_value(key, value)` — for status/plan output
     - `diff(old, new)` — colored diff using `similar` crate
     - `syntax_highlight(code, language)` — using `syntect`
     - `progress_bar(total, message)` — returns managed `indicatif::ProgressBar`
     - `multi_progress()` — returns managed `indicatif::MultiProgress`
     - `plan_phase(name, items)` — formatted phase output for `cfgd plan`
     - `table(headers, rows)` — aligned table output
     - `prompt_confirm(message)` — yes/no via `inquire`
     - `prompt_select(message, options)` — selection via `inquire`
   - All `inquire` prompts themed to match `Theme`

3. **Config parsing** (`src/config/`)
   - Root `CfgdConfig` struct matching `cfgd.yaml` schema
   - `Profile` struct matching profile YAML schema
   - `PackageSpec` structs for each package manager
   - `SecretSpec` struct for secret references
   - `SystemSpec` for OS-level settings
   - `ScriptSpec` for pre/post-apply scripts
   - Profile resolution: load base profile, apply inheritance chain, merge
   - Variable interpolation in config values
   - Support both YAML and TOML deserialization
   - Validate config on load (required fields, valid paths, no circular inheritance)

4. **CLI scaffolding** (`src/cli/`)
   - Top-level `Cli` struct with subcommands via `clap::Subcommand`
   - Subcommands (stubs that parse args but don't execute):
     These are intentional stubs — each command is implemented in its designated later phase, satisfying the quality mandate's exception for explicitly planned work.
     - `init`, `plan`, `apply`, `status`, `diff`, `log`
     - `add`, `remove`
     - `sync`, `pull`
     - `daemon` (with `--install`, `--status`)
     - `secret` (with `encrypt`, `decrypt`, `edit`)
     - `profile` (with `list`, `switch`, `show`)
     - `verify`, `doctor`
   - Global flags: `--config <path>`, `--profile <name>`, `--verbose`, `--quiet`, `--no-color`

5. **Main entry point** (`src/main.rs`)
   - Parse CLI args
   - Initialize tracing subscriber (respecting `--verbose`/`--quiet`)
   - Load config
   - Create `Printer` with theme
   - Dispatch to subcommand handlers
   - Top-level `anyhow` error handling with styled error output

### Acceptance Criteria
- `cfgd --help` prints styled help text
- `cfgd plan` loads config, resolves profile, prints "nothing to do" (no state yet)
- `cfgd profile show` loads and prints the resolved profile (all layers merged)
- `cfgd doctor` checks for config file existence, valid YAML, required tools (sops, git)
- All output goes through `Printer` — `audit.sh` passes with zero errors
- `cargo clippy -- -D warnings` passes
- `cargo test` passes with unit tests for config parsing and profile resolution
- GitHub Actions CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` on push

---

## Phase 2: Files

**Goal**: cfgd can manage files — copy from source to target, template them, diff, and track permissions.

### Tasks

1. **File manager** (`src/files/`)
   - `FileManager` struct that takes resolved profile + config
   - Scan source directory (profile layers in order), build merged file tree
   - Later layers override earlier layers (e.g., `work/.zshrc` overrides `base/.zshrc`)
   - For each managed file:
     - If `.tera` extension → render through Tera with profile variables, strip extension
     - Else → copy as-is
   - Set permissions per `files.permissions` config
   - Diff: compare source (rendered) vs target (on disk) using `similar`
   - Operations: `plan()` returns list of `FileAction` (Create, Update, Delete, SetPermissions, Skip)
   - `apply(actions)` executes the plan

2. **Tera template integration**
   - Register profile variables as Tera context
   - Register custom functions: `os()`, `hostname()`, `arch()`, `env(name)`
   - Template error messages include file path and line number

3. **Integration with output**
   - `cfgd plan` shows file changes with diffs
   - `cfgd apply` shows progress bar for file operations
   - `cfgd diff` shows detailed diffs with syntax highlighting
   - `cfgd add <file>` copies file into source directory, updates manifest

4. **Phase 9 prep** (see `team-config-controller.md` "Changes Required NOW")
   - Tag each managed file with `origin_source` (defaults to `"local"`)
   - Tera template context includes an empty `sources` map alongside flat variables

### Acceptance Criteria
- Can define files in a profile, run `cfgd plan`, see what would change
- `cfgd apply` copies/templates files to correct locations with correct permissions
- `cfgd diff` shows colored, syntax-highlighted diffs
- `cfgd add ~/.config/starship.toml` starts tracking the file
- Templates render correctly with profile variables
- File operations are idempotent — running apply twice changes nothing the second time

---

## Phase 3: Packages

**Goal**: Declarative package management across multiple package managers.

### Tasks

1. **PackageManager trait** (`src/packages/`)
   Implement the `PackageManager` trait defined in `architecture.md`.

2. **Implementations**
   - `BrewManager` — formulae + casks + taps (three separate lists in config)
   - `AptManager` — install, with `sudo` handling
   - `CargoManager` — `cargo install`
   - `NpmManager` — `npm install -g`
   - `PipxManager` — `pipx install`
   - `DnfManager` — for Fedora/RHEL

3. **Package reconciler**
   - Diff installed vs desired → produces `PackageAction` (Install, Uninstall, Skip)
   - `cfgd plan` shows package changes grouped by manager
   - `cfgd apply --phase packages` applies only package changes
   - `cfgd add --package brew ripgrep` adds to manifest
   - `cfgd remove --package brew ripgrep` removes from manifest

### Acceptance Criteria
- `cfgd plan` shows packages to install/remove per manager
- `cfgd apply` installs missing packages, optionally removes unlisted ones
- `cfgd status` shows package drift (installed but not in manifest, or vice versa)
- Adding a package to the YAML and running apply installs it
- Package managers that aren't available on the current OS are silently skipped

---

## Phase 4: State & Reconciliation

**Goal**: Persistent state tracking, unified plan/apply across all resource types, verification.

### Tasks

1. **State store** (`src/state/`)
   - SQLite database at `~/.local/share/cfgd/state.db`
   - Tables: `applies` (timestamp, profile, plan_hash, status, summary), `drift_events` (timestamp, resource_type, resource_id, expected, actual, resolved_by), `managed_resources` (resource_type, resource_id, source, last_hash, last_applied) — unified table for all managed resource types (files, packages, secrets, system). The `source` column defaults to `'local'` (Phase 9 prep).
   - Migrations via embedded SQL (no ORM)
   - `StateStore` struct with methods: `record_apply()`, `record_drift()`, `last_apply()`, `history()`, `managed_resources()`

2. **Unified reconciler** (`src/reconciler/`)
   - `Reconciler` takes config, profile, state store
   - `plan()` → `Plan` struct containing `Vec<Phase>`, each phase has `Vec<Action>`
   - Phases ordered: System → Packages → Files → Secrets → Scripts
   - `apply(plan)` executes phases in order with progress reporting
   - Each action is recorded in state store
   - Failed actions don't abort — logged, skipped, reported at end

3. **CLI commands**
   - `cfgd plan` — full plan across all resource types
   - `cfgd apply` — execute plan (with confirmation prompt)
   - `cfgd apply --phase <name>` — execute single phase
   - `cfgd apply --yes` — skip confirmation
   - `cfgd status` — show drift summary
   - `cfgd log` — show apply history from state store
   - `cfgd verify` — check all managed resources match desired state

4. **Workstation SystemConfigurator implementations**
   - `ShellConfigurator` — reads/sets default shell via `chsh`
   - `MacosDefaultsConfigurator` — reads/writes macOS `defaults` domains
   - `SystemdUnitConfigurator` — manages systemd unit files and enablement
   - `LaunchAgentConfigurator` — manages macOS LaunchAgent plists
   - Each implements the `SystemConfigurator` trait from `providers/`
   - Registered in `ProviderRegistry` by `cli/` based on OS detection

### Acceptance Criteria
- `cfgd plan` shows a phased, human-readable plan
- `cfgd apply` executes the plan, records results in SQLite
- `cfgd log` shows history of applies
- `cfgd verify` reports pass/fail per managed resource
- Failed actions are isolated — one failure doesn't abort the entire apply
- Running `cfgd apply` on an already-applied system shows "nothing to do"

---

## Phase 5: Secrets

**Goal**: Encrypt secrets at rest, decrypt on apply, integrate with external secret stores.

### Tasks

1. **SOPS integration (primary backend)** (`src/secrets/`)
   - `SecretBackend` trait with `SopsBackend` and `AgeBackend` implementations
   - SOPS encrypts values within structured YAML/JSON — keys remain visible, diffs are meaningful
   - SOPS backend wraps the `sops` binary, supporting age, AWS KMS, GCP KMS, Azure Key Vault as key sources
   - Generate age identity on `cfgd init` (store at `~/.config/cfgd/age-key.txt`)
   - Auto-generate `.sops.yaml` creation rules pointing to the age key
   - `cfgd secret encrypt <file>` → SOPS-encrypt values in place
   - `cfgd secret decrypt <file>` → plaintext to stdout
   - `cfgd secret edit <file>` → `sops --edit` workflow (decrypt, open `$EDITOR`, re-encrypt on save)
   - Fallback: raw `age` encryption for opaque binary files where SOPS doesn't apply

2. **Secret references (external providers)**
   - Config schema for external refs: `1password://Vault/Item`, `bitwarden://folder/item`
   - `SecretProvider` trait: `resolve(ref) -> Result<String>`
   - 1Password impl: shells out to `op` CLI
   - Bitwarden impl: shells out to `bw` CLI
   - HashiCorp Vault impl: shells out to `vault` CLI or uses HTTP API

3. **Secret templating**
   - Secrets can be used in templates: `${secret:ref}` syntax
   - Resolved at apply time, never written to source directory
   - SOPS-encrypted files are decrypted transparently during file apply phase

4. **cfgd doctor integration**
   - Check for `sops` binary availability and version
   - Validate `.sops.yaml` creation rules
   - Verify age key exists and is readable
   - Test-decrypt a canary file if one exists

### Acceptance Criteria
- `cfgd secret encrypt/decrypt` round-trips correctly via SOPS
- SOPS-encrypted YAML files show keys in plaintext, values encrypted — safe to diff/review
- `cfgd secret edit` opens editor with decrypted content, re-encrypts on save
- Secrets from external providers resolve and inject into templates
- Encrypted files are safe to commit to git
- `cfgd doctor` validates secrets setup
- Missing `sops` binary or providers produce clear error messages with install instructions

---

## Phase 6: Daemon

**Goal**: Long-running daemon that watches for drift, auto-syncs, and sends notifications.

### Tasks

1. **File watcher** (`src/daemon/`)
   - Watch managed files using `notify` crate (inotify/fsevents/kqueue)
   - On change: record drift event, optionally reconcile
   - Debounce rapid changes (500ms window)

2. **Reconciliation loop**
   - Configurable interval (default 5m)
   - Each cycle: fetch desired state from remote, compare with actual system state, converge if configured
   - Modes: `notify` (alert on drift), `auto-fix` (converge to desired state), `ignore` (log only)

3. **Source synchronization**
   - Watch source directory for local changes
   - Local state kept in sync with remote (if `auto-push: true`)
   - Remote state pulled on interval (if `auto-pull: true`)

4. **Notifications**
   - Desktop notifications via `notify-rust` crate
   - Stdout logging (for systemd journal)
   - Webhook support (POST JSON to URL)

5. **Service management**
   - `cfgd daemon` — run foreground
   - `cfgd daemon --install` — install as launchd plist (macOS) or systemd unit (Linux)
   - `cfgd daemon --uninstall` — remove service
   - `cfgd daemon --status` — check if daemon is running, last reconcile time

6. **Health endpoint**
   - Unix domain socket at `/tmp/cfgd.sock`
   - Simple JSON API: `GET /health`, `GET /status`, `GET /drift`
   - `cfgd daemon --status` queries this socket

7. **Phase 9 prep** (see `team-config-controller.md` "Changes Required NOW")
   - Sync loop designed as `Vec<SyncTask>` (single task in Phases 1-7)
   - Health endpoint returns a per-source status list (single entry in Phases 1-7)

### Acceptance Criteria
- Daemon detects file modifications and records drift events
- `cfgd status` shows drift detected by daemon
- Auto-pull fetches remote changes on interval
- Notifications fire on drift detection
- `cfgd daemon --install` creates working launchd/systemd service
- Daemon is resilient — recovers from transient errors without crashing

---

## Phase 7: Bootstrap

**Goal**: One-command setup for a brand new machine.

### Tasks

1. **Install script** — `install.sh`
   - Detect OS/arch, download correct binary from GitHub releases
   - Verify checksum
   - Place binary in PATH

2. **`cfgd init --from <url>`**
   - Clone repo (or fetch from server)
   - Interactive profile selection if multiple profiles exist
   - Run `cfgd plan`, show full plan
   - On confirm, run `cfgd apply`
   - Run `cfgd verify`
   - Optionally install daemon

3. **Day-one UX**
   - Progress through phases with live status
   - Handle missing dependencies gracefully (e.g., no git → install git first)
   - Secrets setup flow: generate age key, prompt for external provider auth
   - Clear summary at end: what succeeded, what needs manual attention

4. **Phase 9 prep** (see `team-config-controller.md` "Changes Required NOW")
   - Reserve `--source` flag on `cfgd init` (accepted but not yet functional)

### Acceptance Criteria
- `curl ... | sh -s -- init --from <url>` works on macOS and Ubuntu (primary targets for Phase 7; node bootstrap in Phase 10)
- Profile selection works interactively
- Full plan is shown before any changes
- Bootstrap is resumable — interrupted bootstrap can be continued
- Post-bootstrap `cfgd verify` passes

---

## Phase 8: Server & Kubernetes Operator

Self-hosted backend replacing git as the remote. Deployed as a k8s operator.

**cfgd-server core:**
- Device auth flow (browser-based, no SSH keys needed on day one)
- Stores config state centrally, serves it to `cfgd daemon` instances
- Web UI: fleet dashboard, drift history, apply logs across all machines
- Webhook notifications for drift events
- REST/gRPC API for machine check-in and config retrieval

**k8s operator (cfgd-operator):**
- Separate crate in workspace: `cfgd-operator/`
- CRDs via `kube-derive`: `MachineConfig`, `ConfigPolicy`, `DriftAlert`
- `MachineConfig` — declares desired state per machine, tracks reconciliation status
- `ConfigPolicy` — enforces baselines across fleet (required packages, SSH key types, disk encryption)
- `DriftAlert` — fires when machines drift from policy
- Shared core logic with CLI (same config parsing, same reconciler types)
- Built with `kube-rs` + `kube::runtime::Controller`

**Workstation-level k8s support (ships in v1 via profiles):**
- kubeconfig management: templated per profile, credentials encrypted via SOPS
- k8s tooling packages: kubectl, helm, kustomize, k9s declared in packages spec
- Helm values files: tracked and templated per environment

### Acceptance Criteria
- Cargo workspace compiles: `cfgd-core`, `cfgd`, `cfgd-server` as separate crates
- cfgd-server starts and exposes REST API for device check-in
- Web UI shows fleet dashboard with at least one registered machine
- `MachineConfig` CRD can be created and reconciled by the operator
- `cfgd daemon` can check in with cfgd-server and receive config updates
- Existing `cfgd` CLI functionality is unaffected by the workspace split

---

*Phases 9+ completed work moved here from `PLAN.md`. Remaining work stays in `PLAN.md`.*

---

## Merge cfgd-node into cfgd (Q11)

One binary, all providers. Config/profile determines which are active.

- [x] Move cfgd-node's 7 system configurators into `crates/cfgd/src/system/node.rs`
- [x] Move server check-in client into cfgd-core
- [x] Add `checkin` command to cfgd CLI
- [x] Register node system configurators in ProviderRegistry
- [x] Consolidate Dockerfiles, delete `Dockerfile.node`
- [x] Update Helm chart, e2e tests, CI workflows
- [x] Delete `crates/cfgd-node/` crate

## Fix UX bugs in shipped code

- [x] `cfgd remove <file>` — implement file removal
- [x] `cfgd diff` — extend to packages and system drift
- [x] `cfgd plan` — show skipped items with reason
- [x] `cfgd doctor` — add package manager availability check
- [x] Daemon status — show "not running" instead of socket error
- [x] cfgd-server: auth, pagination, retention, status enum, retry, signature verification

## System configurators

- [x] All 7 node configurators fully implemented and merged into cfgd

## Package manager bootstrap

- [x] `can_bootstrap()` and `bootstrap()` on PackageManager trait
- [x] Bootstrap per manager (brew, cargo, npm, pipx; apt/dnf pre-installed)
- [x] Reconciler: bootstrap before packages phase
- [x] Linuxbrew-as-root support
- [x] Doctor output for package manager health

## Native manifest support

- [x] `file:` key for Brewfile, apt list, package.json, Cargo.toml
- [x] Merge with inline declarations

## Team config controller

- [x] Operator deployment: Helm chart, admission webhook, version enforcement
- [x] Crossplane: XRD, Composition, composition function
- [x] Server enrollment: bootstrap tokens, device registration, per-device auth
- [x] Team onboarding UX: source detection, platform profiles, policy tiers, wizard
- [x] Auto-apply decisions: pending_decisions table, daemon policy, `cfgd decide`

## Additional package managers

- [x] OS: apk, pacman, zypper, yum, pkg
- [x] Universal: snap, flatpak, nix, go install

## Custom (user-defined) package managers

- [x] ScriptedManager with shell command templates
- [x] Config schema, template substitution, doctor integration

## Module system

- [x] Phase A: Platform detection
- [x] Phase B: Module core (config, loading, dependency resolution, package resolver, git files)
- [x] Phase C: Reconciler integration (ModuleAction, module phase, state, drift)
- [x] Phase C.1: Script packages and platform filtering
- [x] Phase D: CLI integration (list, show, add, remove, apply/plan --module, init --module)
- [x] Phase E: Team source + fleet integration
- [x] Phase F: Remote/shareable modules (lockfile, search, signatures, daemon)

## Self-update

- [x] `cfgd upgrade` with GitHub Releases, SHA256 verification, daemon restart
- [x] `cfgd upgrade --check` for scripting
- [x] Daemon periodic version check with notifications

## `cfgd explain` command

- [x] All KRM types, CRDs, and Crossplane XR documented
- [x] Dot-notation field drilling, recursive expansion
- [x] Shell completions for explain subcommand

## E2E test coverage

- [x] CLI, operator, and full-stack e2e test suites

## CLI restructure

- [x] Collapse `plan` into `apply --dry-run`
- [x] CLI flag parity (secrets, pre/post-reconcile, inherits on update)
- [x] Profile file layout consistency (`profiles/<name>/files/`)
- [x] Alias system (`gh alias`-style, built-in defaults, user overrides)

## File management

- [x] File deployment strategy (Symlink/Copy/Template/Hardlink)
- [x] File source:target mapping (`:` separator)
- [x] Private files (`private: true`, auto-gitignore)
- [x] Conflict detection (same target from multiple sources)
- [x] Backup/restore prompts (adopt/backup/skip for unmanaged files)

## Enrollment key verification

- [x] Server-level enrollment mode (CFGD_ENROLLMENT_METHOD: token | key)
- [x] Admin key management API (SSH/GPG public keys per user)
- [x] Challenge-response enrollment (`cfgd enroll --server <url>`)
- [x] SSH signing via `ssh-keygen -Y sign`, GPG via `gpg --detach-sign`
- [x] Server verification via `ssh-keygen -Y verify` / `gpg --verify`
- [x] Bootstrap token flow kept as fallback

## API group migration

All documents now use `cfgd.io/v1alpha1` — CRDs, local configs, schemas, examples, tests. `cfgd_core::API_VERSION` constant centralizes the string.

- [x] Update CRD definitions in `crates/cfgd-operator/src/crds/mod.rs`: API group `cfgd.io`, version `v1alpha1`
- [x] Update Crossplane XRD and Composition manifests in `manifests/crossplane/`
- [x] Update `function-cfgd` Go code to use `cfgd.io/v1alpha1`
- [x] Update all references in architecture.md, team-config-controller.md, CLAUDE.md
- [x] Regenerate CRD manifests
- [x] Unify local config apiVersion (`cfgd/v1` → `cfgd.io/v1alpha1`): configs, profiles, modules, sources, schemas, examples, e2e tests
- [x] Add `API_VERSION` constant to `cfgd-core/src/lib.rs`, replace all string literals in Rust code

## Signature verification

GPG/SSH commit signature verification on git sources, enforced via `require-signed-commits` constraint in ConfigSource manifests, with `spec.security.allow-unsigned` opt-out.

- [x] GPG/SSH signature verification on git commits and/or manifest files in `sources/mod.rs`
- [x] When a source's constraints include signature requirements, refuse unsigned content
- [x] `spec.security.allowUnsigned: true` opt-out for development/testing

## Merge cfgd-server into cfgd-operator

cfgd-server has no standalone purpose — its value is as a device gateway bridging machines into the Kubernetes control plane. Merge into cfgd-operator as an optional feature.

- [x] Move cfgd-server Axum routes (checkin, enrollment, devices, drift, SSE, admin) into cfgd-operator, served on a separate port from the admission webhook
- [x] Move ServerDb into cfgd-operator; convert from ad-hoc `CREATE TABLE IF NOT EXISTS` to versioned migrations (matching StateStore pattern)
- [x] Move fleet aggregation and web dashboard modules
- [x] Update Helm chart: second port/service for device gateway, optional ingress, `deviceGateway.enabled` toggle
- [x] Update `server_client.rs` if endpoint structure changes
- [x] Delete `crates/cfgd-server/` crate
- [x] Update Cargo workspace, CLAUDE.md module map, architecture.md

## Release readiness

- [x] Update e2e tests for CLI restructure (`module source` → `module registry`, `module-sources` → `modules.registries`, killed `add-to-profile`/`remove-from-profile`, `plan` → `apply --dry-run`)
- [x] Shell completions (bash/zsh/fish via `clap_complete`)
- [x] Documentation sweep: README covers all shipped features, examples for every use case
- [x] JSON Schema generation: committed schemas for cfgd.yaml, profile YAML, cfgd-source.yaml
- [x] CRD manifests committed and auto-generated (depends on API group migration and cfgd-server merge above)
- [x] CLAUDE.md + module map accuracy — verify descriptions match reality (depends on cfgd-server merge above)
- [x] CONTRIBUTING.md updated for final crate layout (depends on cfgd-server merge above)
- [x] Document operational mode taxonomy (Q8) in architecture.md
- [x] No TODO/FIXME/placeholder URLs remain in shipped files

---

## Shell aliases in profiles and modules

Profiles and modules support declaring shell aliases, same pattern as env vars. Module aliases merge into the resolved profile with module-wins-on-conflict semantics. Aliases are written to `~/.cfgd.env` alongside env vars.

- [x] Add `ShellAlias` struct (`name`, `command`) to `config/mod.rs`
- [x] Add `aliases: Vec<ShellAlias>` to `ProfileSpec`, `ModuleSpec`, `MergedProfile`, `ResolvedModule`
- [x] Merge logic: module aliases win on conflict by `name`, same as env
- [x] Write aliases to `~/.cfgd.env` as `alias name="command"` lines
- [x] For fish: write `abbr -a name command` to `~/.config/fish/conf.d/cfgd-env.fish`
- [x] CLI: `--alias name=command` on profile/module create, `--add-alias`/`--remove-alias` on update
- [x] Drift detection: verify `~/.cfgd.env` alias entries match resolved state
- [x] Update docs, schemas, exhaustive test plan

---

## Unified update flags: `--thing` with leading-minus removal

Replaced verbose `--add-thing`/`--remove-thing` flag pairs on `profile update` and `module update` with single `--thing` flags. A value prefixed with `-` is a removal. Old flags removed entirely (no backwards compat).

- [x] `split_add_remove()` shared helper in `cfgd-core/src/lib.rs`
- [x] Unified flags on `ProfileUpdateArgs`: `--inherit`, `--module`, `--package`, `--file`, `--env`, `--alias`, `--system`, `--secret`, `--pre-apply`, `--post-apply`
- [x] Unified flags on `ModuleUpdateArgs`: `--package`, `--file`, `--env`, `--alias`, `--depends`, `--post-apply`
- [x] Updated docs (cli-reference, profiles, modules, configuration, safety, changelog)
- [x] Updated E2E tests and exhaustive test suite
- [x] Updated default CLI aliases in `cfgd init` scaffold

---

## Structured output: `--output json` and `--jsonpath`

Add `--output json` (alias `-o json`) global flag and `--jsonpath <expr>` for machine-readable output. Applies to all commands that display structured data.

Commands supported:
- `status` — drift state, last apply info, managed resources
- `profile show` — resolved profile as JSON
- `module list` / `module show` — module metadata, packages, deps
- `source list` / `source show` — source subscriptions, manifest info
- `log` — apply history entries
- `doctor` — tool availability checks as structured report
- `verify` — compliance state, drift items
- `explain` — schema definitions
- `config get` — already outputs raw values; JSON mode returns typed values
- `config show` — full config as JSON

- [x] Add `--output` global flag (`table` default, `json`, `yaml`) and `--jsonpath` to Cli struct
- [x] Implement `OutputFormat` enum and `Printer::write_structured()` method
- [x] Add `Serialize` to all display structs (ApplyResult, DriftEvent, ModuleInfo, etc.)
- [x] Wire through each command listed above
- [x] Update docs and CLI reference

---

## Source management improvements

Platform-aware source profile auto-selection. Cross-platform sources auto-detect the correct profile via `platformProfiles` in the source manifest when `--profile` is not specified during `cfgd source add`.

- [x] `platform_profiles: HashMap<String, String>` field on `ConfigSourceProvides` (maps platform identifier → profile name)
- [x] `detect_platform()` + `match_platform_profile()` wired into `cmd_source_add` as fallback (exact distro → OS fallback → interactive)
- [x] Source manifest documentation in `docs/sources.md` updated with `platformProfiles` field, matching order, and examples
- [x] Tests: unit tests for match_platform_profile (exact distro, OS fallback, no-match) + SRC24 E2E test

---

## Buffered script output (completed)

`Printer::run_with_output()` implements Docker-style bounded scrolling (5 visible lines, spinner, collapse to summary).

- [x] Capture script stdout/stderr to state store — added `script_output` TEXT column to `apply_journal` (migration 3), pass combined stdout/stderr from `apply_script_action` to `state.journal_complete()`
- [x] `cfgd log --show-output <apply-id>` — display captured script output from journal for a specific apply run

## `module show` enhancements (completed)

- [x] Mask env values by default — show `***` with last 3 chars, `--show-values` flag on `ModuleCommand::Show` reveals full values
- [x] Display "Last applied" timestamp prominently — label changed from "Installed at" to "Last applied"

## Profile-less `status` and `verify` (completed)

Module state is stored by module name, not by profile — so status/verify/apply can work without a profile when targeting a single module.

- [x] `cfgd apply --module <name>` without a profile — falls back to empty `ResolvedProfile` when profile loading fails, skips profile-level env/aliases/system/files
- [x] `cfgd status --module <name>` — reads module definition + `ModuleStateRecord` directly, shows package/file state and deployed file manifest
- [x] `cfgd verify --module <name>` — resolves only the named module + deps, verifies packages installed and files present via `reconciler::verify()`

## Ecosystem integration (completed)

- [x] OPA/Kyverno policy library at `policies/` — Rego policies (trusted registries, signed modules, security baseline) and Kyverno ClusterPolicies. Will need updates when CRD enhancements and Module CRD land (tracked in PLAN.md)
- [x] OLM bundle for OpenShift at `ecosystem/olm/` — CSV, annotations, RBAC. Will need updates for new CRDs
- [x] GitHub Actions action at `ecosystem/github-action/` — composite action: installs cfgd, runs dry-run, posts plan as PR comment
- [x] GitLab CI template at `ecosystem/gitlab/` — `.cfgd-ci.yml` include template with `.cfgd-plan` and `.cfgd-apply` jobs
- [x] Tekton task at `ecosystem/tekton/` — `cfgd-apply` Task with dry-run, output-format, and workspace support
- [x] DevContainer Feature adapter — `cfgd module export --format=devcontainer` generates `install.sh` + `devcontainer-feature.json` from a module

---

## Tier 2 — Module CRD & OCI Foundation

### Module CRD
- [x] `ModuleSpec` struct with kube derive: cluster-scoped, group `cfgd.io/v1alpha1`, short name `mod`, category `cfgd`. Fields: packages, files, scripts, env, depends, ociArtifact, signature
- [x] Supporting types: `PackageEntry`, `ModuleFileSpec`, `ModuleScripts`, `ModuleEnvVar`, `ModuleSignature`/`CosignSignature`. All derive required traits with `#[serde(rename_all = "camelCase")]`
- [x] `ModuleStatus` struct: resolvedArtifact, availablePlatforms, verified, conditions
- [x] Printer columns: Artifact, Verified, Platforms, Age (NAME is implicit)
- [x] `ModuleSpec::validate()`: non-empty package names, non-empty depends, valid OCI reference format, valid PEM public key
- [x] Module controller: watch Module CRDs, validate ociArtifact against trusted registries from ClusterConfigPolicy, set Available/Verified conditions, emit events (Available, Verified, PullFailed, SignatureInvalid, TrustedRegistryViolation, UnsignedNotAllowed)
- [x] MachineConfig controller enhancement: resolve moduleRefs against Module CRDs (cluster-scoped), set ModulesResolved condition (False with missing names, or True/AllResolved)
- [x] `gen_crds.rs` includes Module CRD with SMD annotations; Helm CRD template generated

### Validation webhook enhancements
- [x] `/validate-module` endpoint: structural validation via `ModuleSpec::validate()`, plus ClusterConfigPolicy-based trusted registry enforcement and unsigned module rejection at admission time
- [x] ValidatingWebhookConfiguration rule in Helm chart for Module CRD
- [x] RBAC: Module CRD verbs (get, list, watch, create, update, patch, delete), status, finalizers added to operator ClusterRole
- [x] Unit tests: accept valid Module, reject empty package name, reject malformed OCI reference, reject invalid PEM key, reject untrusted registry, reject unsigned when required

### OCI pipeline Phase A — push/pull
- [x] OCI Distribution Spec client in cfgd-core/src/oci.rs (using ureq, no new crate dependencies). Media type constants defined
- [x] Registry auth: Docker config.json parsing, credential helper programs, REGISTRY_USERNAME/REGISTRY_PASSWORD env vars
- [x] `cfgd module push <dir> --artifact <ref>`: config blob + tar+gzip layer with cfgd.io/platform annotation, push to registry
- [x] `cfgd module pull <ref> --output <dir>`: authenticate, pull manifest + layer, verify digest, extract. `--require-signature` flag
- [x] Push and pull subcommands wired into clap under `cfgd module`
- [x] Unit tests: OCI reference parsing (8), config blob round-trip, tar+gzip round-trip, Docker auth parsing, base64, digest, manifest serialization, Www-Authenticate parsing (24 total)

### OCI pipeline Phase D — CRD sync
- [x] `cfgd module push --apply`: construct Module CRD from module.yaml + ociArtifact ref, server-side apply with field manager `cfgd`
- [x] Kubeconfig discovery: kube::Client::try_default() (in-cluster → KUBECONFIG → ~/.kube/config)

### Implementation notes
- OCI client implemented using `ureq` (sync HTTP) with hand-rolled OCI Distribution Spec client instead of `oci-client` crate — lighter weight, fewer dependencies, sync-friendly
- Signature verification checks PEM key format validity; actual cryptographic verification against OCI artifact signatures deferred to Tier 3 (OCI pipeline Phase C — signing) when `sigstore-rs` is integrated
- Integration tests (webhook policy enforcement against live cluster, push/pull against real registry, push --apply CRD creation) require a running Kubernetes cluster and OCI registry — these are exercised via E2E testing, not unit tests

---

## Tier 3 — OCI Build & Supply Chain Security

### OCI pipeline Phase B — module build
- [x] `cfgd module build --target <platform>`: Docker/Podman container-based builds, Dockerfile generation from module.yaml packages, container create → cp → extract pipeline
- [x] Multi-platform builds: OCI image index manifest (`push_module_multiplatform`), per-platform tagged manifests, comma-separated `--target linux/amd64,linux/arm64`
- [x] Docker/Podman integration: `detect_container_runtime()` auto-detection, `--base-image` flag, build context with generated Dockerfile
- [x] OCI arch name mapping: `rust_arch_to_oci()` converts `x86_64`→`amd64`, `aarch64`→`arm64`; `current_platform()` helper

### OCI pipeline Phase C — signing
- [x] `cfgd module push --sign`: cosign sign at push time, static key via `--key` flag
- [x] Keyless signing via Fulcio + Rekor: `--sign` without `--key`, OIDC identity-based with `--yes` non-interactive
- [x] `cfgd module keys generate`: wraps `cosign generate-key-pair`, outputs to specified directory
- [x] `cfgd module keys list`: checks `./cosign.pub` and `~/.cfgd/cosign.pub`
- [x] `cfgd module keys rotate`: backs up old key pair, generates new, re-signs specified `--artifacts`
- [x] `verify_signature()` with `VerifyOptions` struct: static key or keyless with identity/issuer constraints
- [x] Keyless verification requires at least identity or issuer — rejects wildcard-only (security hardening)

### Supply chain security
- [x] SLSA v1 provenance: `generate_slsa_provenance()` returns in-toto Statement v1 with SLSA Provenance v1 predicate, git source URI + commit
- [x] `cfgd module push --attest`: attaches provenance via `cosign attest`, auto-detects git remote/commit
- [x] `cfgd module pull --verify-attestation`: verifies provenance at pull time via `cosign verify-attestation`
- [x] `attach_attestation()` and `verify_attestation()` with `VerifyOptions` for key/keyless modes

### CRD enhancements (Tier 3)
- [x] `CosignSignature` expanded: optional `publicKey`, `keyless` bool, `certificateIdentity`, `certificateOidcIssuer`
- [x] `ModuleStatus` expanded: `signatureDigest`, `attestations` fields
- [x] Module controller: keyless verification support in `evaluate_module_verification()`
- [x] Webhook `check_unsigned_policy`: accepts keyless mode as valid signing
- [x] OciError expanded: `BuildError`, `SigningError`, `VerificationFailed`, `AttestationError`, `ToolNotFound`

### Implementation notes
- Container builds use Docker/Podman via `std::process::Command` — consistent with cfgd's external tool pattern
- Signing shells out to `cosign` rather than embedding `sigstore-rs` — lighter weight, standard tooling
- `VerifyOptions` struct supports both static key and keyless (identity/issuer regexp) verification
- Keyless verification requires at least identity or issuer constraint — rejects wildcard-only
- OCI arch mapping (`x86_64` → `amd64`, `aarch64` → `arm64`) via `rust_arch_to_oci()`
- `push_module_inner` returns `(digest, manifest_size)` to avoid HEAD requests in multi-platform index
- SLSA provenance follows in-toto Statement v1 / SLSA Provenance v1 predicate format

---

## Tier 4 — Pod Module Injection

### CSI driver (`crates/cfgd-csi/`)
- [x] Separate binary with `tonic` gRPC on unix socket, Identity + Node services
- [x] Identity RPCs: `GetPluginInfo` (`csi.cfgd.io`), `GetPluginCapabilities` (empty, Node-only), `Probe` (checks cache dir)
- [x] Node RPCs: `NodePublishVolume` (bind mount, idempotent), `NodeUnpublishVolume` (unmount + cleanup), `NodeStageVolume` (OCI pull to cache), `NodeUnstageVolume` (no-op)
- [x] `NodeGetCapabilities`: `STAGE_UNSTAGE_VOLUME`; `NodeGetInfo`: hostname as node ID
- [x] LRU cache with `.cfgd-last-access` marker files, atomic population via temp dir + rename
- [x] Path traversal protection via `validate_no_traversal`, completion sentinel for partial extraction safety
- [x] CSI metrics: `cfgd_csi_volume_publish_total` (module, result), `pull_duration_seconds` (module, cached), `cache_size_bytes`, `cache_hits_total` (module)
- [x] Metrics wired into Node service: stage records pull duration/cache hits, publish records volume_publish_total
- [x] Helm DaemonSet with node-driver-registrar + liveness-probe sidecars, CSIDriver object, RBAC
- [x] 41 tests (35 unit + 6 gRPC integration via temp unix socket)

### Pod module mutating webhook
- [x] `POST /mutate-pods` endpoint: parse `cfgd.io/modules` annotation + ConfigPolicy/ClusterConfigPolicy `requiredModules`
- [x] ClusterConfigPolicy filtered by `namespaceSelector` via `matches_selector`
- [x] JSON patches: CSI volumes, volumeMounts, env vars (with append for PATH), init containers for `postApply` scripts
- [x] Ensures volumeMounts/env arrays exist before RFC 6902 append (fixes missing-array patch failure)
- [x] Module name sanitization for Kubernetes RFC 1123 DNS label rules
- [x] MutatingWebhookConfiguration Helm template: `failurePolicy: Ignore`, `reinvocationPolicy: IfNeeded`, namespace label selector
- [x] 11 tests: annotation parsing, volume injection, env vars, init containers, multiple containers

### kubectl cfgd plugin
- [x] `debug`: ephemeral container with CSI volumes, PATH extension, custom PS1
- [x] `exec`: runs command with module PATH via `sh -c` wrapper (proper `$PATH` expansion)
- [x] `inject`: patches workload controller (Deployment/StatefulSet) pod template annotation
- [x] `status`: lists Module CRDs with verification status
- [x] `version`: client + Kubernetes server version
- [x] argv[0] detection (`kubectl-cfgd`), Krew manifest at `manifests/krew/cfgd.yaml`
- [x] 5 tests: module arg parsing, CSI volume/mount spec generation

### Implementation notes
- CSI driver uses `tonic` gRPC on unix socket, `nix` crate for bind mounts (Linux-only, cfg-gated)
- LRU cache uses `.cfgd-last-access` marker files (filesystem atime unreliable with noatime/relatime)
- Atomic cache population via temp dir + rename prevents concurrent corruption
- Mutating webhook filters ClusterConfigPolicy by namespaceSelector before injecting required modules
- JSON patches ensure volumeMounts/env arrays exist before appending (RFC 6902 compliance)
- `kubectl cfgd inject` targets workload controllers (Deployment/StatefulSet) via annotation patch, not running pods
- Pod-level Events (ModuleInjected/ModuleInjectionFailed) deferred — pod has no UID during CREATE admission

### Distribution & publishing
- [x] `Dockerfile.csi`: multi-stage build with protobuf-compiler, `docker-csi` job in release.yml pushes `ghcr.io/tj-smith47/cfgd-csi`
- [x] `docker-agent` job: builds and pushes `ghcr.io/tj-smith47/cfgd` from existing `Dockerfile`
- [x] kubectl-cfgd: release workflow copies cfgd binary as kubectl-cfgd, packages per platform, uploads to GitHub Release
- [x] Krew manifest: `krew-manifest` job computes SHA256 from build artifacts, populates version and checksums dynamically
- [x] Helm chart OCI: `helm-chart` job packages and pushes to `oci://ghcr.io/tj-smith47/charts/cfgd`
- [x] OLM bundle: `bundle.Dockerfile`, `olm-bundle` job builds and pushes bundle image, CSV updated with all 5 CRDs, 6 webhookdefinitions, env vars, volume mounts, RBAC for all resources
- [x] CRDs moved to Helm-native `chart/cfgd/crds/` with kustomize base at `manifests/crds/`
- [x] Homebrew sha256 extraction fixed (specific filenames instead of ambiguous glob)
- [x] values.schema.json property definitions for csiDriver and mutatingWebhook sections

### mountPolicy feature
- [x] `MountPolicy` enum (Always/Debug) on `ModuleSpec` — Debug modules get CSI volume but no volumeMount/env on declared containers
- [x] `debugModules` field on ConfigPolicySpec and ClusterConfigPolicySpec — policy-level debug module staging
- [x] Policy `debugModules` overrides Module CRD's `mountPolicy` during webhook resolution
- [x] `x-kubernetes-list-type`/`list-map-keys` annotations on `debugModules` for strategic merge patch
- [x] CRD YAML regenerated with mountPolicy and debugModules schemas

## E2E test gaps

### Operator E2E (`tests/e2e/operator/`) — T01-T18
- [x] Module CRD: create, verify controller sets status (verified, resolvedArtifact), webhook rejects invalid OCI refs and malformed PEM keys
- [x] ClusterConfigPolicy: create with namespaceSelector, verify only matching namespaces evaluated, verify cluster-wins merge with namespace ConfigPolicy
- [x] Validation webhooks: Module, ClusterConfigPolicy, DriftAlert endpoints reject invalid specs
- [x] Mutating webhook: pod with `cfgd.io/modules` annotation in labeled namespace gets CSI volumes injected, mountPolicy Debug skips volumeMount, env vars set on containers
- [x] OCI supply chain: push module to test registry (kind-hosted), pull with signature verification, verify content integrity
- [x] Update CRD wait loop to include all 5 CRDs (currently only waits for 3)

### Full-stack E2E (`tests/e2e/full-stack/`) — T01-T16
- [x] CSI driver: deploy DaemonSet via Helm, create pod referencing CSI volume, verify module content mounted read-only, verify unmount on pod delete
- [x] kubectl cfgd plugin: `inject deployment/test -m mod:v1` patches annotation, `status` lists modules, `version` returns server version
- [x] Debug flow: pod with mountPolicy Debug module, `kubectl cfgd debug` creates ephemeral container that accesses debug-only volume

---

## YAML convention alignment

Switched all YAML serialization from kebab-case/lowercase to camelCase/PascalCase to match Kubernetes ecosystem conventions. Zero `rename_all = "kebab-case"` or `rename_all = "lowercase"` serde attributes remain.

- [x] All config structs: `rename_all = "camelCase"` (config/mod.rs ~63 sites, server_client.rs, daemon/mod.rs, upgrade.rs, gateway/api.rs, gateway/db.rs)
- [x] All enums: removed `rename_all`, serialize as PascalCase by default (FileStrategy, PolicyAction, OriginType, NotifyMethod, LayerPolicy, DriftSeverity, ApplyStatus, PhaseName)
- [x] Removed explicit `#[serde(rename = "apiVersion")]` from 5 document structs (camelCase handles it naturally)
- [x] Updated all unit test YAML strings, integration test fixtures, example YAML files, Helm chart templates, documentation
- [x] CLAUDE.md style rule updated to reflect camelCase convention

---

## AI-guided configuration generation (`cfgd generate` + MCP server)

Full design in `.claude/specs/2026-03-19-generate-design.md`. Four-layer implementation: core types, tool implementations, embedded CLI client, MCP server.

- [x] `GenerateError` enum and `CfgdError::Generate` variant in cfgd-core errors
- [x] `AiConfig` struct (provider, model, apiKeyEnv) in config/mod.rs, integrated into ConfigSpec
- [x] Core generate module: schema export (`get_schema`), YAML validation (`validate_yaml`), session state (`GenerateSession`), write functions
- [x] Tool implementations in cfgd binary: `scan_installed_packages`, `scan_dotfiles`, `scan_shell_config`, `scan_system_settings`, `inspect_tool`, `query_package_manager`, `read_file`/`list_directory`/`adopt_files` with security model
- [x] `PackageManager` trait extensions: `installed_packages_with_versions()`, `package_aliases()` with default implementations
- [x] Embedded Anthropic API client: `ai/client.rs` (streaming), `ai/tools.rs` (dispatch), `ai/conversation.rs` (state management)
- [x] Orchestration skill embedded as const string in `generate/skill.md`
- [x] MCP server: JSON-RPC stdin/stdout transport, tool/resource/prompt definitions
- [x] CLI: `cfgd generate` (full/module/profile modes) and `cfgd mcp-server` commands
- [x] `docs/ai-generate.md` user guide

---

## Documentation consistency fixes

- [x] `docs/reconciliation.md`: added missing Env phase, completed system configurator list
- [x] `docs/safety.md`: fixed drift_policy to camelCase driftPolicy
- [x] `docs/cli-reference.md`: fixed --server to --server-url for enroll
- [x] `docs/operator.md`: fixed cfgd init --server to cfgd enroll --server-url
- [x] `docs/modules.md`: fixed CLI commands section to match actual implementation
- [x] `README.md`: fixed module add syntax, added safety doc to table
- [x] `docs/bootstrap.md`: fixed module add syntax
- [x] `CLAUDE.md`: added Helm chart paths to module map
- [x] `docs/cli-reference.md` and `docs/modules.md`: fixed module create --name to positional
- [x] `docs/packages.md`, `docs/system-configurators.md`, `docs/templates.md`: added CLI cross-references

---

## CLI UX improvements

11 items covering CLI consistency, new commands, and script lifecycle overhaul.

- [x] Convert `daemon` from flags to subcommands (Run, Install, Uninstall, Status)
- [x] `profile show` accepts optional name argument
- [x] `--yes` flag on `source remove`
- [x] Rich `-o` flag (`OutputFormatArg`) replacing bare String — supports table, wide, json, yaml, name, jsonpath=EXPR, template=TMPL, template-file=PATH
- [x] Normalize `source create --name` to positional
- [x] `ls` aliases on all `list` subcommands
- [x] `--module` flag on `diff`
- [x] `profile update` defaults to active profile (removed `--active`)
- [x] `plan` top-level command with --phase, --skip, --only, --module, --skip-scripts, --context apply|reconcile, structured output
- [x] Structured output for profile list, module search, module registry list, module keys list
- [x] Script lifecycle overhaul: unified ScriptSpec/ScriptEntry with 6 hook types (preApply, postApply, preReconcile, postReconcile, onDrift, onChange), ReconcileContext (Apply/Reconcile), PreScripts/PostScripts phases, unified executor with timeout/continueOnError/onChange detection, environment variable injection, onDrift in daemon drift detection
- [x] Updated user-facing docs for all CLI UX changes
- [x] Renamed `docs/spec-reference/` to `docs/spec/`

---

## CLI UX follow-up fixes

Code fixes identified by code review, dedup analysis, and gap analysis after the CLI UX implementation.

- [x] Extract shared helpers in cli/mod.rs: `display_plan_preview()` and `strip_scripts_from_plan()` — deduplicate 60+ lines between cmd_apply dry-run and cmd_plan
- [x] Delete duplicate `parse_duration_str` tests from reconciler (6 tests duplicating lib.rs)
- [x] Remove `--jsonpath` deprecation warning — nobody uses this tool yet, no deprecation notices needed
- [x] Wire up module-level preApply, preReconcile, postReconcile, onChange scripts (ResolvedModule now carries all hooks; plan_modules selects based on ReconcileContext; onChange runs per-module after apply)
- [x] Implement `-o wide` with distinct behavior — add extra columns to list commands (profile list, module list, module search)
- [x] Implement daemon auto-apply so preReconcile/postReconcile hooks actually execute (daemon calls reconciler.apply() when drift_policy is Auto)
- [x] Make `-o name` work on all structured output types — `name_from_value()` tries name, context, phase, resourceType, url, applyId as fallbacks
- [x] Add `#[serde(rename_all = "camelCase")]` to all pre-existing output structs (16 in cli/mod.rs + 2 in module.rs)
- [x] Fix pre-existing clippy warnings in test code (useless `format!`, redundant binding, `assert_eq!` with literal bool, borrowed expressions, field reassign with default)
- [x] `script`-based package installs (`manager: script`) respect `--skip-scripts` — `strip_scripts_from_plan()` also filters InstallPackages with manager "script"
- [x] `PROFILE_SCRIPT_TIMEOUT` re-export in reconciler/mod.rs removed — use `crate::PROFILE_SCRIPT_TIMEOUT` directly at call sites
- [x] Plan display loop in init.rs and module.rs calls `display_plan_table(plan, printer, None)` instead of duplicating the loop
- [x] `ScriptPhase::display_name()` — deduplicated 4 identical match blocks into one canonical method (found during review)
- [x] `ModuleStatus` struct in `cmd_status_module` — added missing `#[serde(rename_all = "camelCase")]` (found during review)
- [x] `RunScript` display text uses actual phase name instead of hardcoded "post-apply" (found during review)
---

## Ecosystem integration
- [x] Update `policies/` for new CRD fields — OPA and Kyverno policies for ClusterConfigPolicy (allowUnsigned, trustedRegistries, namespaceSelector, Enforced condition), Module keyless cosign verification, MachineConfig conditions (Reconciled, DriftDetected, ModulesResolved, Compliant), DriftAlert conditions (Acknowledged, Resolved, Escalated), drift detail validation, empty config warning
- [x] Update idiomatic naming in ecosystem files — fixed DriftAlert severity `"warning"` → `"Medium"` (valid enum value), added missing `driftDetails` to OLM CSV example, verified camelCase field paths across all policies and ecosystem manifests

---
## Documentation cleanup
- [x] Consolidated duplicate script lifecycle content — replaced duplicated field reference table, env vars table, and defaults in `docs/modules.md` with summary + cross-reference to `docs/spec/module.md#specscripts`; added spec cross-reference to `docs/daemon.md` onDrift section
- [x] Added cross-references from 7 user-facing docs to `docs/spec/` for detailed field documentation (configuration, modules, profiles, operator, team-config, daemon, sources)
- [x] Renamed stale `spec-reference` exclusion in `.claude/scripts/completeness-check.sh` to match the `docs/spec/` directory rename

---
## Linux Desktop Configurators

Three `SystemConfigurator` implementations for Linux desktop environment preferences, achieving parity with macOS `MacosDefaultsConfigurator` and Windows `WindowsRegistryConfigurator`.

- [x] Evaluated scope: per-DE configurators (gsettings covers GNOME/Cinnamon/MATE/Budgie/Pantheon, kwriteconfig covers KDE Plasma, xfconf-query covers XFCE). LXQt/i3/Sway use config files already handled by cfgd file management.
- [x] `GsettingsConfigurator` — GNOME/GTK desktop settings via `gsettings` CLI. Two-level mapping (schema → key → value). Uses `yaml_value_to_native_bool` for true/false booleans. `strip_gsettings_quotes` for reading values.
- [x] `KdeConfigConfigurator` — KDE Plasma settings via `kwriteconfig5`/`kwriteconfig6`. Three-level mapping (file → group → key → value). Prefers v6, falls back to v5.
- [x] `XfconfConfigurator` — XFCE settings via `xfconf-query`. Two-level mapping (channel → property → value). Auto-creates missing properties with `--create -t string` fallback.
- [x] Registration in `build_provider_registry()` under `cfg!(target_os = "linux")`
- [x] Schema entries in `explain.rs`, YAML examples in `schema.rs`, test assertions
- [x] `gsettings_schemas` scanning in `scan_system_settings()` for AI generate
- [x] Windows parity: added `windows_services` and `windows_registry` (well-known paths) scanning to `scan_system_settings()`
- [x] Documentation: `system-configurators.md`, `configuration.md` Linux section, `spec/profile.md` table, `ai-generate.md`, `skill.md`, `generate-design.md`
- [x] CLAUDE.md module map updated
- [x] Doc prose kebab-case fixes (pre-apply → `preApply`, auto-apply → `autoApply`, etc.)
