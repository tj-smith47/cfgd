---
name: add-package-manager
description: Scaffold a new PackageManager trait implementation for cfgd
allowed-tools: ["Read", "Edit", "Write", "Grep", "Bash(cargo *)"]
user-invocable: true
argument-hint: "[manager-name]"
---

## Add a New Package Manager

Scaffold a new `PackageManager` trait implementation for: $ARGUMENTS

### Before writing code:

1. Read the trait definition:
   - `/opt/repos/cfgd/crates/cfgd-core/src/providers/mod.rs` — PackageManager trait definition
   - `/opt/repos/cfgd/crates/cfgd/src/packages/mod.rs` — shared helpers and existing implementations

2. Study an existing implementation (e.g., `BrewManager`) for the pattern.

### Implementation:

1. Create `crates/cfgd/src/packages/$ARGUMENTS.rs` with a struct that implements `PackageManager`:
   - `name()` — human-readable name
   - `is_available()` — check if the binary exists on PATH via `cfgd_core::command_available()`
   - `installed_packages()` — parse output of the package manager's list command
   - `install()` — run install command, report progress via `printer`
   - `uninstall()` — run uninstall command
   - `update()` — update package index and upgrade

2. Add `mod $ARGUMENTS;` to `crates/cfgd/src/packages/mod.rs`.

3. Register the new manager in the package manager factory/registry.

4. Add config schema support in `crates/cfgd-core/src/config/` for the new manager's package spec. Config structs must use `#[serde(rename_all = "camelCase")]`.

### Rules:

- Shell out via `std::process::Command` — this is one of the allowed modules for it.
- All output through the `Printer` parameter.
- No `unwrap()` — use `PackageError` variants.
- Write unit tests with a mock that verifies the command construction without executing.

### After implementation:

1. Run `cargo test`
2. Run `bash .claude/scripts/audit.sh`
3. Verify the manager is skipped gracefully when not available on the current system.
