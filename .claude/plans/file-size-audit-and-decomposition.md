# File Size Audit and Decomposition

## Context

Several files in the cfgd repo have grown to the point where they cannot be read in a single pass (2000-line limit). This makes them difficult to review, debug, and maintain. Files that are too large to read should be split into focused submodules. This is not cosmetic refactoring — it directly impacts the ability to catch bugs, enforce conventions, and reason about module boundaries.

## Goal

Audit every `.rs` file in the repo for excessive size. For each file that exceeds the readability threshold (~1500 lines), design and execute a decomposition that:

1. **Preserves all existing tests** — every test must still pass after the split
2. **Preserves all public API surface** — callers must not need to change (use `pub use` re-exports from the parent module)
3. **Creates cohesive submodules** — each new file should have a clear, single responsibility
4. **Does NOT introduce code duplication** — shared helpers stay in one place
5. **Does NOT create unnecessary abstraction layers** — splitting `mod.rs` into `mod.rs` + `foo.rs` + `bar.rs` with re-exports is fine; creating new trait hierarchies to justify the split is not

## Known Large Files (audit these first, then scan for others)

- `crates/cfgd/src/cli/mod.rs` — likely the largest file in the repo (CLI command dispatch)
- `crates/cfgd/src/packages/mod.rs` — all package manager implementations in one file
- `crates/cfgd-core/src/daemon/mod.rs` — daemon loop, reconciliation, sync, notifications
- `crates/cfgd-core/src/config/mod.rs` — config types, parsing, profile resolution
- `crates/cfgd/src/system/mod.rs` — all system configurator implementations
- `crates/cfgd/src/system/node.rs` — all node (k8s) configurators
- `crates/cfgd-operator/src/controllers/mod.rs` — all 5 controller reconcile functions
- `crates/cfgd-operator/src/gateway/api.rs` — all gateway REST endpoints
- `crates/cfgd-core/src/reconciler/mod.rs` — reconciler plan generation + apply

## Approach

### Phase 1: Audit

1. Run `wc -l` on every `.rs` file, sorted by size
2. Flag everything over 1500 lines
3. For each flagged file, identify natural split boundaries (look for section comments, trait impls, match arms over distinct concerns)

### Phase 2: Plan Decomposition

For each file to split, write a specific decomposition plan:
- What submodules will be created
- What code moves where
- What stays in the parent `mod.rs` (re-exports, shared types)
- Which tests need to move with their code vs. stay as integration tests

**Important: Plan BEFORE touching code.** Get alignment on the split boundaries before executing. Bad splits are worse than large files.

### Phase 3: Execute

For each file, one at a time:
1. Create the new submodule file(s)
2. Move code (cut from source, paste to destination)
3. Add `pub use` re-exports in the parent module so callers don't break
4. Run `cargo test` — must be green before moving to the next file
5. Run `cargo clippy -- -D warnings` — must be clean

### Phase 4: Verify

After all splits:
1. Full `cargo test` — every test passes
2. Full `cargo clippy` — zero warnings
3. `git diff --stat` — verify no unintended changes outside the target files
4. Spot-check that no code was duplicated (run `/dedup`)

## Anti-Patterns to Avoid

- **Don't create `utils.rs` / `helpers.rs` grab-bag files** — if a helper is shared, it goes in `cfgd-core/src/lib.rs` per CLAUDE.md
- **Don't split by "size" alone** — split by responsibility. Two 800-line halves of the same concern is worse than one 1600-line file
- **Don't break `pub use` re-exports** — external callers must not see the split
- **Don't move tests away from their code** — unit tests stay co-located in `#[cfg(test)] mod tests` within each new submodule
- **Don't create one-function files** — a submodule should have enough content to justify its existence (at least 50-100 lines of real logic)
- **Don't change any logic** — this is pure structural refactoring. Zero behavior changes.

## Suggested Decomposition Patterns

### `cli/mod.rs` → `cli/mod.rs` + `cli/apply.rs` + `cli/status.rs` + `cli/source.rs` + ...
Split by subcommand. Each `cmd_*` function and its helpers move to a file named after the command.

### `packages/mod.rs` → `packages/mod.rs` + `packages/brew.rs` + `packages/cargo.rs` + `packages/npm.rs` + ...
Split by package manager. Each `XxxManager` struct + impl moves to its own file.

### `daemon/mod.rs` → `daemon/mod.rs` + `daemon/reconcile.rs` + `daemon/sync.rs` + `daemon/notify.rs` + ...
Split by concern. The main loop stays in `mod.rs`; handler functions move to dedicated files.

### `system/mod.rs` → `system/mod.rs` + `system/shell.rs` + `system/systemd.rs` + `system/macos_defaults.rs` + ...
Split by configurator. Each `XxxConfigurator` moves to its own file.
