# cfgd

Declarative, GitOps-style machine configuration state management. Written in Rust.

A suite of products sharing a core reconciliation engine:
- **cfgd** — unified machine config management CLI + daemon; workstation + k8s node providers
- **cfgd-operator** — k8s operator + optional device gateway (fleet control plane, web UI, enrollment)

Each binary operates in two modes:
- `cfgd <command>` — CLI for direct interaction (apply, add, status, plan, checkin)
- `cfgd daemon` — long-running process that watches for drift, auto-syncs, reconciles

## Architecture

See `docs/` for user-facing documentation. See `.claude/PLAN.md` for the phased implementation plan.

## Module Map

```
crates/
├── cfgd-core/src/          # Core library crate
│   ├── config/             # YAML config loading, profile resolution, layer merging
│   ├── output/             # CENTRALIZED theming, styled output, progress, syntax highlighting
│   ├── errors/             # Error types (thiserror), result aliases
│   ├── providers/          # Provider traits + ProviderRegistry
│   ├── reconciler/         # Diff engine: actual state vs desired state, plan generation
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
│   ├── system/             # All SystemConfigurators — workstation (shell, macosDefaults, systemd, launchd, gsettings, kdeConfig, xfconf, environment, windowsRegistry, windowsServices) + node (sysctl, kernelModules, containerd, kubelet, apparmor, seccomp, certificates)
│   ├── generate/           # AI generate tools: system scanning, tool inspection, file access
│   ├── ai/                 # Anthropic API client, tool dispatch, conversation management
│   └── mcp/                # MCP server: JSON-RPC transport, tool/resource/prompt definitions
└── cfgd-operator/src/      # k8s operator binary crate
    ├── main.rs             # Operator entry point (controllers + optional gateway)
    ├── lib.rs              # Crate root, module declarations
    ├── crds/               # CRD definitions (MachineConfig, ConfigPolicy, DriftAlert, ClusterConfigPolicy)
    ├── controllers/        # kube-rs reconciliation controllers (4 controllers)
    ├── webhook.rs          # Admission webhook server (TLS, 4 validation + 1 mutation endpoints)
    ├── health.rs           # Dedicated health probe server (/healthz, /readyz)
    ├── leader.rs           # Lease-based leader election
    ├── metrics.rs          # Prometheus metrics registry + HTTP endpoint
    ├── gen_crds.rs         # CRD JSON schema generation utility
    ├── errors.rs           # Operator-specific error types
    └── gateway/            # Device gateway (optional, enabled via DEVICE_GATEWAY_ENABLED)
        ├── mod.rs          # Gateway setup, Axum router assembly
        ├── api.rs          # REST API: checkin, enrollment, devices, drift, admin, SSE
        ├── db.rs           # SQLite: devices, credentials, tokens, challenges, events
        ├── fleet.rs        # Fleet status aggregation
        ├── web.rs          # Web dashboard (HTML/CSS/JS)
        └── errors.rs       # GatewayError with IntoResponse
├── cfgd-csi/src/           # CSI Node plugin binary crate
│   ├── main.rs             # Entry point: gRPC server on unix socket, metrics HTTP
│   ├── lib.rs              # Crate root, proto include
│   ├── identity.rs         # CSI Identity service (GetPluginInfo, Probe)
│   ├── node.rs             # CSI Node service (Publish/Unpublish/Stage/Unstage)
│   ├── cache.rs            # LRU module cache with atomic population
│   ├── metrics.rs          # Prometheus CSI metrics
│   └── errors.rs           # CsiError enum
chart/
└── cfgd/                   # Unified Helm chart (operator + agent + CSI driver)
```

See `.claude/kubernetes-first-class.md` for the Kubernetes ecosystem design (CRDs, controllers, webhooks, CSI, OCI, observability, multi-tenancy, Crossplane).

## Quality Mandate

**Quality over speed.** Every module must be production-grade when committed. Do not leave stubs, `todo!()`, placeholder implementations, or partial features unless the full implementation is explicitly planned for a later phase in PLAN.md. If a phase's acceptance criteria aren't fully met, the phase isn't done — keep working or report what remains.

## Coding Standards

### Hard Rules — Violations Must Be Fixed Immediately

1. **ALL terminal output goes through `output::`**. No module may use `println!`, `eprintln!`, `console::*`, or `indicatif::*` directly. The `output` module owns all interaction with the terminal. This is the single most important architectural constraint. It enables consistent theming, syntax highlighting, and future TUI migration.

