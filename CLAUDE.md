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
│   ├── server_client.rs    # Device gateway HTTP client (checkin, enrollment, device flow)
│   └── upgrade.rs          # Self-upgrade: GitHub release detection, download, checksum verify
├── cfgd/src/               # Unified binary crate (workstation + node)
│   ├── main.rs             # Entry point, clap dispatch
│   ├── cli/                # Clap command definitions, argument parsing
│   ├── files/              # File management: copy, template, diff, permissions
│   ├── packages/           # PackageManager implementations (brew, apt, cargo, npm, pipx, dnf)
│   ├── secrets/            # SOPS/age backends, 1Password/Bitwarden/Vault providers
│   └── system/             # All SystemConfigurators — workstation (shell, macos-defaults, systemd, launchd, environment) + node (sysctl, kernel-modules, containerd, kubelet, apparmor, seccomp, certificates)
└── cfgd-operator/src/      # k8s operator binary crate
    ├── main.rs             # Operator entry point (controllers + optional gateway)
    ├── lib.rs              # Crate root, module declarations
    ├── crds/               # CRD definitions (MachineConfig, ConfigPolicy, DriftAlert)
    ├── controllers/        # kube-rs reconciliation controllers
    ├── webhook.rs          # Admission webhook server (TLS)
    ├── gen_crds.rs         # CRD JSON schema generation utility
    ├── errors.rs           # Operator-specific error types
    └── gateway/            # Device gateway (optional, enabled via DEVICE_GATEWAY_ENABLED)
        ├── mod.rs          # Gateway setup, Axum router assembly
        ├── api.rs          # REST API: checkin, enrollment, devices, drift, admin, SSE
        ├── db.rs           # SQLite: devices, credentials, tokens, challenges, events
        ├── fleet.rs        # Fleet status aggregation
        ├── web.rs          # Web dashboard (HTML/CSS/JS)
        └── errors.rs       # GatewayError with IntoResponse
```

See `.claude/team-config-controller.md` for the multi-source architecture and Phase 1-7 prep work.

## Quality Mandate

**Quality over speed.** Every module must be production-grade when committed. Do not leave stubs, `todo!()`, placeholder implementations, or partial features unless the full implementation is explicitly planned for a later phase in PLAN.md. If a phase's acceptance criteria aren't fully met, the phase isn't done — keep working or report what remains.

## Coding Standards

### Hard Rules — Violations Must Be Fixed Immediately

1. **ALL terminal output goes through `output::`**. No module may use `println!`, `eprintln!`, `console::*`, or `indicatif::*` directly. The `output` module owns all interaction with the terminal. This is the single most important architectural constraint. It enables consistent theming, syntax highlighting, and future TUI migration.

2. **No `unwrap()` or `expect()` in library code**. Use `?` with proper error types. `unwrap()` is permitted only in tests and in `main.rs` for top-level setup where failure means "crash immediately."

3. **All providers implement their respective traits** (`PackageManager`, `SystemConfigurator`, `FileManager`, `SecretBackend`). No ad-hoc shelling out. Every provider gets a struct that implements the trait. The reconciler depends on `ProviderRegistry`, never on concrete implementations.

4. **Errors use `thiserror` for library errors, `anyhow` only at the CLI boundary**. Module-level error enums in `errors/`. Functions return `Result<T, CfgdError>` or module-specific errors. `anyhow::Result` is only used in `main.rs` and `cli/`.

5. **Config structs derive `serde::Deserialize` and `serde::Serialize`**. All config types live in `config/`. No config parsing logic outside that module.

6. **No `std::process::Command` outside of `cli/`, `packages/`, `secrets/`, `system/`, `reconciler/`, `platform/`, `sources/`, `gateway/`, and `output/`**. If you need to shell out, it must go through a controlled execution layer, not scattered across the codebase. `cli/` spawns `$EDITOR` for resource editing commands. `secrets/` shells out to `sops` and external provider CLIs (`op`, `bw`, `vault`). `system/` implements `SystemConfigurator` trait (same provider pattern as `packages/`). `reconciler/` handles script execution (pre/post-reconcile hooks). `platform/` shells out for OS detection (`sw_vers`, `freebsd-version`). `sources/` shells out to `git` for signature verification and clone fallback. `gateway/` shells out to `ssh-keygen` and `gpg` for enrollment signature verification. `output/` runs commands via `Printer::run_with_output` (the controlled execution layer for buffered progress display).

### Style

- **Formatting**: `cargo fmt` (rustfmt defaults). No custom rustfmt.toml.
- **Linting**: `cargo clippy -- -D warnings`. All clippy warnings are errors.
- **Naming**: Rust conventions. snake_case for functions/variables, PascalCase for types/traits, SCREAMING_SNAKE for constants.
- **Imports**: Group by std, external crates, internal modules. Separated by blank lines.
- **Config serde**: `#[serde(rename_all = "kebab-case")]` on config structs to map YAML kebab-case to Rust snake_case.
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
- `command_available(cmd)` — check if a CLI command exists on PATH
- `expand_tilde(path)` — expand `~/...` to home directory
- `git_ssh_credentials(url, username, allowed)` — git2 credential callback (SSH agent/keys + HTTPS credential helper)
- `parse_loose_version(s)` — parse "1.28" → semver Version(1.28.0); handles 1-part, 2-part, and 3-part versions
- `version_satisfies(version, requirement)` — check version against semver range (uses `parse_loose_version`)
- `copy_dir_recursive(src, dst)` — recursively copy a directory tree
- `merge_env(base, updates)` — merge `Vec<EnvVar>` by name (later overrides earlier); used by config merging, composition, reconciler
- `merge_aliases(base, updates)` — merge `Vec<ShellAlias>` by name (later overrides earlier); same semantics as `merge_env`
- `split_add_remove(values)` — split `&[String]` into (adds, removes); values starting with `-` are removals (strip prefix); powers unified `--thing` CLI flags
- `parse_env_var(input)` — parse `KEY=VALUE` string into `EnvVar`; used by all CLI env flag parsing
- `parse_alias(input)` — parse `name=command` string into `ShellAlias`; used by all CLI alias flag parsing
- `atomic_write(target, content)` — atomic file write via temp+rename; returns SHA256 hash; use instead of `fs::write()` in ALL production code
- `atomic_write_str(target, content)` — string variant of `atomic_write`
- `capture_file_state(path)` — capture file content/permissions/symlink state for backup; returns `Option<FileState>`
- `FileState` — struct holding captured file state (content, hash, permissions, symlink info, oversized flag)
- `validate_path_within(path, root)` — canonicalize and verify path is within root directory
- `validate_no_traversal(path)` — reject paths containing `..` components
- `shell_escape_value(value)` — escape a value for shell `export` statements (single-quotes metacharacters)
- `xml_escape(s)` — escape `&<>"'` for safe XML/plist inclusion
- `acquire_apply_lock(state_dir)` — exclusive flock-based apply lock; returns `ApplyLockGuard` (RAII release on drop)
- `resolve_effective_reconcile(module, profile_chain, config)` — resolve per-module reconcile settings from patches; returns `EffectiveReconcile`
- `EffectiveReconcile` — resolved reconcile settings (interval, auto_apply, drift_policy) with no Options

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
