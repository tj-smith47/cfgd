---
paths: ["**/*.rs"]
---
# cfgd Hard Rules — violations must be fixed immediately

1. **ALL terminal output goes through `output::`**. No module outside `output/` may use `println!`, `eprintln!`, `print!`, `eprint!`, `console::*`, or `indicatif::*` directly. The `output` module owns all interaction with the terminal. This is the single most important architectural constraint — it enables consistent theming, syntax highlighting, and future TUI migration. The pre-R3 `Printer::{success, warning, info, error, header, subheader, key_value, newline, plan_phase, stdout_line}` methods no longer exist — use `status_simple(Role::Ok/Info/Warn/Err, ...)`, `heading(...)`, `kv(...)`, `data_line(...)` instead. See `output-module.md`.

2. **No `unwrap()` or `expect()` in library code**. Use `?` with proper error types. `unwrap()` is permitted only in tests and in `main.rs` for top-level setup where failure means "crash immediately."

3. **All providers implement their respective traits** (`PackageManager`, `SystemConfigurator`, `FileManager`, `SecretBackend`). No ad-hoc shelling out. Every provider gets a struct that implements the trait. The reconciler depends on `ProviderRegistry`, never on concrete implementations.

4. **Errors use `thiserror` for library errors, `anyhow` only at the CLI boundary**. Module-level error enums in `errors/`. Functions return `Result<T, CfgdError>` or module-specific errors. `anyhow::Result` is only used in `main.rs` and `cli/`.

5. **Config structs derive `serde::Deserialize` and `serde::Serialize`**. All config types live in `config/`. No config parsing logic outside that module.

6. **`std::process::Command` is restricted by module**. See `module-boundaries.md` for the allow-list.

## Quality Mandate

**Quality over speed.** Every module must be production-grade when committed. No stubs, `todo!()`, placeholder implementations, or partial features unless the full implementation is explicitly planned for a later phase in `PLAN.md`. If a phase's acceptance criteria aren't fully met, the phase isn't done.
