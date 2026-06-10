# File Safety

cfgd is designed to be a safe, trustworthy tool for managing machine configuration. This document covers the safety mechanisms that protect your files.

## Atomic Writes

All file writes use a temp-file-then-rename pattern (`NamedTempFile::persist()`). This guarantees:

- **No partial writes**: if the process crashes mid-write, the original file is untouched
- **No corruption**: the rename is atomic on POSIX systems
- **Permission preservation**: existing file permissions are carried over

This applies to managed files, system configurator outputs (`/etc/environment`, `/etc/sysctl.d/`, systemd units, launchd plists), and node configurator configs (containerd, kubelet, AppArmor, seccomp).

## File Backups

Before overwriting any file during `cfgd apply`, the original content is captured and stored in the state database (`file_backups` table). Backups include:

- Full file content (up to 10 MB)
- File permissions
- Symlink targets (for symlink files)
- Timestamp of backup
- Association with the apply operation

Backups are retained for the last 10 applies and automatically pruned after each successful apply.

## Transaction Journal

Each `cfgd apply` creates a transaction journal (`apply_journal` table) that records:

- Every action attempted (phase, type, resource ID)
- Pre-state and post-state
- Success/failure status with error details
- Timestamps

This enables rollback of partially failed applies.

## Rollback

`cfgd rollback <apply-id>` restores files to the state that existed immediately
after the target apply — whether to recover a partially failed apply or to undo
a later one:

- Backed-up content is restored via atomic write (an empty managed file is
  restored as empty, not removed)
- Files created by a later apply — absent when the target apply completed — are removed
- Package installs and system changes require manual review (listed in output)

Rollback is available for any apply that has backups in the state store.

## Apply Locking

cfgd uses `flock()` to prevent concurrent applies. Only one `cfgd apply` can run at a time.

- The lock file is at `~/.local/state/cfgd/apply.lock` (Linux; under the state dir on every platform — see `configuration.md`)
- The daemon skips reconciliation ticks if the lock is held by a CLI apply
- The lock is released automatically when the process exits

**Resolving a stuck lock**: If a cfgd process crashes without releasing the lock, `flock()` releases it automatically on file descriptor close. If the lock file contains a stale PID (process no longer running), simply delete `~/.local/state/cfgd/apply.lock` or kill the PID shown in the error message.

## Graceful Interruption (SIGINT / SIGTERM)

`cfgd apply` handles `SIGINT` (Ctrl-C) and `SIGTERM` as a **cooperative abort** rather than an abrupt kill:

- **File and package actions** finish before the abort is honoured — atomic file writes complete, and a package install completes before the reconciler stops. The abort is detected between actions, never mid-write.
- **Script actions** (`preApply`, `postApply`, module scripts) are killed immediately: cfgd sends `SIGKILL` to the script's process group so the process exits within milliseconds instead of waiting for the full script timeout. Script authors should write idempotent scripts so a kill-and-rerun leaves the system in a clean state.
- The reconciler stops **before** starting the next action after any killed/completed abort and unwinds normally.
- The apply lock is released via its normal RAII drop (the guard drops as `cfgd apply` returns, *before* the process exits), so a subsequent `cfgd apply` runs immediately (no stuck lock).
- The run is journaled with status `Aborted` (visible in `cfgd status` / `cfgd log`), distinct from `success` / `partial` / `failed`.
- The process exits with the signal-conventional code: **130** for SIGINT, **143** for SIGTERM (128 + signal number).

**Second signal force-quits.** A second `SIGINT`/`SIGTERM` while the first abort is being processed takes the OS default disposition (immediate termination), so a user hammering Ctrl-C is never stuck waiting on cleanup. Because cfgd now responds to the first signal immediately (scripts are killed at once), the second signal is rarely needed.

The reported "{applied} of {total}" count is **filter-aware**: under `--phase` / `--skip` / `--only` / `--skip-scripts`, `total` is the number of actions actually in scope for the run, not the whole plan. A one-line message is printed, and `-o json` carries a structured payload:

```console
$ cfgd apply --yes
...
⚠ apply aborted by signal — 3 of 7 action(s) applied; no partial writes, rerun to converge
$ echo $?
130
```

```console
$ cfgd apply --yes --phase files -o json   # 2 file actions in scope, interrupted with Ctrl-C
{"aborted":true,"signal":"SIGINT","applied":1,"total":2}
```

Already-applied actions are real and recorded; rerun `cfgd apply` to converge the rest. On Windows, cooperative abort is not available and Ctrl-C falls back to the OS default disposition.

## Path Safety

cfgd validates all file paths to prevent directory traversal and symlink attacks:

- **Source path validation**: relative source paths are checked to ensure they don't escape the config directory via `../`
- **Traversal rejection**: paths containing `..` components are rejected before canonicalization
- **Symlink skip in source scan**: symlinks in source directories are skipped during scanning to prevent symlink attacks and infinite loops
- **TOCTOU mitigation**: source content is hashed during planning and verified at apply time; if the source changed between plan and apply, the action is aborted

## Daemon Drift Policy

The daemon's reconciliation behavior is controlled by `driftPolicy` in the reconcile config:

```yaml
spec:
  daemon:
    reconcile:
      driftPolicy: NotifyOnly  # Auto | NotifyOnly | Prompt
```

- **NotifyOnly** (default): detects drift, sends notification, records events, but does NOT automatically apply. User must run `cfgd apply` manually.
- **Auto**: applies drift corrections automatically (you must opt in)
- **Prompt**: future interactive approval mechanism

## Module Removal Cleanup

When a module is removed from a profile via `cfgd profile update --module -<name>`, cfgd:

1. Queries the file manifest to find all files the module deployed
2. Lists the files and prompts for confirmation
3. For each file: restores from backup if available, otherwise removes
4. Cleans up the module's state and manifest entries

## System Configurator Safety

### Environment Variables

Managed environment blocks use explicit `# BEGIN cfgd managed block` / `# END cfgd managed block` markers (backwards-compatible with older `# Managed by cfgd` format). Shell values are properly escaped using single quotes for metacharacters.

### Service Configs (containerd, kubelet)

Before writing config and restarting a service:

1. Serialized config is re-parsed to validate syntax
2. Existing config is backed up via `capture_file_state`
3. Config is written atomically
4. Service is restarted
5. If restart fails: backup is restored, service restarted again, error returned

### Plist Generation

All values interpolated into macOS plist XML are XML-escaped to prevent injection.