2. **No `unwrap()` or `expect()` in library code**. Use `?` with proper error types. `unwrap()` is permitted only in tests and in `main.rs` for top-level setup where failure means "crash immediately."

3. **All providers implement their respective traits** (`PackageManager`, `SystemConfigurator`, `FileManager`, `SecretBackend`). No ad-hoc shelling out. Every provider gets a struct that implements the trait. The reconciler depends on `ProviderRegistry`, never on concrete implementations.

4. **Errors use `thiserror` for library errors, `anyhow` only at the CLI boundary**. Module-level error enums in `errors/`. Functions return `Result<T, CfgdError>` or module-specific errors. `anyhow::Result` is only used in `main.rs` and `cli/`.

5. **Config structs derive `serde::Deserialize` and `serde::Serialize`**. All config types live in `config/`. No config parsing logic outside that module.

6. **No `std::process::Command` outside of `cli/`, `packages/`, `secrets/`, `system/`, `reconciler/`, `platform/`, `sources/`, `gateway/`, `output/`, `generate/`, `oci/`, `daemon/`, and `crates/cfgd-csi/`**. If you need to shell out, it must go through a controlled execution layer, not scattered across the codebase. `cli/` spawns `$EDITOR` for resource editing commands. `secrets/` shells out to `sops` and external provider CLIs (`op`, `bw`, `vault`). `system/` implements `SystemConfigurator` trait (same provider pattern as `packages/`). `reconciler/` handles script execution (pre/post-reconcile hooks). `platform/` shells out for OS detection (`sw_vers`, `freebsd-version`). `sources/` shells out to `git` for signature verification and clone fallback. `gateway/` shells out to `ssh-keygen` and `gpg` for enrollment signature verification. `output/` runs commands via `Printer::run_with_output` (the controlled execution layer for buffered progress display). `generate/` shells out for tool inspection (`--version` checks) and system settings scanning. `oci/` shells out to Docker credential helpers (`docker-credential-*`). `daemon/` shells out to `sc.exe` for Windows Service lifecycle management. `crates/cfgd-csi/` may shell out to `mount`/`umount` as fallback for bind mount operations.

### Style

- **Formatting**: `cargo fmt` (rustfmt defaults). No custom rustfmt.toml.
- **Linting**: `cargo clippy -- -D warnings`. All clippy warnings are errors.
- **Naming**: Rust conventions. snake_case for functions/variables, PascalCase for types/traits, SCREAMING_SNAKE for constants.
- **Imports**: Group by std, external crates, internal modules. Separated by blank lines.
- **Config serde**: `#[serde(rename_all = "camelCase")]` on config structs to match Kubernetes ecosystem conventions (maps Rust snake_case to YAML camelCase). Enums have no rename_all — they serialize as PascalCase by default.
- **Tests**: Co-located unit tests in `#[cfg(test)] mod tests {}` within each module. Integration tests in `tests/`.
- **Comments**: Only where the "why" isn't obvious. No doc comments on private functions unless the logic is genuinely complex.

### Patterns

- **Builder pattern** for complex structs (plans, configs).
- **Trait objects** (`Box<dyn PackageManager>`) for runtime polymorphism over package managers.
- **`impl Into<T>`** for function parameters where multiple types make sense.
- **Structured logging** via `tracing`. Use `tracing::info!`, `tracing::debug!`, etc. Never `log::*`.

### Shared Utilities — `cfgd-core/src/lib.rs`

Cross-cutting functions used by multiple modules live in `cfgd-core/src/lib.rs`. **Before writing any helper function, check lib.rs first** — if a similar function exists, use it. If a new function will be needed by more than one module, add it to lib.rs, not inline.

