---
paths: ["**/*.rs"]
---
# cfgd Shared Utilities — `cfgd-core/src/util/`

Cross-cutting functions used by multiple modules live in `cfgd-core/src/util/<topic>.rs` and are re-exported through `cfgd-core/src/lib.rs` so external callers reach them as `cfgd_core::<name>(...)`. **Before writing any helper function, check the topic file first** — if a similar function exists, use it. If a new function will be needed by more than one module, add it to the topic file that matches its domain (or to a new topic file if none fits cleanly).

External call sites do not change: `cfgd_core::utc_now_iso8601(...)`, `cfgd_core::sha256_hex(...)`, `cfgd_core::atomic_write(...)`, etc. — the topic-file split is a layout change, not an API change.

## Topic files

| Topic file | What goes here |
|---|---|
| `util/constants.rs` | Crate-wide constants: API/CSI strings, k8s label keys, OCI annotations, timeouts, histogram-bucket presets |
| `util/time.rs` | Timestamps + duration parsing |
| `util/yaml_merge.rs` | YAML deep merge + Vec<EnvVar/ShellAlias>/Vec<String> mergers |
| `util/strings.rs` | Env-var / alias parsing + validation, shell/XML/k8s-name escaping & sanitization |
| `util/paths.rs` | `default_config_dir`, `expand_tilde`, `resolve_relative_path`, `validate_path_within`, `validate_no_traversal`, `copy_dir_recursive`, plus the test-home thread-local: `TestHomeGuard`, `with_test_home`, `with_test_home_guard` |
| `util/fs_perms.rs` | Cross-platform symlinks, permissions, exec-bit, inode/file-index identity |
| `util/file_io.rs` | `atomic_write[_str]`, `capture_file_state[_resolved]`, `FileState` struct |
| `util/process.rs` | Command helpers (`command_output_with_timeout`, `command_available`, `terminate_process`, `stdout/stderr_lossy_trimmed`, `is_root`, `hostname_string`, `tracing_env_filter`, `require_tool`) |
| `util/env_session.rs` | User-session live env refresh shell-outs: `refresh_session_env` (idempotent, all-platform dispatch), `launchctl_setenv`, `windows_setx`. The controlled `Command` layer for `launchctl`/`systemctl --user`/`setx` (see module-boundaries.md); shared by the user `spec.env` path and the system `environment` configurator |
| `util/git.rs` | Git + Sigstore/cosign factories: `git_cmd_safe`, `try_git_cmd`, `cosign_cmd`, `detect_default_branch`, `git_ssh_credentials` |
| `util/hashing.rs` | SHA256 helpers + loose-semver parsing/satisfaction |
| `util/apply_lock.rs` | `ApplyLockGuard` + `acquire_apply_lock` (Unix/Windows) |
| `util/reconcile.rs` | `EffectiveReconcile` + per-module reconcile-patch resolution |
| `util/encryption.rs` | `is_file_encrypted` (sops/age detection) |

## Constants

- `API_VERSION` — canonical API version string (`cfgd.io/v1alpha1`); use everywhere instead of string literals
- `CSI_DRIVER_NAME` — canonical CSI driver name string (`csi.cfgd.io`)
- `MODULES_ANNOTATION` — canonical annotation key (`cfgd.io/modules`)
- `LABEL_MACHINE_CONFIG` — k8s label key (`cfgd.io/machine-config`); use instead of the raw string in gateway/controllers
- `LABEL_DEVICE_ID` — k8s label key (`cfgd.io/device-id`); use instead of the raw string in gateway/controllers
- `OCI_ANNOTATION_PLATFORM` — OCI manifest annotation key (`cfgd.io/platform`); use instead of the raw string in oci.rs
- `PROFILE_SCRIPT_TIMEOUT` — 5 minutes; use instead of hardcoded `Duration::from_secs(300)`
- `COMMAND_TIMEOUT` — 2 minutes, for external commands
- `GIT_NETWORK_TIMEOUT` — 5 minutes, for git network operations
- `DURATION_BUCKETS_SHORT` / `DURATION_BUCKETS_LONG` — Prometheus histogram bucket presets

## Time

- `utc_now_iso8601()` — ISO 8601 timestamp (the only timestamp function; do NOT create wrappers)
- `unix_secs_now()` — current Unix epoch seconds
- `unix_secs_to_iso8601(secs)` — Unix epoch to ISO 8601
- `iso8601_to_filename_safe(ts)` — strip `:`, `-`, `T`, `Z` from an ISO 8601 timestamp so it can be used as a path segment; use instead of inline `.replace([':', '-', 'T', 'Z'], "")`
- `utc_now_filename_safe()` — convenience: current UTC time as a filename-safe string (composes the two above)
- `parse_duration_str(s)` — parse "30s", "5m", "1h", or plain seconds into `Duration`

