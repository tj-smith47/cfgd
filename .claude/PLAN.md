# cfgd — Remaining Work

Single source of truth for all incomplete work. Items are in dependency order — earlier sections unblock later ones. Check items off as completed.

Design detail for team config / Crossplane / onboarding / auto-apply lives in `team-config-controller.md`. Module system design lives in `modules-design.md`. Package manager bootstrap design detail is inline below. This file is the task list.

---

## Standards

These apply to all work below.

- **Testing**: Unit tests co-located in `#[cfg(test)] mod tests {}`. Integration tests in `tests/`. Mock trait impls for package managers and system configurators. `tempfile` for filesystem tests. In-memory SQLite for state tests.
- **CI**: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` on every push.
- **Release**: Cross-compile for linux-x86_64, linux-aarch64, darwin-x86_64, darwin-aarch64. Install script points to GitHub releases.

---

## Formatting

- [x] Run `cargo fmt` and commit — 384 lines of drift, blocks CI

## Merge cfgd-node into cfgd (Q11)

One binary, all providers. Config/profile determines which are active. Simplifies the project and gives workstation users access to system configurators (sysctl, kernel modules, etc.) that were locked in a separate binary.

- [x] Move cfgd-node's 7 implemented system configurators (Sysctl, KernelModule, Containerd, Kubelet, AppArmor, Seccomp, Certificate) into `crates/cfgd/src/system/node.rs`
- [x] Move cfgd-node's server check-in client into cfgd-core (`server_client.rs`)
- [x] Add `checkin` command to cfgd CLI
- [x] Register node system configurators in cfgd's ProviderRegistry (active based on platform/config)
- [x] Consolidate Dockerfiles: update cfgd's Dockerfile with node runtime deps (kmod, apparmor-utils, procps), delete `Dockerfile.node`
- [x] Update Helm chart — moved to `charts/cfgd/`, all `cfgd-node` refs → `cfgd`
- [x] Update e2e-node tests to use `cfgd` binary
- [x] Delete `crates/cfgd-node/` crate
- [x] Update CI workflows to drop cfgd-node build targets

## Fix UX bugs in shipped code

Broken or misleading behavior in existing features. Not new functionality.

- [x] `cfgd remove <file>` prints "not yet implemented" — implement file removal from config + cleanup
- [x] `cfgd diff` only shows file diffs — extend to show package and system drift
- [x] `cfgd plan` silently filters Skip actions (`format_plan_items` returns `None`) — show skipped items with reason
- [x] `cfgd doctor` doesn't check package manager availability — add declared-vs-available check
- [x] Daemon status gives raw socket connection error when not running — show "daemon is not running"
- [x] cfgd-server: web UI and SSE endpoints bypass `CFGD_API_KEY` auth — apply same auth middleware
- [x] cfgd-server: no warning when `CFGD_API_KEY` is unset — log a loud startup warning
- [x] cfgd-server: `/devices` and `/events` return all records — add pagination (limit/offset)
- [x] cfgd-server: drift_events and checkin_events grow unbounded — add retention policy (configurable max age, default 90 days)
- [x] cfgd-server: device status is a raw string — make it a proper enum in the API contract
- [x] cfgd-server: DriftAlert CRD creation is fire-and-forget — add retry with backoff
- [x] Server check-in receives desired config but discards it — apply on receipt or queue for next reconcile
- [x] Server client has no retry/backoff on network failures — add exponential backoff
- [x] Source signature verification TODO in `sources/mod.rs` — implement or remove the comment and log a warning when fetching unsigned sources

## ~~Finish stub system configurators~~

All 7 node configurators (Sysctl, KernelModule, Containerd, Kubelet, AppArmor, Seccomp, Certificate) were already fully implemented. Merged into cfgd. Node EnvironmentConfigurator dropped — cfgd's cross-platform EnvironmentConfigurator covers both workstation and node.

- [x] KubeletConfigurator — already fully implemented
- [x] AppArmorConfigurator — already fully implemented
- [x] SeccompConfigurator — already fully implemented
- [x] CertificateConfigurator — already fully implemented
- [x] Node EnvironmentConfigurator — dropped; cfgd's cross-platform version covers Linux nodes

## Package manager bootstrap

Make cfgd install its own package managers instead of silently skipping. Depends on: nothing (existing code).

- [x] Add `can_bootstrap()` and `bootstrap()` to PackageManager trait
- [x] Implement bootstrap per manager (see strategies below)
- [x] Reconciler: before packages phase, check availability, bootstrap if possible, error if still unavailable after bootstrap (never silently skip)
- [x] Linuxbrew-as-root (see below)
- [x] Fix plan visibility: `format_plan_items()` in `cfgd-core/reconciler/mod.rs` filters Skip actions to `None` — show them with reason instead
- [x] Add package manager health section to `cfgd doctor`

### Trait change

```rust
pub trait PackageManager: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn can_bootstrap(&self) -> bool;
    fn bootstrap(&self, printer: &Printer) -> Result<()>;
    fn installed_packages(&self) -> Result<HashSet<String>>;
    fn install(&self, packages: &[String], printer: &Printer) -> Result<()>;
    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()>;
    fn update(&self, printer: &Printer) -> Result<()>;
}
```

### Bootstrap strategies per manager

| Manager | Bootstrap method | Notes |
|---|---|---|
| brew (macOS) | Official install script (`/bin/bash -c "$(curl -fsSL ...)"`) | Prompts for sudo |
| brew (Linux) | Create `linuxbrew` user if root, install as that user | See linuxbrew section |
| apt | `can_bootstrap() = false` — pre-installed on Debian/Ubuntu | |
| dnf | `can_bootstrap() = false` — pre-installed on Fedora/RHEL | |
| cargo | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y` | |
| npm | Install via available system manager (brew/apt/dnf) or nvm | Falls back to nvm |
| pipx | Install via available system manager or `pip install --user pipx` | |

