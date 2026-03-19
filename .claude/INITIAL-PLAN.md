# cfgd Initial Implementation Plan (Historical)

Phases 1-8 are complete. This file is kept as a reference for the original design rationale and acceptance criteria of shipped features. All remaining work is tracked in `PLAN.md`.

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

## Buffered script output (completed)

`Printer::run_with_output()` implements Docker-style bounded scrolling (5 visible lines, spinner, collapse to summary).

- [x] Capture script stdout/stderr to state store — added `script_output` TEXT column to `apply_journal` (migration 3), pass combined stdout/stderr from `apply_script_action` to `state.journal_complete()`
- [x] `cfgd log --show-output <apply-id>` — display captured script output from journal for a specific apply run

## `module show` enhancements (completed)

- [x] Mask env values by default — show `***` with last 3 chars, `--show-values` flag on `ModuleCommand::Show` reveals full values
- [x] Display "Last applied" timestamp prominently — label changed from "Installed at" to "Last applied"