## YAML / merges

- `deep_merge_yaml(base, overlay)` — recursive YAML value merge
- `union_extend(target, source)` — Vec<String> merge without duplicates
- `merge_env(base, updates)` — merge `Vec<EnvVar>` by name (later overrides earlier)
- `merge_aliases(base, updates)` — merge `Vec<ShellAlias>` by name
- `split_add_remove(values)` — split `&[String]` into (adds, removes); values starting with `-` are removals

## CLI parsing / validation

- `parse_env_var(input)` — parse `KEY=VALUE` into `EnvVar`; validates via `validate_env_var_user_name`
- `parse_alias(input)` — parse `name=command` into `ShellAlias`; validates via `validate_alias_name`
- `validate_env_var_user_name(name)` — validates shell-safety + rejects reserved `CFGD_*` prefix; use for all user-supplied env var names
- `validate_env_var_name(name)` — matches `[A-Za-z_][A-Za-z0-9_]*`; prevents shell injection (low-level; prefer `validate_env_var_user_name` for user input)
- `validate_alias_name(name)` — matches `[A-Za-z0-9_.-]+`; prevents shell injection
- `shell_escape_value(value)` — escape a value for shell `export` statements
- `escape_double_quoted(s)` — escape inside bash/zsh double quotes
- `xml_escape(s)` — escape `&<>"'` for safe XML/plist inclusion
- `sanitize_k8s_name(name)` — RFC 1123 DNS label sanitization

## Config

- `cfgd_core::config::is_yaml_ext(path) -> bool` — case-insensitive `yaml`/`yml` extension predicate; use instead of open-coding `ext == "yaml" || ext == "yml"` when iterating `<profiles_dir>/*.yaml`-style directories

## Filesystem

- `default_config_dir()` — cross-platform config dir (Unix `~/.config/cfgd`, Windows `AppData\Roaming\cfgd`)
- `expand_tilde(path)` — expand `~/...` or `~\...` to home; uses `HOME` on Unix, `USERPROFILE` (then `HOME`) on Windows
- `resolve_relative_path(path, base)` — resolve relative to base with traversal validation
- `validate_path_within(path, root)` — canonicalize and verify path within root
- `validate_no_traversal(path)` — reject paths containing `..`
- `atomic_write(target, content)` — atomic write via temp+rename; returns SHA256 hash; use instead of `fs::write()` in ALL production code
- `atomic_write_str(target, content)` — string variant
- `copy_dir_recursive(src, dst)` — recursively copy a directory tree
- `create_symlink(source, target)` — cross-platform; Windows errors with Developer Mode guidance
- `is_same_inode(a, b) -> bool` — check same file (inode+dev on Unix, file index+volume on Windows)
- `file_permissions_mode(metadata) -> Option<u32>` — Unix mode bits; `None` on Windows
- `set_file_permissions(path, mode)` — set Unix mode; no-op on Windows
- `is_executable(path, metadata) -> bool` — Unix exec bit; Windows checks `.exe/.cmd/.bat/.ps1/.com`
- `capture_file_state(path)` — capture content/permissions/symlink state; returns `Option<FileState>`
- `capture_file_resolved_state(path)` — like above but follows symlinks
- `FileState` — struct (content, hash, permissions, symlink info, oversized flag)

## Process / commands

- `command_available(cmd)` — check if a CLI command exists on PATH
- `command_output_with_timeout(cmd, timeout)` — run `Command` with timeout, kill on exceed; use for any external command that could hang
- `terminate_process(pid)` — SIGTERM (Unix) / TerminateProcess (Windows)
- `stdout_lossy_trimmed(output)` — trimmed lossy-UTF8 stdout from `Command` output
- `stderr_lossy_trimmed(output)` — trimmed lossy-UTF8 stderr
- `is_root()` — elevated privileges check: euid==0 (Unix) / IsUserAnAdmin() (Windows)
- `hostname_string()` — system hostname as `String`; `"unknown"` on failure
- `tracing_env_filter(default)` — `EnvFilter::try_from_default_env().unwrap_or_else(EnvFilter::new(default))`
- `require_tool(name, install_hint)` — uniform "X not found" error message, used by all `command_available`-gated CLI flows

## Git

