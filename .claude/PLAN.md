# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## Phase: Codebase-Wide Robustness Audit

### Context

During E2E test expansion and debugging the `cfgd init --from` flow with a real user, we found a pattern of bugs where a fix was applied in one place but the same class of problem existed elsewhere. Examples:

- **SSH clone hang**: Fixed in `sources::git_clone_with_fallback` but the identical bug existed in `sources::clone_source` AND `daemon::git_pull`. Required extracting `git_cmd_safe()` into lib.rs.
- **Path traversal**: `module create --file` would delete `/etc/passwd` and replace it with a symlink. The `copy_files_to_dir` function had no validation. Meanwhile `files/mod.rs` had proper `validate_no_traversal` + `validate_path_within` checks — the technique existed in the codebase but wasn't applied everywhere files are touched.
- **Fish config for non-fish users**: Written unconditionally in `plan_env` AND `verify_env` — two separate code paths both needed fixing.
- **serde camelCase mismatch**: `postApply` key in module YAML was silently ignored because it was written as `post-apply` (kebab-case). serde `rename_all = "camelCase"` silently drops unknown keys. There could be other YAML keys with the same silent-drop problem.
- **Double clone in init**: `resolve_from` cloned, then `cmd_init` called `clone_into` again. The `clone_into` function helpfully pulled on existing repos — a "feature" nobody asked for.
- **No timeouts on git operations**: Fixed for CLI clone but libgit2 fetch operations in sources and modules still have no timeout.
- **`regenerate_workflow` mutating cloned repos**: init called it unconditionally, writing a CI file into the user's cloned repo and dirtying their tree.
- **Apply status race**: `record_apply` was called with `Success` before apply started. Process crash = false success in DB.

### What This Audit Must Do

A full codebase sweep. Not just grepping for known-bad patterns — understanding the *techniques* used in well-protected code and finding everywhere those techniques are missing.

**Scope:** Every `.rs` file in `crates/`. Every `.sh` file in `tests/e2e/`. Every `.yaml` in `chart/`. The audit is not limited to files changed in recent work.

### Audit Categories

#### 1. Shared Techniques Not Applied Everywhere

For each defensive technique that exists somewhere in the codebase, find everywhere it SHOULD be applied but isn't:

- **Path validation** (`validate_no_traversal`, `validate_path_within`): `files/mod.rs` uses these. Does `copy_files_to_dir`? Module file resolution? Template source resolution? Anywhere a user-controlled path reaches a filesystem operation.
- **SSH safety** (`git_cmd_safe`): Now in lib.rs. Is every `git2::RemoteCallbacks` usage preceded by a git CLI attempt? Check module fetch, source fetch, daemon pull, auto-commit-push.
- **Atomic writes** (`atomic_write`, `atomic_write_str`): Are there `fs::write()` calls that should use the atomic variant? Direct writes to config files, state files, env files.
- **File permission safety** (`set_file_permissions`, `file_permissions_mode`): Are permissions checked/set consistently after file deployment? Are there bare `fs::set_permissions` calls that should use the cross-platform wrapper?
- **Symlink safety** (`create_symlink`): Are there bare `std::os::unix::fs::symlink` calls? Does the cross-platform wrapper handle all cases?
- **Process output helpers** (`stdout_lossy_trimmed`, `stderr_lossy_trimmed`): Are there inline `String::from_utf8_lossy` patterns that should use the shared helpers?
- **Hash helpers** (`sha256_hex`): Are there inline `Sha256::digest` → hex patterns?
- **Timestamp helpers** (`utc_now_iso8601`): Are there local timestamp wrappers or inline chrono calls?
- **Tilde expansion** (`expand_tilde`): Are there bare `Path::new("~/...")` constructions that skip expansion?
- **Command availability** (`command_available`): Are there `Command::new("X").output()` calls that don't check availability first?

#### 2. Silent Failures

Find every place where an error is swallowed:
- `let _ = ...` on Result types
- `.unwrap_or_default()` on operations that could meaningfully fail
- `.ok()` on Results where the error matters
- `2>/dev/null || true` in shell scripts without `--ignore-not-found` (already swept E2E tests; check setup scripts, CI workflows, install scripts)
- serde `#[serde(default)]` on fields where a missing value should be an error, not a default
- serde silently ignoring unknown YAML keys (is `deny_unknown_fields` appropriate anywhere?)

#### 3. Missing Timeouts

Every `Command::new(...).output()` / `.status()` call and every `git2` network operation. Catalog them. Which ones could hang? Which ones need timeout wrappers?

#### 4. Platform Safety

- Every `#[cfg(unix)]` block: is there a corresponding `#[cfg(windows)]` that handles the same operation?
- Every `std::os::unix::*` import: is it gated?
- Every `/` path separator: should it be `std::path::MAIN_SEPARATOR` or `Path::join`?
- Every `HOME` env var read: does it fall back to `USERPROFILE` on Windows?
- Every `fs::set_permissions` with Unix mode bits: is it gated or no-op on Windows?

#### 5. Concurrency Safety

- Every `unsafe` block: is it justified? Is the safety invariant documented?
- Every `static mut` or global mutable state
- Every `std::env::set_var` / `std::env::remove_var` (these are unsafe in Rust 2024)

#### 6. State Consistency

- Every `StateStore` write: is the status correct at the time of write? (The `InProgress` fix was one instance)
- Every place where multiple files are written as a "transaction": what happens if the process dies between writes?
- Every `fs::remove_file` / `fs::remove_dir_all`: is there a backup? Could this leave the system in a state that's worse than before?

#### 7. Config Schema Integrity

- Every serde struct: does `rename_all = "camelCase"` match the documented YAML format?
- Every enum: are variants PascalCase (the default) and is that what the YAML expects?
- Are there struct fields that should be `Option<T>` but aren't (causing deserialization failure on absent keys)?
- Are there `Option<T>` fields that should be required (allowing silent misconfiguration)?

#### 8. Test Safety (E2E)

- Every test that calls `apply --yes`: is the target directory a scratch dir?
- Every test that creates files: are they in `$SCRATCH` or `$TGT`?
- Every `kubectl delete` in K8s tests: does it have `--ignore-not-found`?
- Every port-forward: is the PID tracked and killed in cleanup?
- Every background process: is it killed on exit?

### Deliverable

For each finding: exact file, exact line, what the code does, what's wrong, and the fix. Group by category. Fix everything — no "track for later." If a fix is too large to do inline (e.g., adding rayon for parallelism), create a focused follow-up item in this plan with enough detail that the next session can execute it without re-investigating.

### After the Audit

Once all findings are fixed and tests pass:
1. Build a Docker image that simulates `cfgd init --from git@github.com:tj-smith47/cfgd-config.git --apply-module nvim -y` on a clean Ubuntu and clean macOS-like environment
2. Validate every file created is in the right place with the right content/permissions
3. Validate no files are created outside the expected set
4. Run the full CLI E2E suite and confirm 0 failures