### Reconciler flow change

Before the packages phase:
1. For each manager with declared packages, check `is_available()`
2. If unavailable and `can_bootstrap()`: inform user, confirm (unless `--yes`), run `bootstrap()`, re-check — error if still unavailable
3. If unavailable and `!can_bootstrap()`: show in plan as "unavailable — cannot auto-install on this platform"

### Linuxbrew-as-root

Homebrew refuses to run as root on Linux. Solution:

1. Create `linuxbrew` system user: `useradd --system --create-home --shell /bin/bash linuxbrew`
2. Install as that user: `sudo -u linuxbrew /bin/bash -c "$(curl -fsSL ...install.sh)"`
3. All brew commands: `sudo -u linuxbrew brew install ...`
4. `is_available()` checks PATH first, then falls back to `/home/linuxbrew/.linuxbrew/bin/brew`
5. PATH handling: cfgd uses full path internally. For user's shell, surface as post-bootstrap step: `eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"`

### Doctor output

```
Package Managers:
  Declared in config:
    ✓ brew          /opt/homebrew/bin/brew (4.2.0)
    ✗ cargo         not found — can auto-bootstrap via rustup
    ✓ npm           /usr/local/bin/npm (10.2.0)
  Not declared:
    - apt           available but not used in config
```

### Plan output with bootstrap

```
Package Manager Bootstrap:
  + Install Rust/Cargo             via rustup

Packages:
  ⚠ cargo: will be auto-bootstrapped via rustup (2 packages: bat, fd)
  + brew install ripgrep fd bat
```

## Native manifest support

Let users point cfgd at their existing Brewfile, apt package list, etc. Depends on: package manager bootstrap.

- [x] Add `file:` key to package manager config sections
- [x] Brew: delegate to `brew bundle --file=<path>`, merge with inline declarations
- [x] Other managers: parse file format (one-per-line for apt, JSON for npm, TOML for cargo), union with inline

### Config example

```yaml
spec:
  packages:
    brew:
      file: Brewfile                  # path relative to config repo root
      formulae: [extra-tool]          # coexists — merged with Brewfile
    apt:
      file: packages.apt.txt         # one package per line
    npm:
      file: package.json              # reads dependencies + devDependencies
      global: [extra-global-tool]
    cargo:
      file: Cargo.toml                # reads [dependencies] section
```

How it works:
- `brew file:` → `brew bundle --file=<path>` (native Brewfile support)
- `apt file:` → read lines, pass to `apt install`
- `npm file:` → `npm install` in directory containing package.json
- `cargo file:` → parse TOML, `cargo install` each dependency
- `file:` packages merge with inline — union, no conflict
- cfgd tracks file content hash for drift detection

## Team config controller

All design detail, schemas, XRDs, enrollment flow, and implementation specs in `team-config-controller.md`. The checklist below tracks completion; the doc has the full context needed to implement each item.