Current shared items (keep this list updated when adding new ones):
- `API_VERSION` — canonical API version string (`cfgd.io/v1alpha1`); use everywhere instead of string literals
- `utc_now_iso8601()` — ISO 8601 timestamp (the only timestamp function; do NOT create wrappers)
- `unix_secs_to_iso8601(secs)` — Unix epoch to ISO 8601
- `deep_merge_yaml(base, overlay)` — recursive YAML value merge
- `union_extend(target, source)` — Vec<String> merge without duplicates
- `default_config_dir()` — cross-platform config directory (`~/.config/cfgd` on Unix, `AppData\Roaming\cfgd` on Windows via `directories` crate)
- `command_available(cmd)` — check if a CLI command exists on PATH
- `expand_tilde(path)` — expand `~/...` or `~\...` to home directory; uses `HOME` on Unix, `USERPROFILE` (then `HOME`) on Windows
- `hostname_string()` — get system hostname as `String`; returns `"unknown"` on failure. Use instead of inline `hostname::get()` patterns
- `resolve_relative_path(path, base)` — resolve a path relative to a base directory with traversal validation; absolute paths returned as-is, relative paths joined and checked for `..` via `validate_no_traversal`
- `create_symlink(source, target)` — cross-platform symlink creation; Windows uses `symlink_file`/`symlink_dir`, errors with Developer Mode guidance on permission failure
- `file_permissions_mode(metadata) -> Option<u32>` — Unix mode bits (`mode() & 0o777`); returns `None` on Windows (NTFS uses inherited ACLs)
- `set_file_permissions(path, mode)` — set Unix mode bits; no-op on Windows. Use instead of direct `PermissionsExt`
- `is_executable(path, metadata) -> bool` — Unix checks executable bit; Windows checks file extension (`.exe`, `.cmd`, `.bat`, `.ps1`, `.com`)
- `is_same_inode(a, b) -> bool` — check if two paths refer to the same file (same inode+dev on Unix, same file index+volume on Windows); use instead of inline `MetadataExt::ino()` comparisons
- `git_cmd_safe(url, ssh_policy)` — build a `Command` for git with `GIT_TERMINAL_PROMPT=0` and configurable `StrictHostKeyChecking` for SSH URLs; `ssh_policy: Option<SshHostKeyPolicy>` (None = accept-new default); low-level builder, prefer `try_git_cmd` for the common try-CLI-then-fallback pattern
- `try_git_cmd(url, args, label, ssh_policy)` — run a git CLI command via `git_cmd_safe`, return `true` on success, log stderr via `tracing::debug` on failure; use before every git2 network operation as CLI-first fallback to prevent SSH hangs
- `git_ssh_credentials(url, username, allowed)` — git2 credential callback (SSH agent/keys + HTTPS credential helper)
- `parse_loose_version(s)` — parse "1.28" → semver Version(1.28.0); handles 1-part, 2-part, and 3-part versions
- `version_satisfies(version, requirement)` — check version against semver range (uses `parse_loose_version`)
- `copy_dir_recursive(src, dst)` — recursively copy a directory tree
- `merge_env(base, updates)` — merge `Vec<EnvVar>` by name (later overrides earlier); used by config merging, composition, reconciler
- `merge_aliases(base, updates)` — merge `Vec<ShellAlias>` by name (later overrides earlier); same semantics as `merge_env`
- `split_add_remove(values)` — split `&[String]` into (adds, removes); values starting with `-` are removals (strip prefix); powers unified `--thing` CLI flags
- `parse_env_var(input)` — parse `KEY=VALUE` string into `EnvVar`; validates name via `validate_env_var_name`; used by all CLI env flag parsing
- `parse_alias(input)` — parse `name=command` string into `ShellAlias`; validates name via `validate_alias_name`; used by all CLI alias flag parsing
- `validate_env_var_name(name)` — validate env var name matches `[A-Za-z_][A-Za-z0-9_]*`; prevents shell injection in generated env files
- `validate_alias_name(name)` — validate alias name matches `[A-Za-z0-9_.-]+`; prevents shell injection in generated alias definitions
- `stdout_lossy_trimmed(output)` — extract trimmed stdout from `Command` output as lossy UTF-8 string; use instead of inline `String::from_utf8_lossy` patterns
- `stderr_lossy_trimmed(output)` — extract trimmed stderr from `Command` output as lossy UTF-8 string; use instead of inline `String::from_utf8_lossy` patterns
- `sha256_hex(data)` — compute SHA256 hash of `&[u8]` and return as lowercase hex string; use instead of inline `Sha256::digest` patterns
- `atomic_write(target, content)` — atomic file write via temp+rename; returns SHA256 hash; use instead of `fs::write()` in ALL production code
- `atomic_write_str(target, content)` — string variant of `atomic_write`
- `capture_file_state(path)` — capture file content/permissions/symlink state for backup; returns `Option<FileState>`
- `FileState` — struct holding captured file state (content, hash, permissions, symlink info, oversized flag)
- `validate_path_within(path, root)` — canonicalize and verify path is within root directory
- `validate_no_traversal(path)` — reject paths containing `..` components
- `shell_escape_value(value)` — escape a value for shell `export` statements (single-quotes metacharacters)
- `xml_escape(s)` — escape `&<>"'` for safe XML/plist inclusion
- `acquire_apply_lock(state_dir)` — exclusive apply lock; uses `flock` on Unix, `LockFileEx` on Windows; returns `ApplyLockGuard` (RAII release on drop)
- `resolve_effective_reconcile(module, profile_chain, config)` — resolve per-module reconcile settings from patches; returns `EffectiveReconcile`
- `EffectiveReconcile` — resolved reconcile settings (interval, auto_apply, drift_policy) with no Options
- `CSI_DRIVER_NAME` — canonical CSI driver name string (`csi.cfgd.io`); use everywhere instead of string literals
- `MODULES_ANNOTATION` — canonical annotation key (`cfgd.io/modules`); use everywhere instead of string literals
- `sanitize_k8s_name(name)` — sanitize a string for Kubernetes RFC 1123 DNS label rules
- `parse_duration_str(s)` — parse "30s", "5m", "1h", or plain seconds into `Duration`; returns `Result<Duration, String>`
- `PROFILE_SCRIPT_TIMEOUT` — default timeout for profile-level scripts (5 minutes); use instead of hardcoded `Duration::from_secs(300)`
- `COMMAND_TIMEOUT` — default timeout for external commands (2 minutes)
- `GIT_NETWORK_TIMEOUT` — default timeout for git network operations (5 minutes)
- `command_output_with_timeout(cmd, timeout)` — run a `Command` with a timeout, killing the process if exceeded; use for any external command that could hang
- `terminate_process(pid)` — send SIGTERM (Unix) or TerminateProcess (Windows) to a process by PID; cross-platform, ungated
- `is_root()` — check if the current process runs with elevated privileges: euid==0 (Unix) or IsUserAnAdmin() (Windows)
- `cleanup_old_binary()` — remove `.exe.old` left by the Windows rename-dance self-upgrade; no-op on Unix. Called from `main.rs` on startup (lives in `upgrade.rs`, not `lib.rs`)

