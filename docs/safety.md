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

These backups are automatic, cover only files cfgd itself is about to write, and exist to power
`cfgd rollback`. To snapshot arbitrary files or directories on a schedule — an application
database, a photo library — declare them in `spec.backups[]`; see
[Declarative Backups](backups.md).

## Unmanaged-File Adoption

When `cfgd apply` reaches a target that already holds a file cfgd has never
written, [`--on-conflict`](cli-reference.md#unmanaged-files-at-a-managed-target)
decides what happens to it. The default (`backup`, once `--yes` or a
non-interactive stdin has ruled out asking) leaves a sidecar copy at
`<target>.cfgd-backup`.

That sidecar is a **copy, never a move**:

- The original stays at the target until the managed write rename-replaces it,
  so at every instant the content is readable at the sidecar, at the target, or
  at both — a crash mid-apply cannot leave it at neither
- The copied bytes are re-read and hashed before the copy is accepted; a short
  write is an error, not a sidecar quietly holding less than it claims
- The copy carries the original's permission bits
- A symlinked target is copied as a symlink, so the link is preserved rather
  than flattened into its destination
- The copy carries the original's setuid, setgid and sticky bits as well as its
  permission bits — a sidecar is the file it preserves, and a special bit
  dropped in the copy cannot be restored from it
- An existing `<target>.cfgd-backup` holding *different* content is never
  clobbered: the newer copy lands at `<target>.cfgd-backup.<timestamp>`, so the
  sidecar `cfgd profile update` and module removal offer to restore is always
  the content that predates cfgd
- The timestamp has one-second resolution, so it is a hint at a free name rather
  than a guarantee of one. Every candidate path is checked before it is written,
  and a taken one moves to `<target>.cfgd-backup.<timestamp>-1`, `-2`, … — two
  adoptions of the same target inside one second land beside each other, never
  on top of each other. The same check covers a directory target, where an
  occupied sidecar would otherwise be *merged into* rather than replaced

A target that already holds exactly the bytes cfgd would write is not adopted at
all — no prompt, no sidecar, no rewrite, and the run does not report a change it
did not make.

### Durability of a sidecar

Two different failures, two different guarantees:

| Failure | Linux / macOS / BSD | Windows |
|---|---|---|
| **Process crash / kill** (`cfgd` dies, OS keeps running) | Guaranteed. The sidecar is written to a temp file and renamed into place; the rename is atomic, so the path either does not exist or holds complete, hash-verified content | Guaranteed, on the same basis |
| **Power loss / kernel panic** | Guaranteed. The temp file is `fsync`ed before the rename and the parent directory is `fsync`ed after it, so both the content and the directory entry naming it are on stable storage before the copy is reported | **Best-effort.** The content is flushed, but the directory entry is left to the filesystem's own flush interval; a sidecar reported as written may not survive an immediate power cut |

Neither platform's guarantee depends on the *target* surviving: the original is
still at the target throughout, because the sidecar is a copy.

### The daemon does not run this pass

`--on-conflict` is a `cfgd apply` / `cfgd init --apply` flag, and the adoption
pass that reads it lives in the CLI. **The daemon's auto-apply reconcile loop
does not run it**: a daemon tick that finds an unmanaged file at a managed
target overwrites it, without a prompt, a sidecar copy, or a way to configure
otherwise.

What still protects that write is the transaction journal below — the daemon's
applies record `file_backups` rows exactly as a CLI apply does, so
`cfgd rollback <apply-id>` restores the overwritten content. What is missing is
the *pre-write sidecar* and the *policy*: a daemon cannot prompt, so a
config-driven policy is the only shape available to it, and none is defined yet.
Until one is, a machine whose targets may hold files cfgd never wrote should be
adopted once with `cfgd apply` before the daemon's auto-apply is enabled.

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
- Package installs and system changes require manual review (listed in output, most recent
  first — the order actions *finished*, which is what "undo the last thing" means once the
  `Packages` phase runs its managers concurrently)

Rollback is available for any apply that has backups in the state store.

## Apply Locking

cfgd takes an exclusive whole-file lock to prevent concurrent applies: `flock()` on Unix, `LockFileEx` on Windows. Only one `cfgd apply` can run at a time.

- The lock file is at `~/.local/state/cfgd/apply.lock` (Linux; under the state dir on every platform, see `configuration.md`)
- The daemon skips reconciliation ticks if the lock is held by a CLI apply
- The lock is released automatically when the process exits
- The holder records its PID in the lock file, and a refused apply names it: `apply lock held by another process: pid 12345`

**Resolving a stuck lock**: A crash does not leave a stuck lock. The OS releases the lock the moment the crashed process's file handle closes. The leftover file is unlocked, and the next acquire reuses it. If the file names a PID that is no longer running, that record is stale and harmless: the next holder overwrites it. In that no-holder case you may delete the file, but check first. Kill the PID shown if it is still alive, or retry the acquire. A refused acquire means a live holder exists, whatever the PID record says.

The message reads `unknown pid` when the file holds no complete PID record, and cfgd would rather say it does not know than name a process it is not sure about. Two things produce it:

- A holder that is not cfgd (`flock(1)`, say) never writes a record at all.
- **Version skew across an upgrade.** cfgd started writing a terminator after the PID. A daemon still running from before `cfgd upgrade` writes the older, terminator-less record, which a newer contender will not read as a PID. The holder is a perfectly legitimate cfgd process.

Both `unknown pid` cases have a live holder. Find it and stop it: kill the non-cfgd holder (`fuser` or `lsof` on the lock file will name it), or restart the skewed daemon (`systemctl --user restart cfgd`, or whatever supervises it) to put the two on the same record format.

**Never delete a lock file while its holder is alive.** The holder keeps its lock on the deleted file. The next acquire creates a fresh file at the same path and locks that one instead. Both processes then run at once. This split is the one sequence cfgd's own lock safety checks cannot catch, because each holder's lock is valid on its own file.

**NFS caveat**: flock-based locking is not sound on NFS-backed state or cache directories. Linux emulates `flock()` over NFS with POSIX locks, and closing any descriptor for the file drops the lock. Keep the state dir and source cache on a local filesystem.

**The PID is advisory; the refusal is not.** "Lock held" is decided by the OS and is always correct. The PID is read from the file separately, and a holder that crashed without clearing its record leaves it in place until the next holder overwrites it, so a contender arriving in the syscall-narrow window between that acquire and that write can name the *previous* holder. Treat the PID as a starting point for `ps`, not as proof.

## Graceful Interruption (SIGINT / SIGTERM)

`cfgd apply` handles `SIGINT` (Ctrl-C) and `SIGTERM` as a **cooperative abort** rather than an abrupt kill:

- **File and package actions** finish before the abort is honoured — atomic file writes complete, and every package install already in flight completes before the reconciler stops. The abort is checked before anything new is dispatched, never mid-write, so the concurrent `Prerequisites` and `Packages` phases drain their running lanes rather than dropping them.
- **Script actions** (`preApply`, `postApply`, module scripts) are killed immediately: cfgd sends `SIGKILL` to the script's process group so the process exits within milliseconds instead of waiting for the full script timeout. Script authors should write idempotent scripts so a kill-and-rerun leaves the system in a clean state.
- The reconciler stops **before** starting the next action after any killed/completed abort and unwinds normally.
- The apply lock is released via its normal RAII drop (the guard drops as `cfgd apply` returns, *before* the process exits), so a subsequent `cfgd apply` runs immediately (no stuck lock).
- The run is journaled with status `Aborted` (visible in `cfgd status` / `cfgd log`), distinct from `success` / `partial` / `failed`.
- The process exits with the signal-conventional code: **130** for SIGINT, **143** for SIGTERM (128 + signal number).

**Second signal force-quits.** A second `SIGINT`/`SIGTERM` while the first abort is being processed takes the OS default disposition (immediate termination), so a user hammering Ctrl-C is never stuck waiting on cleanup. Because cfgd now responds to the first signal immediately (scripts are killed at once), the second signal is rarely needed.

The reported "{applied} of {total}" count is **filter-aware**: under `--phase` / `--skip` / `--only` / `--skip-scripts`, `total` is the number of actions actually in scope for the run, not the whole plan. A one-line message is printed, and `-o json` carries a structured payload:

```console
$ cfgd apply --yes            # Ctrl-C pressed during the package install
Apply
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  abortdemo
  Phases   Prerequisites, Packages, Files
  Actions  3 planned

Phase: Prerequisites
  cfgd:managers
    ✓ refresh slowbox index

Phase: Packages
  profile:abortdemo
    ✓ slowbox install epsilon (6.0s)

⚠ apply aborted by signal — 2 of 3 actions applied; no partial writes, rerun to converge
⊙ 1 action not attempted (6.0s)
$ echo $?
130
```

The in-flight install finished; the `Files` action after it was never dispatched, so its
phase never opened. The same run under `-o json` carries the counts as a payload:

```console
$ cfgd apply --yes -o json   # same run, interrupted the same way
{
  "aborted": true,
  "applied": 2,
  "failed": 0,
  "signal": "SIGINT",
  "total": 3
}
```

A signal reaches the child process too, so an install that was in flight can die with
the run rather than merely stopping before it. That action is a failure, and both
surfaces say so — the closing line gains `, 1 failed` and the payload's `failed` count
rises — so `total - applied` is never read as "never started".

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