- `git_cmd_safe(url, ssh_policy)` — build a `Command` for git with `GIT_TERMINAL_PROMPT=0` and configurable `StrictHostKeyChecking`; required for any operation that may touch a remote (clone, fetch, ls-remote)
- `git_cmd_local()` — build a `Command` for git suitable for LOCAL-only operations (`config` get/set, `tag -v`, `add`, `commit`, `rev-parse`, `log`). Sets `GIT_TERMINAL_PROMPT=0` (still desirable — prevents prompt-driven hang if git unexpectedly tries to authenticate) but skips `GIT_SSH_COMMAND` since no network is involved. Use instead of `std::process::Command::new("git")` for every local git invocation
- `try_git_cmd(url, args, label, ssh_policy)` — run via `git_cmd_safe`, return `true` on success; use before every git2 network operation as CLI-first fallback to prevent SSH hangs
- `detect_default_branch(repo_dir)` — best-effort detection of `origin/HEAD` then local `HEAD`; returns `Option<String>`
- `git_ssh_credentials(url, username, allowed)` — git2 credential callback (SSH agent + HTTPS helper)

## Sigstore / cosign

- `cosign_cmd()` — build a `Command` for cosign with piped stderr. Consumers add the subcommand (`sign`, `verify`, `verify-blob`, `attest`, `verify-attestation`, `generate-key-pair`) and flags. Use instead of `std::process::Command::new("cosign")` anywhere cosign is shelled out — the sole controlled layer for Sigstore shell-out across `oci.rs`, `cli/module.rs`, and `upgrade.rs`.

## Hashing / versions

- `sha256_hex(data)` — SHA256 of `&[u8]` as lowercase hex; use instead of inline `Sha256::digest` patterns
- `sha256_digest(data)` — OCI-style `sha256:<hex>` digest string
- `strip_sha256_prefix(s)` — strip `sha256:` prefix; idempotent if no prefix
- `parse_loose_version(s)` — parse "1.28" → semver Version(1.28.0); handles 1/2/3-part versions
- `version_satisfies(version, requirement)` — semver range check (uses `parse_loose_version`)

## Locks / reconcile

- `acquire_apply_lock(state_dir)` — exclusive apply lock; `flock` on Unix, `LockFileEx` on Windows; returns `ApplyLockGuard` (RAII release)
- `resolve_effective_reconcile(module, profile_chain, config)` — resolve per-module reconcile settings from patches
- `EffectiveReconcile` — resolved (interval, auto_apply, drift_policy) with no Options

## Encryption

- `is_file_encrypted(path, backend)` — sops (YAML/JSON `sops.mac` + `lastmodified`) or age (header byte check)

## Test guards

Reached via `cfgd_core::test_helpers::*` (the `test_helpers` module is gated behind the `test-helpers` Cargo feature, enabled in dev/test builds — not bare `cfgd_core::*` like the `util/` helpers above). Pair every consumer with `serial_test::serial` because env-var mutation is process-global.

- `EnvVarGuard::set(key, value)` / `EnvVarGuard::unset(key)` — RAII env-var save/restore (re-exported via `cfgd_core::test_helpers::EnvVarGuard`); captures prior value on construction, restores (or removes if no prior) on drop, even on panic
- `with_test_env_var(var, value, f)` — scoped env-var override; calls `f` with the var set to `Some(v)` or removed for `None`, then restores the prior value
- `CosignTestShim::install()` / `CosignTestShim::builder()...install()` — consolidated fake-cosign shim (Unix-only); writes a `/bin/sh` script under `CFGD_COSIGN_BIN` with builder-configured argv logging (`with_argv_logging`), keygen-mode key-pair writes on `generate-key-pair` (`with_keygen`), exit code (`with_exit`), and canned stderr (`with_stderr`). Captures + restores prior `CFGD_COSIGN_BIN` / `CFGD_FAKE_COSIGN_LOG` on drop. Replaces the per-file `CosignShimGuard` / `CosignShim` / `CosignKeygenShim` duplicates in `oci/sign/tests.rs`, `upgrade/tests.rs`, and `cli/module/tests.rs`

## Upgrade

- `cleanup_old_binary()` — remove `.exe.old` left by Windows rename-dance self-upgrade; no-op on Unix. Called from `main.rs` on startup (lives in `upgrade.rs`, not `util/`)

## What NOT to do

- Don't create new utility files outside `cfgd-core/src/util/`. Shared functions go in the existing topic file that matches the helper's domain.
- Don't add the same helper as a sibling of an existing topic file. Pick the existing topic.
- Don't create a brand-new topic file unless the helper genuinely doesn't fit any existing one — adding three string-validation functions doesn't justify a new file when `strings.rs` exists.
- Don't duplicate a function that already exists. Search the catalog above first.
- Don't create local timestamp/hash/command-check wrappers — use the shared ones above.