### Database Conventions

All SQLite databases (StateStore in cfgd-core, GatewayDb in cfgd-operator gateway) must:
- Set `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;` on open
- Use versioned migrations (not ad-hoc `CREATE TABLE IF NOT EXISTS`)
- Use `cfgd_core::utc_now_iso8601()` for timestamps — no local wrappers
- Hash with `Sha256::digest()` (one-liner) not `Sha256::new()` + `update()` + `finalize()`

### What NOT To Do

- Don't add features not in the current phase of PLAN.md.
- Don't create new utility files. Shared functions go in `cfgd-core/src/lib.rs`.
- Don't duplicate a function that already exists in lib.rs. Search first.
- Don't add backwards-compatibility shims. Just change the code.
- Don't over-abstract. Three similar lines > a premature abstraction.
- Don't add `#[allow(dead_code)]` — if code is unused, delete it.
- Don't create new files without checking if the functionality belongs in an existing module.
- Don't create local timestamp/hash/command-check wrappers — use the shared ones in lib.rs.

## Output System — Critical Design Constraint

The `output` module provides:
- `Theme` struct: all colors, styles, icons defined in one place
- `Printer` struct: the sole interface for writing to the terminal
- Methods like `printer.header()`, `printer.success()`, `printer.warning()`, `printer.error()`, `printer.plan_phase()`, `printer.progress_bar()`, `printer.diff()`, `printer.syntax_highlight()`
- Built on `console` crate for styling and `syntect` for syntax highlighting
- `indicatif` multi-progress bars managed through `Printer`, never created directly

Every module receives a `&Printer` (or `Arc<Printer>` in async contexts). This is non-negotiable.

## Config Format

Primary: YAML (KRM-inspired structure with `apiVersion`, `kind`, `metadata`, `spec`).
Secondary: TOML supported for user preference.
All parsing in `config/` module. See `docs/configuration.md` for schema reference.

## Testing

- `cargo test` must pass before any phase is considered complete.
- Unit tests for pure logic (config parsing, diffing, template rendering).
- Integration tests for CLI commands using `assert_cmd`.
- Package manager tests use mock trait implementations, not real system calls.
- Use `tempfile` for any test that touches the filesystem.

## Quality Scripts

- `.claude/scripts/audit.sh` — Run to check for DRY violations, banned patterns, module boundary violations.

Run these periodically during implementation sessions.

## Skills

`/implement-phase [N]`, `/audit`, `/add-package-manager [name]`, `/validate-gitops [target]`, `/scope-audit`