### Operator deployment (unblocks Crossplane; depends on: nothing)

- [x] Helm chart at `crates/cfgd-operator/chart/cfgd-operator/`: deployment, serviceaccount, RBAC, CRD manifests
- [x] Admission webhook for MachineConfig/ConfigPolicy spec validation
- [x] ConfigPolicy version enforcement (require minimum package versions, not just presence)

### Crossplane integration (depends on: operator Helm chart)

- [x] XRD: `TeamConfig` at `manifests/crossplane/xrd-teamconfig.yaml`
- [x] Composition: `teamconfig-to-machineconfigs` at `manifests/crossplane/composition.yaml`
- [x] Composition function `function-cfgd/`: Go module using `function-sdk-go`

### Server enrollment + per-device auth (depends on: Crossplane)

- [x] Bootstrap token generation + validation endpoint
- [x] Token-to-user mapping (TeamConfig member lookup)
- [x] Device registration: exchange bootstrap token for permanent credential
- [x] Per-device auth replacing shared `CFGD_API_KEY`
- [x] `cfgd init --server <url> --token <bootstrap-token>` client-side enrollment

### Team onboarding UX (depends on: server enrollment)

- [x] `cfgd init --from` with source detection: detect `cfgd-source.yaml`, enter source-aware bootstrap
- [x] Platform auto-detection: OS/distro → match `platform-profiles` in source manifest
- [x] Policy tier review during init: show required, prompt for recommended/optional
- [x] Interactive config creation wizard for `cfgd init` without `--from`
- [x] Git remote setup during `cfgd init`: offer `gh repo create` or manual URL
- [x] Pre-bootstrap diagnostics: doctor-style checks before plan/apply
- [x] Conflict preview during `cfgd source add`

### Auto-apply decision handling (depends on: team onboarding working end-to-end)

- [x] `pending_decisions` + `source_config_hashes` tables in state.db
- [x] Daemon policy config (`new-recommended`, `new-optional`, `locked-conflict`)
- [x] Daemon reconcile loop: diff previous vs current merge, apply tier policy
- [x] `cfgd decide accept|reject` CLI command
- [x] Pending decisions shown in `cfgd status` and `cfgd plan`
- [x] Notification on new pending decisions (once per decision, not per tick)

## Additional package managers

Independent of other work. Each follows the PackageManager trait pattern. Note: once Module system Phase A lands, the trait gains `available_version()` — new managers must implement it too.

### OS system managers
- [x] ApkManager — Alpine (`apk add/del`, `apk list --installed`)
- [x] PacmanManager — Arch/Manjaro (`pacman -S/-R/-Qq`)
- [x] ZypperManager — openSUSE/SLES (`zypper install/remove`, `zypper se --installed-only`)
- [x] YumManager — RHEL 7/CentOS 7 (`yum install/remove/list`; skip when `dnf` is present)
- [x] PkgManager — FreeBSD (`pkg install/remove/info`)

### Universal/language managers
- [x] SnapManager — Ubuntu (`snap install/remove/list`, always use `--classic` when needed)
- [x] FlatpakManager — Fedora/cross-distro (`flatpak install/uninstall/list`, full reverse-DNS app IDs)
- [x] NixManager — cross-platform (`nix-env` and `nix profile` support)
- [x] GoInstallManager — Go toolchain (`go install`, track via `$GOPATH/bin` scanning)

## Custom (user-defined) package managers

Allow users to define their own package managers via config, with shell commands for each operation. Covers internal tools, niche package managers, or anything with a CLI that cfgd doesn't ship a built-in for. Independent of other work.

- [x] Config schema: `custom` key under `packages` — list of entries with `name`, `check`, `list-installed`, `install`, `uninstall`, `update` (all shell command templates)
- [x] `ScriptedManager` struct implementing `PackageManager` trait — shells out user-provided commands, parses stdout line-per-package for `installed_packages()`
- [x] Command template substitution: `{packages}` (space-joined batch) and `{package}` (one-at-a-time loop)
- [x] `all_package_managers()` appends `ScriptedManager` instances from config's `custom` entries
- [x] `add_package()` / `remove_package()` support for custom managers
- [x] `cfgd doctor` shows custom managers with their `check` status
- [x] Tests: scripted manager with mock commands, template substitution, config parsing

### Config example

