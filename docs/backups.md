# Declarative Backups

`spec.backups[]` declares snapshots of a file or directory that cfgd takes on your behalf,
retaining the newest N and pruning the rest. It is the "keep a copy of my app's data before I
touch it" surface — distinct from the automatic pre-overwrite `file_backups` that power
`cfgd rollback` (see [File Safety](safety.md#file-backups)).

| | `spec.backups[]` (this document) | Pre-overwrite backups |
|---|---|---|
| Declared by | you, in a profile | nobody — automatic |
| Covers | any file or directory on the machine | only files cfgd is about to write |
| Stored | on the filesystem, under `destination` | inline in the state DB |
| Retained | newest `retention` per backup | last 10 applies |
| Restored by | `cfgd backup restore` | `cfgd rollback` |

## Quick Start

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: workstation
spec:
  backups:
    - name: openlist-db
      source: /var/lib/openlist/data.db
      retention: 7
      preBackup:
        - systemctl stop openlist
      postBackup:
        - systemctl start openlist
```

That declaration snapshots `data.db` into `<state_dir>/backups/openlist-db/` as
`data.db.20260801T031500Z`, keeps the newest 7, and stops/starts the service around the copy so
the snapshot is consistent.

## CLI

A schedule-less backup (no `schedule`) also runs automatically during `cfgd apply`, after the
reconciler's file/package/module phases (skipped in `--dry-run`, shown in the plan preview
instead). A scheduled backup runs on its own timer in the [daemon](#daemon-scheduling), or on
demand via `cfgd backup run`.

Each schedule-less backup runs independently during apply — a unit that fails to complete (source
missing, a hook errored, or a state-store write failure) is reported as a `✗`/`Warn` status and
counted against the exit code, but does **not** abort the remaining backups or the rest of apply.
A failed or unclean unit downgrades the apply's overall status from `success` to `partial`, which
exits nonzero (`ExitCode::ApplyFailed`, code `7`) the same way a failed reconciler action would —
see [Exit Codes](cli-reference.md#exit-codes).

```console
$ cfgd backup run
Run Backups

✓ backup 'openlist-db'

✓ backup 'weekly'

$ cfgd backup run openlist-db
Run Backups

✓ backup 'openlist-db'

$ cfgd backup run missing-name
Run Backups

✗ Backup 'missing-name' not found

→ valid backups: openlist-db, weekly

$ cfgd backup list
Backups

Name          Source                       Schedule    Retention  Last Run                        Next Run
──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
openlist-db   /var/lib/openlist/data.db    -           7          success @ 2026-08-01T03:15:00Z   -
weekly        ~/Pictures                   0 3 * * *   3          never                            2026-08-02T03:00:00Z

$ cfgd --output json backup run openlist-db
[
  {
    "name": "openlist-db",
    "status": "success",
    "clean": true,
    "destinationPath": "/home/me/.local/state/cfgd/backups/openlist-db/data.db.20260801T031500Z"
  }
]

$ cfgd --output json backup run missing-name
{
  "error": "not_found",
  "hint": "valid backups: openlist-db, weekly",
  "name": "missing-name"
}
```

`cfgd backup run [name]` runs every declared backup when `name` is omitted, or just the named one.
An unknown name is a typed error (exit code `6`, see [Exit Codes](cli-reference.md#exit-codes))
that lists every valid name — in human mode as a `→` hint line below the failure, in `-o json`
as the payload's `hint` field — and a run whose snapshot did not complete cleanly — see
[Run Semantics](#run-semantics) for what "clean" means — also exits nonzero so a script can
detect it without parsing output.

`cfgd backup list [name]` (alias `ls`) shows every declared backup — or just the named one — its
last recorded run, and when the daemon's timer will next fire it (`nextRunAt` in `-o json`); every
backup command honors the global `-o`/`--output` flag for `json`/`yaml`/`jsonpath`/`template`
consumers.

`cfgd backup list <name> --snapshots` switches the view from the backup to its snapshots — what
[`cfgd backup restore`](#restoring) can put back:

```console
$ cfgd backup list openlist-db --snapshots
Snapshots: openlist-db

Snapshot                       Created               Size
─────────────────────────────────────────────────────────
data.db.20260801T231502Z       2026-08-01T23:15:02Z  1.2 MB
data.db.20260730T120000Z       2026-07-30T12:00:00Z  1.1 MB

$ cfgd --output json backup list openlist-db --snapshots
[
  {
    "name": "data.db.20260801T231502Z",
    "created": "2026-08-01T23:15:02Z",
    "sizeBytes": 1258291
  }
]
```

`name` is the snapshot's path **relative to the backup's `destination`**, so a nested
`namePattern` lists `daily/data.db.20260801T231502Z` — the exact string `restore --at` accepts.
`created` is the ISO 8601 UTC time the run that wrote it finished, on the same scale as
`backup list`'s Last Run column.

The list comes from the recorded runs, not a directory glob, so it agrees with what
[retention](#retention) prunes. Two records never appear: one whose path is not inside the
backup's current `destination` (the same gate pruning uses — a stale or foreign row can never be
offered as a restore source), and one whose payload is no longer on disk. A snapshot you could
not restore is not listed as one.

`--snapshots` requires a backup name; a bare `cfgd backup list --snapshots` is a usage error.

**Next Run** is computed the same way the daemon seeds its timer — from the unit's `schedule` and
its last recorded `finished_at` (see [`schedule`](#schedule)) — so the listed time is the one the
timer will actually use, not a second opinion. It renders as an ISO 8601 UTC stamp on the same
scale as Last Run. A schedule-less unit shows `-` (`nextRunAt` omitted from the JSON payload): it
runs during `cfgd apply`, on no clock of its own. An overdue interval unit shows a time at or
before now — the daemon fires it as soon as it comes back. Reading it does not require a running
daemon; without one, it is what the timer *would* be armed to.

## Field Reference

The authoritative table lives in the [Profile spec](spec/profile.md#specbackups). The semantics
each field implies are below.

### `source`

A file or a directory. A leading `~` expands to the home directory.

- **File source** → the snapshot is a file at `<destination>/<rendered-name>`.
- **Directory source** → the snapshot is a directory at `<destination>/<rendered-name>/`, copied
  recursively. Symlinks inside the tree are **skipped**, so a snapshot can never follow a link out
  of the source tree.

A source that does not exist is a failed run, not a silent no-op.

The source's filename is what the default `namePattern` interpolates as `{filename}`, so a legal
Unix filename containing `:` (`~/notes:2026.md`) renders a snapshot name cfgd refuses — `:` is a
drive and data-stream separator on Windows, and snapshot names must be valid on every platform.
cfgd does not rewrite the character; give the backup an explicit `namePattern` that leaves
`{filename}` out:

```yaml
- name: notes
  source: ~/notes:2026.md
  namePattern: "notes.{timestamp}"   # default would render "notes:2026.md.{timestamp}"
```

### `destination`

Where snapshots are written. Defaults to `<state_dir>/backups/<name>/` — see
[configuration.md](configuration.md#file-locations) for where the state dir lands on each
platform. Set it explicitly to put snapshots on another disk:

```yaml
backups:
  - name: photos
    source: ~/Pictures
    destination: /mnt/nas/backups/photos
```

Snapshot payloads never go into the state database, and there is no size cap.

**The destination must live outside the source.** A destination inside the source tree would make
each snapshot part of the next one, without end, so cfgd rejects it before copying anything:

```yaml
# rejected — every snapshot would be copied into the following snapshot
- name: photos
  source: ~/Pictures
  destination: ~/Pictures/backups
```

A `namePattern` that renders to the source's own path is rejected for the same reason: taking the
snapshot would destroy the data being backed up.

The check resolves symlinks on both sides, so a destination that only *looks* separate is caught
too:

```yaml
# also rejected — ~/link is a symlink to ~/Pictures, so the destination is
# physically inside the source even though the two paths share no prefix
- name: photos
  source: ~/Pictures
  destination: ~/link/backups
```

### Permissions

On Unix, snapshots carry the source's modes: file modes come across with the copy, and each copied
directory is set to the mode of the directory it came from — a `0700` tree does not land as a
`0755` one. Windows has no mode bits; a snapshot there inherits the destination's ACL.

The **default** destination (`<state_dir>/backups/<name>/`) is additionally set to `0700`, because
cfgd owns it and it may hold a copy of something like `~/.ssh`. An **explicit** `destination:` is
your directory and keeps whatever permissions you gave it — set them yourself if the source is
sensitive.

### `namePattern`

The filename each snapshot gets, defaulting to `{filename}.{timestamp}`.

| Variable | Expands to |
|---|---|
| `{name}` | the backup's `name` |
| `{filename}` | the final component of `source` |
| `{timestamp}` | UTC in `%Y%m%dT%H%M%SZ` form, e.g. `20260801T031500Z` |

```yaml
namePattern: "{name}-{timestamp}.snap"   # openlist-db-20260801T031500Z.snap
namePattern: "daily/{filename}"          # nests snapshots in a subdirectory
```

An unknown `{var}` is rejected when the config is parsed. At run time the rendered value must be a
relative path whose every segment names something: an empty string, a rooted path (`/daily`,
`C:/daily`, `C:daily`, `\\server\share`), an empty segment (`a//b`, `daily/`), a `:` anywhere (a
drive or NTFS data-stream separator), and `.` or `..` segments are all rejected. Windows shapes are
rejected on Linux and macOS too, so a pattern is valid on every host or on none. A snapshot always
lands inside `destination`, and a pattern can never resolve to a directory that already exists and
holds other snapshots.

Pruning removes intermediate directories a nested pattern created once they hold nothing else.

Two runs that render the same name (a pattern with no `{timestamp}`, or two runs inside one
second) resolve as "newest wins": the older snapshot is replaced.

### `retention`

How many snapshots to keep, defaulting to 10 and required to be at least 1. Pruning walks the
recorded runs — not a filename glob — and deletes both the artifact on disk and its record.

Retention is counted **per outcome**: the newest `retention` runs that produced a snapshot are
kept, and independently the newest `retention` that did not. A run of failures therefore never
deletes a good snapshot, and a permanently broken backup cannot grow the run table without bound.

Pruning deletes nothing that is not demonstrably inside the current `destination`. A record naming
a path anywhere else — you changed `destination:` between runs, another profile declares a backup
with the same `name`, or the state database was edited — is dropped from the history with a warning
and its path is left untouched for you to deal with. Retention slots are not consumed by such
records either, so a stale one cannot evict a snapshot you asked to keep.

### `schedule`

A duration (`6h`, `30m`, `1d`) or a cron expression, 5-field (`0 3 * * *`) or 6-field with leading
seconds (`30 0 3 * * *`). Omitted means the backup runs on every apply.

Setting it hands the backup to the [daemon's timers](#daemon-scheduling) and takes it out of
apply. A cron expression is read in the machine's **local** timezone, the same as a crontab entry:
`0 3 * * *` is 3am where the machine sits, not 3am UTC.

A duration is a plain period between runs, with no alignment to the wall clock — use cron when the
run has to land at a particular time of day. The period is measured from the unit's **last recorded
run**, not from the daemon's start, so it survives restarts: a `schedule: 1d` backup on a laptop
rebooted every morning still fires once a day, and a unit whose period elapsed while the machine
was off runs shortly after the daemon comes back. With no recorded run yet, the first fire is one
full period out.

```yaml
backups:
  - name: openlist-db
    source: /var/lib/openlist/data.db
    schedule: "0 3 * * *"    # 3am local, daily
  - name: scratch
    source: ~/scratch
    schedule: 6h             # every six hours, measured from the last run
  - name: pre-apply
    source: ~/.ssh
                             # no schedule → runs during `cfgd apply`
```

### `preBackup` / `postBackup`

Hooks in the same shape as [`spec.scripts`](lifecycle-scripts.md) entries — `run`, `shell`,
`timeout`, `workdir`, `onlyIf`, `unless`, `creates`, `continueOnError` all apply. Relative script
paths resolve against the config directory, and hooks see the usual metadata:

| Variable | Value |
|---|---|
| `CFGD_CONFIG_DIR` | the config directory |
| `CFGD_PROFILE` | the active profile |
| `CFGD_CONTEXT` | `apply` or `reconcile` |
| `CFGD_PHASE` | `preBackup` or `postBackup` |

## Run Semantics

```
preBackup hooks
      │
      ├──fail──►  copy SKIPPED  ──┐
      │ ok                        │
      ▼                           │
copy source → destination/<rendered-name>   (atomic: temp + rename)
      │                           │
      ▼                           ▼
postBackup hooks     ← always attempted, on every path above
      │
      ▼
record the run  ──►  prune to `retention`
```

Three rules follow from that ordering:

1. **A `preBackup` failure skips the snapshot.** The hook exists to make the source consistent
   (stop the service, flush the buffer); if it failed, the source is not in the state you asked
   for, so copying it would produce a snapshot you cannot trust. The run is recorded as failed with
   no artifact.
2. **`postBackup` always runs** — after a good copy, after a failed copy, and after a failed
   `preBackup`. It is normally the counterpart that restarts whatever `preBackup` stopped, and a
   `preBackup` list that failed halfway (service already down, flush failed) is precisely when
   skipping it would leave the machine stopped with nothing to bring it back.
3. **A `postBackup` failure after a good copy leaves the run successful, with the failure
   recorded.** The snapshot is complete and restorable, so it stays retention-eligible; marking the
   run failed would strand a valid artifact that pruning could never reclaim. The failure is still
   surfaced — the run is not *clean*, and the command reporting it says so.

Every failure in a run reaches the record: a `preBackup`, copy, and `postBackup` failure in the
same run are joined with `; ` in the run's error.

Copies are atomic where the filesystem allows: a file streams into a sibling temp file that is
fsynced and renamed into place, and a directory is built under a `.<name>.partial` staging
directory published with a single rename. The destination directory is fsynced after the rename on
Unix, so a completed snapshot survives a power loss. An interrupted run never leaves a half-written
snapshot under a name a restore would trust.

Every run — success or failure — is recorded in the `backup_runs` table of the state database with
its source, destination, size, status, error, and start/finish timestamps.

**One run at a time per backup, enforced.** Each run takes an exclusive lock on its own unit
(`<state-dir>/locks/backup-<name>.lock`) for the whole run, hooks included. Two runs of one unit
would otherwise share a staging directory and prune against the same history, and the loser's
cleanup would land inside the winner's half-copied tree — a torn snapshot recorded as a success.

The lock is per unit, not global: different backups still run at the same time. It is held by
*every* surface with no opt-out, so a `cfgd backup run` you type while the daemon's timer for that
same unit is firing is refused rather than interleaved:

```console
$ cfgd backup run openlist-db
Run Backups

— backup 'openlist-db' — already running (pid 4127)
$ echo $?
1
```

Every surface renders the collision the same way — a **skip**, because the unit *is* being backed
up, just not by the caller. Only the exit code differs: `cfgd backup run` exits `1` (you asked for a
run and did not get one), while `cfgd apply` and the daemon's timer carry on unaffected. Under
`-o json` the unit appears in the payload with `"status": "skipped"`, and that payload stays a
single JSON document — the nonzero exit carries the failure, not a second error object.

## Daemon scheduling

A backup with a `schedule` gets a timer in the [daemon](daemon.md) alongside the reconcile and sync
tasks. Nothing else changes: the timer dispatches the same engine `cfgd backup run` does, so a
scheduled run writes the same `backup_runs` row, runs the same hooks, and prunes to the same
`retention`. Only `CFGD_CONTEXT` differs — it is `reconcile` for a daemon-driven run and `apply`
for a CLI-driven one.

```console
$ cfgd daemon
Daemon

Starting cfgd daemon...

✓ Health: /run/user/1000/cfgd/cfgd.sock

✓ Intervals: reconcile=300s, backups=2 scheduled

Daemon running — press Ctrl+C to stop
 INFO scheduled backup tick backup=openlist-db

✓ backup 'openlist-db'
```

Timer behaviour:

- **Only scheduled backups get timers.** A schedule-less entry belongs to `cfgd apply` and is never
  installed as a timer.
- **The set reloads on `SIGHUP`** — see [Live config reload](daemon.md#live-config-reload-sighup).
  Added, removed, and rescheduled units are picked up without a restart, and a unit whose schedule
  did not change keeps its pending deadline, so reloading does not restart the clock on a daily
  backup.

  ```console
  $ kill -HUP "$(cfgd daemon status -o json | jq .pid)"
  # in the daemon's output:
  Reloading configuration (SIGHUP) — timer intervals and backup schedules only; other fields require restart

  ✓ Backup schedules reloaded: 1 added, 1 removed, 1 rescheduled
  ```

  The swap is all-or-nothing. A reload that cannot fully resolve the config — a profile saved
  mid-edit, a source cache being rewritten — keeps the schedules that are already running and
  retries on its own, so one `SIGHUP` over a transient error can never retire a working timer set:

  ```console
  ⚠ Backup schedules NOT reloaded: config did not fully resolve — keeping the 2 running schedule(s), retrying automatically
  ```
- **A degraded start is visible and temporary.** If sources cannot be composed at startup, the
  daemon installs the locally-declared backups rather than none, says so in the banner, holds their
  first fire back until it has re-resolved, and keeps retrying:

  ```console
  ✓ Intervals: reconcile=300s, backups=2 scheduled (source composition unavailable)
  ```

  If the profile itself will not resolve there are no timers to install at all — but the retry is
  armed just the same, and the banner names that cause rather than blaming the sources:

  ```console
  ✓ Intervals: reconcile=300s, backups=0 scheduled (profile unresolved)
  ```

  Either way the daemon recovers on its own, with no restart and no manual `SIGHUP`:

  ```console
  ✓ Backup schedules restored: 3 scheduled
  ```

  A profile that heals into zero declared backups is not a restoration of anything, so it gets its
  own line rather than the odd-looking `restored: 0 scheduled`:

  ```console
  ✓ Backup schedule resolved: no units configured
  ```

  A recovery that is only *partial* — the profile parses again but sources are still unavailable —
  says so on the same line rather than reporting an all-clear, because the retry is still armed and
  a unit a source overrides would back up to its **local** destination once the first-fire deferral
  expires:

  ```console
  ⚠ Backup schedules restored: 3 scheduled (source composition unavailable)
  ```

  The `SIGHUP` completion line carries the same qualifier when a reload adopts a partial set.
- **A unit never overlaps itself.** The daemon's loop runs one tick at a time and waits for a run to
  finish, so a unit's next fire is not even evaluated while its own run is in flight. Fires that
  elapse during a long run are **skipped**, not queued: cfgd logs how many were passed over and arms
  the next one from now. A backup that consistently takes longer than its own schedule therefore
  runs back-to-back rather than piling up.
- **A failed run does not stop the timer.** The failure is recorded like any other, reported on the
  daemon's output, and the unit is re-armed for its next fire.
- **Shutdown is not held hostage by a hook.** `SIGTERM` / Ctrl-C reaches an in-flight `preBackup` or
  `postBackup` hook, so a `systemctl stop cfgd` during a backup does not wait out the hook's own
  timeout.

## Restoring

`cfgd backup restore <name>` puts a snapshot back where it came from. With no `--at` it picks the
**newest** snapshot; the confirmation names the snapshot and the path it is about to overwrite:

```console
$ cfgd backup restore openlist-db
Restore Backup

Restore 'openlist-db' from snapshot data.db.20260801T231502Z into /var/lib/openlist/data.db? [y/N] y

✓ backup 'openlist-db' restored from data.db.20260801T231502Z — into /var/lib/openlist/data.db

→ previous contents saved to /home/me/.local/state/cfgd/backups/openlist-db/data.db.20260802T044026Z
```

```bash
cfgd backup restore openlist-db                              # newest snapshot
cfgd backup restore openlist-db --at 20260730T120000Z        # by the timestamp portion
cfgd backup restore openlist-db --at data.db.20260730T120000Z  # or the full snapshot name
cfgd backup restore openlist-db --to /tmp/inspect --yes      # somewhere else, no prompt
```

`--at` matches the full snapshot name first, then any snapshot name **containing** the value —
which is what lets a bare timestamp reach `data.db.20260730T120000Z` without you knowing the
unit's `namePattern`. A value matching more than one snapshot is refused rather than resolved to
the newest match: a restore overwrites live data, so an ambiguous selection is never guessed at.
An unknown value lists every available snapshot and exits `6`, the same treatment an unknown
backup name gets.

**Confirmation is required.** `--yes` (or `CFGD_YES=1`) skips the prompt. Where cfgd *cannot*
prompt — piped stdin, a CI runner, or `-o json` — a restore without `--yes` is an **error**, not a
silent "aborted": you asked for a restore and did not get one.

### What a restore does

```
acquire the unit's lock
      │
      ▼
stage the selected snapshot into a temp dir beside the target
      │                                  (before the safety backup — see below)
      ▼
safety backup of the CURRENT target      (skipped with --to, or if the source is gone)
      │
      ▼
preBackup hooks
      │
      ├──fail──►  overlay SKIPPED  ──┐
      │ ok                           │
      ▼                              │
overlay the staged snapshot onto the target
      │                              │
      ▼                              ▼
postBackup hooks     ← always attempted, on every path above
      │
      ▼
staging removed      ← on every path, success or failure
```

- **Overlay, not mirror.** Every file the snapshot holds overwrites its counterpart; files present
  only in the target are **left alone**. A restore therefore never deletes anything you added
  since the snapshot was taken. Use `--to` and copy by hand if you want an exact mirror.
- **File modes come across** on Unix, the same way the backup carried them in. Symlinks are absent
  from every snapshot by construction (the writer skips them), so a symlink living in the target
  has nothing to overwrite it and survives untouched.
- **The safety backup is an ordinary run.** It writes a normal `backup_runs` row, runs the unit's
  hooks, and **participates in normal retention** — so it counts against `retention` and can evict
  an older snapshot. Its path is reported as `safetySnapshot` in `-o json` and as the `→` line in
  human output. If it fails to produce a snapshot, the restore is **abandoned**: cfgd will not
  overwrite data whose current contents were not captured.
- **Staging comes first for a reason.** The safety backup prunes to `retention`, and the snapshot
  being restored can be the one it evicts. Staging the payload beforehand makes the restore immune
  to that. One visible consequence: a restore run inside the same second as the snapshot it
  selects makes the safety backup render the *same* name (`namePattern` stamps to the second) and
  replace it under the [newest-wins](#namepattern) rule. The restore is unaffected and cfgd warns;
  give the unit a finer `namePattern` if you need both.
- **The unit's `preBackup` / `postBackup` hooks run twice** on a restore-to-source — once around
  the safety backup, once around the restore itself — because both mutate the same data and both
  need the source quiesced. With `--to` there is no safety backup, so they run once.
- **A `preBackup` failure skips the overlay**, exactly as it skips the snapshot during a run: the
  hook exists to quiesce the target, and overwriting it after the hook failed is what the hook
  existed to prevent. `postBackup` still runs.
- **One at a time.** A restore takes the same per-unit lock a run does, so it can never interleave
  with a scheduled fire or an apply of the same backup.

### What a restore refuses

| Refused | Why |
|---|---|
| `--to` inside the backup's `destination` | restoring there would overwrite the snapshot store |
| a snapshot/target kind mismatch (file over directory, or the reverse) | publishing a file over a directory would delete the whole directory on the way to the rename |
| a snapshot that vanished since it was listed | a concurrent prune, or a hand-deleted destination |
| a failed safety backup | the current contents were not captured |

**Restores are not recorded.** The `backup_runs` table is the ledger retention walks, and a
restore produces no artifact for it to prune. The safety backup it takes *is* recorded, as an
ordinary run. `cfgd rollback` covers cfgd's own file writes and is unrelated to this table.

### Restoring by hand

A snapshot is an ordinary file or directory, so nothing stops you from doing it yourself — which
is the right call when you want mirror semantics (`rsync --delete`) rather than an overlay:

```bash
# file snapshot
sudo systemctl stop openlist
sudo cp ~/.local/state/cfgd/backups/openlist-db/data.db.20260801T031500Z /var/lib/openlist/data.db
sudo systemctl start openlist

# directory snapshot
rsync -a --delete ~/.local/state/cfgd/backups/photos/Pictures.20260801T031500Z/ ~/Pictures/
```

## Limitations

- A missed **cron** occurrence is skipped, not caught up: a daemon that was stopped over a `0 3 * * *`
  fire takes the next 3am, not the one it slept through. (Interval schedules do resume from the last
  recorded run — see [`schedule`](#schedule).)
- Snapshots are full copies — no incremental, deduplicating, or compressed modes.
- Symlinks inside a directory source are skipped rather than recreated.
- Concurrent runs of one backup are refused, not queued: the second caller is told who holds the
  unit (see above).
- `cfgd backup restore` overlays; it never deletes files the snapshot does not contain. Restore
  with `--to` and mirror by hand when you need the target to match the snapshot exactly.
- `spec.backups[]` is available on the YAML/TOML profile path only; CRD parity is not implemented.
