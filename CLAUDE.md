# cfgd

Declarative, GitOps-style machine configuration state management. Written in Rust.

A suite of products sharing a core reconciliation engine:
- **cfgd** — machine config management CLI + daemon; Phases 1-7 ship workstation providers [Phases 1-7]
- **cfgd-server** — fleet control plane, web UI, k8s operator [Phase 8]
- **cfgd-node** — k8s node-level config agent (DaemonSet) [Phase 10]

Each binary operates in two modes:
- `cfgd <command>` — CLI for direct interaction (apply, add, status, plan)
- `cfgd daemon` — long-running process that watches for drift, auto-syncs, reconciles

## Architecture

See `.claude/architecture.md` for the full module design, trait definitions, and data flow.
See `.claude/PLAN.md` for the phased implementation plan.

## Module Map

```
src/
├── main.rs          # Entry point, clap dispatch
├── cli/             # Clap command definitions, argument parsing
│   └── mod.rs       # One submodule per command group
├── config/          # YAML config loading, profile resolution, layer merging
│   └── mod.rs
├── output/          # CENTRALIZED theming, styled output, progress, syntax highlighting
│   └── mod.rs       # ALL terminal output goes through this module
├── providers/       # Provider traits (PackageManager, SystemConfigurator, FileManager,
│   └── mod.rs       # SecretBackend, SecretProvider) + ProviderRegistry
├── files/           # File management: copy, template, diff, permissions
│   └── mod.rs
├── packages/        # PackageManager implementations (trait defined in providers/)
│   └── mod.rs       # brew, apt, cargo, npm, pipx, dnf
├── secrets/         # SOPS encryption (primary), age fallback, external providers
│   └── mod.rs       # SecretBackend + SecretProvider implementations
├── reconciler/      # Diff engine: actual state vs desired state, plan generation
│   └── mod.rs
├── state/           # SQLite state store: history, drift events, apply log
│   └── mod.rs
├── daemon/          # File watchers, reconciliation loop, sync, notifications
│   └── mod.rs
├── sources/         # Multi-source config management (Phase 9, stubs only until then)
│   └── mod.rs       # SourceManager, git fetching, caching
├── composition/     # Multi-source merge engine with policy enforcement (Phase 9)
│   └── mod.rs       # CompositionEngine, conflict resolution
└── errors/          # Error types (thiserror), result aliases
    └── mod.rs
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

6. **No `std::process::Command` outside of `packages/` and `secrets/`**. If you need to shell out, it must go through a controlled execution layer, not scattered across the codebase. `secrets/` shells out to `sops` and external provider CLIs (`op`, `bw`, `vault`). Script execution (pre/post-apply) is handled by the reconciler.

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

### What NOT To Do

- Don't add features not in the current phase of PLAN.md.
- Don't create utility modules or helpers for one-off operations.
- Don't add backwards-compatibility shims. Just change the code.
- Don't over-abstract. Three similar lines > a premature abstraction.
- Don't add `#[allow(dead_code)]` — if code is unused, delete it.
- Don't create new files without checking if the functionality belongs in an existing module.

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
All parsing in `config/` module. See `.claude/architecture.md` for schema definitions.

## Testing

- `cargo test` must pass before any phase is considered complete.
- Unit tests for pure logic (config parsing, diffing, template rendering).
- Integration tests for CLI commands using `assert_cmd`.
- Package manager tests use mock trait implementations, not real system calls.
- Use `tempfile` for any test that touches the filesystem.

## Quality Scripts

- `.claude/scripts/audit.sh` — Run to check for DRY violations, banned patterns, module boundary violations.
- `.claude/scripts/check-style.sh` — Verify formatting, clippy, and naming conventions.

Run these periodically during implementation sessions.

## Skills

`/implement-phase [N]`, `/audit`, `/add-package-manager [name]`, `/validate-gitops [target]`, `/scope-audit`