```yaml
spec:
  packages:
    custom:
      - name: mise
        check: "which mise"
        list-installed: "mise list --installed --plain"
        install: "mise install {packages}"
        uninstall: "mise uninstall {packages}"
        update: "mise upgrade"
        packages: [node, python, ruby]
      - name: my-internal-tool
        check: "which corp-pkg"
        list-installed: "corp-pkg inventory"
        install: "corp-pkg fetch {package}"
        uninstall: "corp-pkg purge {package}"
        packages: [vpn-client, corp-proxy]
```

## Module system

Self-contained, portable, platform-agnostic configuration units. Full design in `modules-design.md` — covers module spec, package resolution, platform detection, reconciler integration, CRD/XRD changes, and remote modules.

### Platform detection (Phase A — leaf, no module dependencies)

- [x] `cfgd_core::platform` module: `Platform` struct with OS, distro, arch; `detect()` function; `native_manager()` mapping
- [x] Move `parse_loose_version()` and `version_satisfies()` from `crds/mod.rs` to `cfgd_core::lib.rs` as shared utilities
- [x] Add `available_version(&self, package: &str) -> Result<Option<String>>` to `PackageManager` trait
- [x] Implement `available_version()` for all existing managers (brew, apt, dnf, cargo, npm, pipx, apk, pacman, zypper, yum, pkg, snap, flatpak, nix, go, brew-cask, brew-tap, custom)
- [x] Tests: platform detection, version query per manager

### Module core (Phase B — depends on platform detection)

- [x] Config structs in `config/`: `ModuleDocument`, `ModulePackageEntry` (with `name`, `min-version`, `prefer`, `aliases`), `ModuleFileEntry` (with git URL support), `modules` field on `ProfileSpec`
- [x] Module loading: parse `modules/<name>/module.yaml` from config dir
- [x] Dependency resolution: topological sort with cycle detection (Kahn's algorithm)
- [x] Package resolver: walk `prefer` list → check `available_version()` → alias lookup → fall back to platform native → error with available options if no match
- [x] Git file source support: URL parsing (`https://` / `git@`, `@tag`, `?ref=branch`, `//subdir`), clone/fetch to `~/.cache/cfgd/modules/`, resolve to local path
- [x] `ModuleError` enum in `errors/` (6 variants: NotFound, DependencyCycle, MissingDependency, UnresolvablePackage, GitFetchFailed, InvalidSpec)
- [x] Tests: 35 tests — resolution with mocked managers, dependency graphs, git URL parsing, file resolution, full end-to-end

### Reconciler integration (Phase C — depends on module core)

- [x] `ModuleAction` variant in reconciler `Action` enum — first-class phase, not flattened to ProfileSpec
- [x] Module phase in reconciler: resolve → plan → apply (runs before profile-level packages/files)
- [x] `module_state` table in state.db (module name, resolved packages hash, file hashes, last apply timestamp)
- [x] Module-aware plan output: grouped by module with deps, packages, files, scripts
- [x] Module-aware drift detection: per-module status in drift events
- [x] Tests: full resolve → plan → apply cycle with mock managers

### Script packages and platform filtering (Phase C.1 — depends on reconciler integration)

- [x] `script` field on `ModulePackageEntry`: inline string or file path to a shell script that installs the package. Acts as a custom "manager" — when `prefer` includes `"script"`, the resolver treats it as always-available and emits `ResolvedPackage { manager: "script", .. }`.
- [x] `platforms` field on `ModulePackageEntry`: optional `Vec<String>` filter. If non-empty, the resolver skips the entry on platforms that don't match. Values match against OS (`linux`, `macos`, `freebsd`), distro (`ubuntu`, `debian`, `fedora`, `arch`, etc.), or arch (`x86_64`, `aarch64`). Empty/absent means all platforms.
- [x] Resolver changes: `resolve_packages()` checks `platforms` against current `Platform` before resolution. When `prefer` includes `"script"` and it's selected, resolves to `manager: "script"` with the script content/path stored on `ResolvedPackage`.
- [x] Reconciler changes: `apply_module_action()` handles `manager: "script"` — runs the inline script or executes the script file via `sh -c`.
- [x] Tests: script resolution (inline + file path), platform filtering (skip on wrong OS, include on match, empty = all), script execution in apply

### CLI integration (Phase D — depends on reconciler integration)

- [x] `cfgd module list` — show available modules and their status
- [x] `cfgd module show <name>` — show module details: packages, files, deps, resolved managers
- [x] `cfgd module add <name>` — add a module to the active profile
- [x] `cfgd module remove <name>` — remove a module from the active profile
- [x] `cfgd apply --module <name>` — apply only the specified module and its dependencies
- [x] `cfgd plan --module <name>` — show plan for only the specified module
- [x] `cfgd init --from <repo> --module <name>` — clone, find module, resolve deps, detect platform, apply
- [x] Module sections in `cfgd status` and `cfgd doctor`
- [x] Profile `modules` field support in config loading

### Team source + fleet integration (Phase E — depends on CLI integration)

- [ ] `modules` field in `cfgd-source.yaml` provides + policy tiers
- [ ] Module policy tiers in composition engine (required/recommended/optional modules)
- [ ] `moduleRefs` field on MachineConfig CRD
- [ ] `requiredModules` field on ConfigPolicy CRD
- [ ] Operator controller: module compliance checking
- [ ] `function-cfgd` (Go): generate `moduleRefs` and `requiredModules` from TeamConfig XR
- [ ] TeamConfig XRD: add `modules`, `policy.requiredModules`, `policy.recommendedModules` to schema

### Remote/shareable modules (Phase F — depends on CLI integration, independent of Phase E)

- [ ] Module lockfile format (`modules.lock`)
- [ ] `cfgd module add <url>` — fetch remote module, review contents, confirm, write lockfile entry
- [ ] `cfgd module update <name>` — diff old vs new, confirm, update lock
- [ ] Signature verification: pinned refs mandatory, content hash in lockfile, optional GPG/cosign signatures
- [ ] Registry index + `cfgd module search <query>`

## Self-update

- [x] `cfgd upgrade`: query GitHub Releases API, compare against compiled-in version, download correct OS/arch binary, verify SHA256, atomic rename, restart daemon if running
- [x] `cfgd upgrade --check`: exit 0 if current, exit 1 if update available (scriptable)
- [x] Daemon periodic version check (24h cache): desktop notification when update available, visible in `cfgd daemon --status`

## `cfgd explain` command

Like `kubectl explain` — show documentation and schema for all cfgd resource types, including CRDs. Makes the system self-documenting.

- [ ] `cfgd explain` with no args — list all explainable resource types (Module, Profile, CfgdConfig, MachineConfig, ConfigPolicy, DriftAlert, TeamConfig, cfgd-source)
- [ ] `cfgd explain <resource>` — show schema and field docs for a resource type (e.g., `cfgd explain module`, `cfgd explain machineconfig`)
- [ ] `cfgd explain <resource>.<field>` — drill into a specific field (e.g., `cfgd explain module.spec.packages`, `cfgd explain configpolicy.spec.rules`)
- [ ] `cfgd explain <resource> --recursive` — show all fields expanded
- [ ] Cover all KRM types: Module, Profile, CfgdConfig (cfgd.yaml), cfgd-source.yaml
- [ ] Cover all CRDs: MachineConfig, ConfigPolicy, DriftAlert
- [ ] Cover Crossplane XR: TeamConfig
- [ ] Shell completions for `explain` subcommand: autocomplete resource names and dot-notation field paths (bash/zsh/fish)
- [ ] Schema data embedded at compile time — derived from serde struct definitions or committed JSON Schema files

## E2E test coverage (Q10)

Test the things built above. Depends on: whatever it's testing being done.

- [ ] `e2e-cli.yml`: init, plan, apply, verify cycle; source add/list/update flow; daemon start/stop; profile inheritance; secrets round-trip; skip/only flags
- [ ] `e2e-operator.yml`: CRD install, controller reconciliation, policy compliance, DriftAlert lifecycle
- [ ] `e2e-full-stack.yml`: cfgd CLI → server → operator → CRDs full loop; multi-device fleet; drift propagation end-to-end

## Release readiness

Last. No new features — completeness and polish only.

- [ ] Shell completions (bash/zsh/fish via `clap_complete`)
- [ ] Documentation sweep: README covers all shipped features, examples for every use case
- [ ] JSON Schema generation: committed schemas for cfgd.yaml, profile YAML, cfgd-source.yaml
- [ ] CRD manifests committed and auto-generated
- [ ] CLAUDE.md + module map accuracy — verify descriptions match reality
- [ ] CONTRIBUTING.md updated for final crate layout
- [ ] Document operational mode taxonomy (Q8) in architecture.md
- [ ] No TODO/FIXME/placeholder URLs remain in shipped files
