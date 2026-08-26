# Declarative Backups

`spec.backups[]` declares snapshots of a file or directory that cfgd takes on your behalf,
retaining the newest N and pruning the rest. It is the "keep a copy of my app's data before I
touch it" surface, distinct from the automatic pre-overwrite `file_backups` that power
`cfgd rollback` (see [File Safety](safety.md#file-backups)).

| | `spec.backups[]` (this document) | Pre-overwrite backups |
|---|---|---|
| Declared by | you, in a profile | nobody — automatic |
| Covers | any file or directory on the machine | only files cfgd is about to write |
| Stored | on the filesystem, under `destination` | inline in the state DB |
| Retained | newest `retention` per backup | last 10 applies |
| Restored by | `cfgd backup restore` | `cfgd rollback` |

![declare, snapshot, tamper, restore](../demo/cfgd-backup.gif)
A snapshot taken with `cfgd backup run`, a file broken from outside cfgd, and `cfgd backup
restore` putting it back.

## Quick Start

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: workstation
spec:
  backups:
    - name: notes-db
      source: ~/.local/share/notes/notes.db
      retention: 7
      preBackup:
        - sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
      postBackup:
        - sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
```

That declaration snapshots `notes.db` into `<state_dir>/backups/notes-db/` as
`notes.db.20260813T061306Z`, keeps the newest 7, and folds the write-ahead log into the database
before the copy so the snapshot is a consistent point in time rather than half of one.

## CLI

A schedule-less backup (no `schedule`) also runs automatically during `cfgd apply`, after the
reconciler's file/package/module phases (skipped in `--dry-run`, shown in the plan preview
instead). A scheduled backup runs on its own timer in the [daemon](#daemon-scheduling), or on
demand via `cfgd backup run`.

Each schedule-less backup runs independently during apply. A unit that fails to complete (source
missing, a hook errored, or a state-store write failure) is reported as a `✗`/`Warn` status and
counted against the exit code, but does **not** abort the remaining backups or the rest of apply.
A failed or unclean unit downgrades the apply's overall status from `success` to `partial`, which
exits nonzero (code `7`) the same way a failed reconciler action would; see
[Exit Codes](cli-reference.md#exit-codes).

A backup run is a run like any other: a `Backup` header, one `backup:<name>` group per unit, and a
rollup. The groups sit directly under the header (the run has no other phase to tell them apart
from; inside `cfgd apply` the same groups render under a `Backups` phase beside `Packages` and
`Files`). Each unit's group carries one line per `preBackup` / `postBackup` hook and one for the
snapshot itself, so the rollup's counts are the lines on screen.

```console
$ cfgd backup run
Backup
  Config   /home/me/.config/cfgd/cfgd.yaml
  Profile  workstation
  Actions  4 planned

backup:notes-db
  ◐ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
    0|0|0
  ✓ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)" (0.1s)
  ◐ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
    ok
  ✓ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"             (0.1s)
  ✓ snapshot notes.db.20260813T061306Z                                                 — 8.0 KB
backup:journal
  ✓ snapshot journal.20260813T061306Z                                                  — 24 B

✓ Backup complete — 4 actions succeeded (0.2s wall)

$ cfgd backup run missing-name
✗ Backup 'missing-name' not found

→ valid backups: notes-db, journal

$ cfgd backup list
Backups

Name      Source                         Schedule   Retention  Snapshots  Status   Last Run  Next Run
──────────────────────────────────────────────────────────────────────────────────────────────────────────
notes-db  ~/.local/share/notes/notes.db  -          7          1          Success  4h ago    -
journal   ~/Documents/journal            0 3 * * *  3          1          Success  4h ago    in 11h

$ cfgd --output json backup run notes-db
[
  {
    "clean": true,
    "destinationPath": "/home/me/.local/state/cfgd/backups/notes-db/notes.db.20260813T061322Z",
    "name": "notes-db",
    "status": "success"
  }
]

$ cfgd --output json backup run missing-name
{
  "error": "not_found",
  "hint": "valid backups: notes-db, journal",
  "name": "missing-name"
}
```

`cfgd backup run [name]` runs every declared backup when `name` is omitted, or the named one.
An unknown name is a typed error (exit code `6`, see [Exit Codes](cli-reference.md#exit-codes))
that lists every valid name: in human mode as a `→` hint line below the failure, in `-o json`
as the payload's `hint` field. A run whose snapshot did not complete cleanly (see
[Run Semantics](#run-semantics)) also exits nonzero, so a script can detect it without
parsing output.

`cfgd backup list [name]` (alias `ls`) shows every declared backup (or the named one), how
many snapshots it currently holds (`snapshots` in `-o json`), its last recorded run, and when the
daemon's timer will next fire it (`nextRunAt` in `-o json`). Every backup command honors the global
`-o`/`--output` flag for `json`/`yaml`/`jsonpath`/`template` consumers. The Snapshots column reads
`-` when the state store could not be opened, the same degradation the Status and Last Run
columns take: an unknown count is not a count of zero.

`Status` and `Last Run` are two columns, the way `source list` splits them: the verdict is
tinted by what it says, and the age beside it answers how stale the unit is. `Next Run`
counts forward the same way (`in 11h`, `due now`). All three read as relative time on
purpose — `-o json` keeps the exact instants in `lastRunAt` and `nextRunAt`.

The count includes the safety snapshots [`cfgd backup restore`](#restoring) takes, because they
occupy the destination and count against `retention` like any other. When there is at least one,
the column says so (`2 (1 safety)`); `-o json` keeps `snapshots` as the total and adds
`safetySnapshots` for the share of it a restore wrote. **Last Run and Next Run read backups only.**
A safety snapshot is a side effect of putting data back, never a backup of the unit, so it is not
the unit's Last Run and does not re-anchor its schedule: restore a unit at noon and an hourly
schedule still fires on the last real run's clock.

`cfgd backup list <name> --snapshots` switches the view from the backup to its snapshots, the
ones [`cfgd backup restore`](#restoring) can put back:

```console
$ cfgd backup list notes-db --snapshots
Snapshots: notes-db

Snapshot                   Kind    Created  Size
────────────────────────────────────────────────
notes.db.20260813T061322Z  Safety  4h ago   8.0 KB
notes.db.20260813T061321Z  Run     4h ago   8.0 KB
notes.db.20260813T061306Z  Run     4h ago   8.0 KB

$ cfgd --output json backup list notes-db --snapshots
[
  {
    "created": "2026-08-13T06:13:22Z",
    "kind": "safety",
    "name": "notes.db.20260813T061322Z",
    "sizeBytes": 8192
  }
]
```

`name` is the snapshot's path **relative to the backup's `destination`**, so a nested
`namePattern` lists `daily/notes.db.20260813T061322Z`: the exact string `restore --at` accepts.
`Created` is the age of the run that wrote it, on the same scale as `backup list`'s Last Run
column; `-o json`'s `created` keeps the ISO 8601 UTC instant. `Kind` reads `Run` for a backup of
the unit and `Safety` for the copy
[`cfgd backup restore`](#restoring) took of what it was about to overwrite; both restore, and both
count against retention, but only a `Run` is the unit's Last Run (`-o json`'s `kind` carries the
stored `run` / `safety` token). The `Size` column uses the same `1.2 MB` / `4.0 KB` / `12 B`
scale `cfgd upgrade` prints; `-o json` reports raw bytes in `sizeBytes` and leaves formatting
to you.

The list comes from the recorded runs, not a directory glob, so it agrees with what
[retention](#retention) prunes. Two records never appear: one whose path is not inside the
backup's current `destination` (the same gate pruning uses), and one whose payload is no
longer on disk. A snapshot you could not restore is not listed as one.

`--snapshots` requires a backup name; a bare `cfgd backup list --snapshots` is a usage error.

**Next Run** is computed the same way the daemon seeds its timer, from the unit's `schedule` and
its last recorded `finished_at` (see [`schedule`](#schedule)), so the listed time is the one the
timer will actually use. A schedule-less unit shows `-` (`nextRunAt` omitted from the JSON
payload): it runs during `cfgd apply`, on no clock of its own. An overdue interval unit shows a
time at or before now; the daemon fires it as soon as it comes back. Reading it does not require
a running daemon; without one, it is what the timer *would* be armed to.

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
Unix filename containing `:` (`~/notes:2026.md`) renders a snapshot name cfgd refuses: `:` is a
drive and data-stream separator on Windows, and snapshot names must be valid on every platform.
cfgd does not rewrite the character; give the backup an explicit `namePattern` that leaves
`{filename}` out:

```yaml
- name: notes
  source: ~/notes:2026.md
  namePattern: "notes.{timestamp}"   # default would render "notes:2026.md.{timestamp}"
```

### `destination`

Where snapshots are written. Defaults to `<state_dir>/backups/<name>/`; see
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
# rejected: every snapshot would be copied into the following snapshot
- name: photos
  source: ~/Pictures
  destination: ~/Pictures/backups
```

A `namePattern` that renders to the source's own path is rejected for the same reason: taking the
snapshot would destroy the data being backed up.

The check resolves symlinks on both sides, so a destination that only *looks* separate is caught
too:

```yaml
# also rejected: ~/link is a symlink to ~/Pictures, so the destination is
# physically inside the source even though the two paths share no prefix
- name: photos
  source: ~/Pictures
  destination: ~/link/backups
```

### Permissions

On Unix, snapshots carry the source's modes: file modes come across with the copy, and each copied
directory is set to the mode of the directory it came from, so a `0700` tree does not land as a
`0755` one. Windows has no mode bits; a snapshot there inherits the destination's ACL.

The **default** destination (`<state_dir>/backups/<name>/`) is additionally set to `0700`, because
cfgd owns it and it may hold a copy of something like `~/.ssh`. An **explicit** `destination:` is
your directory and keeps whatever permissions you gave it; set them yourself if the source is
sensitive.

### `namePattern`

The filename each snapshot gets, defaulting to `{filename}.{timestamp}`.

| Variable | Expands to |
|---|---|
| `{name}` | the backup's `name` |
| `{filename}` | the final component of `source` |
| `{timestamp}` | UTC in `%Y%m%dT%H%M%SZ` form, e.g. `20260801T031500Z` |

```yaml
namePattern: "{name}-{timestamp}.snap"   # notes-db-20260813T061306Z.snap
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
second) take **distinct** names: cfgd appends `-1`, `-2`, and so on until the path is free.

```console
$ cfgd backup list journal --snapshots
Snapshots: journal

Snapshot                    Created  Size
─────────────────────────────────────────
journal.20260813T061710Z-1  4h ago   24 B
journal.20260813T061710Z    4h ago   24 B
journal.20260813T061547Z    4h ago   24 B
```

Nothing is overwritten: each recorded run owns exactly one payload, and both count against
`retention` normally.

### `retention`

How many snapshots to keep. Default 10, minimum 1.

| Rule | Behavior |
|---|---|
| What pruning walks | the recorded runs, not a filename glob; deletes both the artifact on disk and its record |
| Counted per outcome | the newest `retention` runs that produced a snapshot are kept, and independently the newest `retention` that did not; a run of failures never deletes a good snapshot |
| Paths outside `destination` | a record naming a path outside the backup's current `destination` (you changed `destination:` between runs, or the state database was edited) is dropped from history with a warning; the path itself is left untouched, and the record consumes no retention slot |

### `schedule`

Setting `schedule` hands the backup to the [daemon's timers](#daemon-scheduling) and takes it
out of apply.

| Form | Example | Meaning |
|---|---|---|
| Duration | `6h`, `30m`, `1d` | a plain period between runs, measured from the last recorded run; no wall-clock alignment |
| Cron, 5-field | `0 3 * * *` | machine-**local** timezone, same as a crontab entry: 3am where the machine sits, not 3am UTC |
| Cron, 6-field | `30 0 3 * * *` | leading seconds field |
| Omitted | | the backup runs on every `cfgd apply` |

A duration is measured from the unit's **last recorded run**, not from the daemon's start, so it
survives restarts: a `schedule: 1d` backup on a laptop rebooted every morning still fires once a
day, and a unit whose period elapsed while the machine was off runs shortly after the daemon
comes back. With no recorded run yet, the first fire is one full period out. Use cron when the
run has to land at a particular time of day.

```yaml
backups:
  - name: notes-db
    source: ~/.local/share/notes/notes.db
    schedule: "0 3 * * *"    # 3am local, daily
  - name: scratch
    source: ~/scratch
    schedule: 6h             # every six hours, measured from the last run
  - name: pre-apply
    source: ~/.ssh
                             # no schedule → runs during `cfgd apply`
```

### `preBackup` / `postBackup`

Hooks in the same shape as [`spec.scripts`](lifecycle-scripts.md) entries: `run`, `shell`,
`timeout`, `workdir`, `onlyIf`, `unless`, `creates`, `continueOnError` all apply. Relative script
paths resolve against the config directory, and hooks see the usual metadata:

| Variable | Value |
|---|---|
| `CFGD_CONFIG_DIR` | the config directory |
| `CFGD_PROFILE` | the active profile |
| `CFGD_CONTEXT` | `apply` or `reconcile` |
| `CFGD_PHASE` | `preBackup` or `postBackup` |
| `CFGD_OPERATION` | `backup` or `restore` |

One hook list serves both directions. `CFGD_PHASE` names the list, not the direction (a
`preBackup` hook runs before a snapshot AND before a [restore](#restoring)), so a hook that has to
quiesce for one and drop-and-recreate for the other branches on `CFGD_OPERATION`:

```yaml
preBackup:
  - run: |
      sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
      [ "$CFGD_OPERATION" = restore ] && rm -f ~/.local/share/notes/notes.db-wal
```

Snapshots do not pause a concurrently running `cfgd apply`: each file is copied atomically, but a
multi-file source captured while an apply is rewriting it can mix pre- and post-apply contents
across files. If a source needs a point-in-time-consistent snapshot, quiesce its writer in
`preBackup` and restart it in `postBackup`.

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
   (stop the service, flush the buffer); if it failed, copying would produce a snapshot you
   cannot trust. The run is recorded as failed with no artifact.
2. **`postBackup` always runs**: after a good copy, after a failed copy, and after a failed
   `preBackup`. It is normally the counterpart that restarts whatever `preBackup` stopped, and a
   `preBackup` list that failed halfway is precisely when skipping it would leave the machine
   stopped with nothing to bring it back.
3. **A `postBackup` failure after a good copy leaves the run successful, with the failure
   recorded.** The snapshot is complete and restorable, so it stays retention-eligible. The
   failure is still surfaced: the run is not *clean*, and the command reporting it says so.

Every failure in a run reaches the record: a `preBackup`, copy, and `postBackup` failure in the
same run are joined with `; ` in the run's error.

Copies are atomic where the filesystem allows: a file streams into a sibling temp file that is
fsynced and renamed into place, and a directory is built under a `.<name>.partial` staging
directory published with a single rename. The destination directory is fsynced after the rename on
Unix, so a completed snapshot survives a power loss. An interrupted run never leaves a half-written
snapshot under a name a restore would trust.

Every run, success or failure, is recorded in the `backup_runs` table of the state database with
its source, destination, size, status, error, and start/finish timestamps.

**One run at a time per backup, enforced.** Each run takes an exclusive lock on its own unit
(`<state-dir>/locks/backup-<name>.lock`) for the whole run, hooks included; two interleaved runs
of one unit could otherwise record a torn snapshot as a success.

The lock is per unit, not global: different backups still run at the same time. It is held by
*every* surface with no opt-out, so a `cfgd backup run` you type while the daemon's timer for that
same unit is firing is refused rather than interleaved:

```console
$ cfgd backup run notes-db
Backup
  Config   /home/me/.config/cfgd/cfgd.yaml
  Profile  workstation
  Actions  3 planned

backup:notes-db
  — snapshot                           — already running (pid 3349308)

— Backup did not run — 3 actions not attempted (<0.1s wall)
$ echo $?
1
```

Nothing the unit planned ran, so its items are `not attempted` rather than failed, and the
rollup says the run *did not run* rather than claiming it completed.

Every surface renders the collision as a **skip**, because the unit *is* being backed up, only
not by the caller. The exit code differs: `cfgd backup run` exits `1` (you asked for a run and
did not get one), while `cfgd apply` and the daemon's timer carry on unaffected. Under `-o json`
the unit appears in the payload with `"status": "skipped"`; the nonzero exit carries the
failure, not a second error object.

## Daemon scheduling

A backup with a `schedule` gets a timer in the [daemon](daemon.md) alongside the reconcile and sync
tasks. Nothing else changes: the timer dispatches the same engine `cfgd backup run` does, so a
scheduled run writes the same `backup_runs` row, runs the same hooks, and prunes to the same
`retention`. Only `CFGD_CONTEXT` differs: `reconcile` for a daemon-driven run, `apply`
for a CLI-driven one.

```console
$ cfgd daemon
09:00:00  INFO daemon: starting cfgd 0.9.0
09:00:00  INFO daemon: health endpoint at /home/me/.cache/cfgd/runtime/cfgd.sock
09:00:00  INFO daemon: running — reconcile every 300s, 2 scheduled backups
→ Press Ctrl+C to stop

Backup
  Config   /home/me/.config/cfgd/cfgd.yaml
  Profile  workstation
  Trigger  schedule
  Actions  3 planned

backup:notes-db
  ◐ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
    0|0|0
  ✓ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)" (0.1s)
  ◐ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
    ok
  ✓ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"             (0.1s)
  ✓ snapshot notes.db.20260813T061559Z                                                 — 8.0 KB

✓ Backup complete — 3 actions succeeded (0.2s wall)
09:05:01  INFO daemon: scheduled backup notes-db completed
```

A scheduled fire renders the same group a hand-run does, so the journal a background run leaves
behind is what you would have seen on the terminal. Each unit also gets one `tracing` line naming
its own outcome: `completed`, `completed with errors`, or
`skipped — already running under <holder>`.

Timer behaviour:

- **Only scheduled backups get timers.** A schedule-less entry belongs to `cfgd apply` and is never
  installed as a timer.
- **The set reloads on `SIGHUP`.** Added, removed, and rescheduled units are picked up without a
  restart, and a unit whose schedule did not change keeps its pending deadline, so reloading does
  not restart the clock on a daily backup. The swap is all-or-nothing: a reload that cannot fully
  resolve the config keeps the schedules already running and retries on its own. See
  [Live config reload](daemon.md#live-config-reload-sighup) for the reload messages.
- **A degraded start is visible and temporary.** If sources cannot be composed at startup, the
  daemon installs the locally-declared backups rather than none, says so in the banner, holds
  their first fire back until it has re-resolved, and keeps retrying. If the profile itself will
  not resolve, no timers are installed, and the banner names that cause instead:

  ```console
  ✓ Intervals: reconcile=300s, backups=2 scheduled (source composition unavailable)
  ✓ Intervals: reconcile=300s, backups=0 scheduled (profile unresolved)
  ```

  Either way the daemon recovers on its own, with no restart and no manual `SIGHUP`, and reports
  the recovery. A partial recovery (the profile parses again but sources are still unavailable)
  keeps the qualifier rather than reporting an all-clear:

  ```console
  ✓ Backup schedules restored: 3 scheduled
  ⚠ Backup schedules restored: 3 scheduled (source composition unavailable)
  ```
- **A unit never overlaps itself.** A unit's next fire is not evaluated while its own run is in
  flight. Fires that elapse during a long run are **skipped**, not queued: cfgd logs how many were
  passed over and arms the next one from now, so a backup that consistently outruns its own
  schedule runs back-to-back rather than piling up.
- **A failed run does not stop the timer.** The failure is recorded like any other, reported on the
  daemon's output, and the unit is re-armed for its next fire.
- **Shutdown is not held hostage by a hook.** `SIGTERM` / Ctrl-C reaches an in-flight `preBackup`
  or `postBackup` hook, so a `systemctl stop cfgd` during a backup does not wait out the hook's
  own timeout.

## Restoring

`cfgd backup restore <name>` puts a snapshot back where it came from. With no `--at` it picks the
**newest** snapshot. On a terminal it asks first, naming the snapshot and the path it is about to
overwrite:

```
? Restore 'notes-db' from snapshot notes.db.20260813T061322Z into
  /home/me/.local/share/notes/notes.db? (y/N)
```

`--yes` answers it up front, which is the form that fits in a script:

```console
$ cfgd backup restore notes-db --yes
Restore: notes-db
  Config   /home/me/.config/cfgd/cfgd.yaml
  Profile  workstation
  Actions  1 planned

◐ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)"
  0|0|0
✓ preBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA wal_checkpoint(TRUNCATE)" (0.1s)
◐ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check"
  ok
✓ postBackup: sqlite3 ~/.local/share/notes/notes.db "PRAGMA quick_check" (0.1s)

backup:notes-db
  ✓ restore from notes.db.20260813T061333Z — 8.0 KB
  Destination  /home/me/.local/share/notes/notes.db
  → Previous contents saved to /home/me/.local/state/cfgd/backups/notes-db/notes.db.20260813T061347Z

✓ Restore complete — 1 action succeeded (0.3s wall)
```

```bash
cfgd backup restore notes-db                                  # newest snapshot
cfgd backup restore notes-db --at 20260730T120000Z            # by the timestamp portion
cfgd backup restore notes-db --at notes.db.20260730T120000Z   # or the full snapshot name
cfgd backup restore notes-db --to /tmp/inspect --yes          # somewhere else, no prompt
```

`--to` redirects where the snapshot lands. A path outside the backup's source leaves the live
source untouched and takes no safety snapshot; a path at or inside the source is a restore-to-source
in all but spelling, and behaves like one.

`--at` matches the full snapshot name first, then any snapshot name **containing** the value,
which is what lets a bare timestamp reach `notes.db.20260730T120000Z` without you knowing the
unit's `namePattern`. A value matching more than one snapshot is refused rather than resolved to
the newest match: a restore overwrites live data, so an ambiguous selection is never guessed at.
An unknown value lists every available snapshot and exits `6`, the same treatment an unknown
backup name gets.

**Confirmation is required.** `--yes` (or `CFGD_YES=1`) skips the prompt. Where cfgd *cannot*
prompt (piped stdin, a CI runner, or `-o json`), a restore without `--yes` is an **error**, not a
silent "aborted": you asked for a restore and did not get one.

### What a restore does

```
acquire the unit's lock, re-resolve the selected snapshot under it
      │
      ▼
stage the selected snapshot into a temp dir beside the target
      │                                  (before the safety snapshot; see below)
      ▼
preBackup hooks                          (CFGD_OPERATION=restore)
      │
      ├──fail──►  safety snapshot + overlay SKIPPED──┐
      │ ok                                           │
      ▼                                              │
safety snapshot of the CURRENT target    (skipped when the target is not the source,
      │                                   or the source is gone; no hooks of its own)
      ▼                                              │
overlay the staged snapshot onto the target          │
      │                                              │
      ▼                                              ▼
postBackup hooks     ← always attempted, on every path above
      │
      ▼
staging removed      ← on every path, success or failure
```

- **Overlay, not mirror.** Every file the snapshot holds overwrites its counterpart; files present
  only in the target are **left alone**, so a restore never deletes a *name* the snapshot does not
  contain. Use `--to` and copy by hand if you want an exact mirror.
- **A name the snapshot owns is taken back, whatever occupies it.** If the snapshot holds a file at
  a name the target now holds as a directory (or a symlink), that directory is **removed**, with
  everything under it, and replaced by the snapshot's file. It is inside the restore target, so
  the safety snapshot captured it and is the recovery.
  The kind check in [What a restore refuses](#what-a-restore-refuses) guards the **top-level**
  target only; nested kind swaps are resolved in the snapshot's favour rather than refused, because
  a restore that stops halfway through a tree is worse than one that completes.
- **File modes come across** on Unix, the same way the backup carried them in. Snapshots hold no
  symlinks by construction (the writer skips them), so a link living in the target at a name the
  snapshot does **not** own survives untouched. A link sitting at a name the snapshot **does** own
  is **removed and replaced** by the snapshot's own file or directory, never written through.
  Following it would truncate a file, or populate a whole tree, outside the restore target and
  outside what the safety snapshot captured.
- **The overlay is not atomic as a whole.** Each file is replaced atomically (temp file + rename),
  but a directory restore interrupted halfway leaves the target part old and part new. The safety
  snapshot is what recovers it; a single-file backup has no such window.
- **The safety snapshot is an ordinary snapshot, but not an ordinary run.** It writes a normal
  `backup_runs` row and **participates in normal retention**, so it counts against `retention`, can
  evict an older snapshot, and lists (as `Kind: safety`) and restores like any other. What it is
  not is a backup of the unit: `backup list` never reports it as the unit's **Last Run**, and the
  daemon never re-anchors **Next Run** on it, so restoring a unit does not push its schedule out.
  Its path is reported as `safetySnapshot` in `-o json` and as the `→` line in human
  output. If it fails to produce a snapshot, the restore is **abandoned**: cfgd will not overwrite
  data whose current contents were not captured.
- **It is skipped on the target, not on the flag.** `--to` pointing back at the source, or at a
  path inside it, overwrites exactly what a plain restore would, so it still takes one. Only a
  target genuinely outside the source (or a source that does not exist yet) skips it.
- **Staging comes first for a reason.** The safety snapshot prunes to `retention`, and the snapshot
  being restored can be the one it evicts; staging the payload beforehand makes the restore immune
  to that. When the safety snapshot renders the same *name* as the snapshot being restored (same
  second, same `namePattern`), cfgd appends `-1`, `-2`, and so on, so both survive under distinct
  names (see [`namePattern`](#namepattern)).
- **The unit's `preBackup` / `postBackup` hooks run exactly once**, wrapped around the whole
  restore including the safety snapshot. The safety snapshot does not open a second envelope of its
  own: the unit declares one hook list, and running it twice around a source the restore has
  already quiesced breaks any hook that is not idempotent. Hooks see
  `CFGD_OPERATION=restore` to tell the two directions apart.
- **A `preBackup` failure skips the safety snapshot and the overlay**, exactly as it skips the
  snapshot during a run: the hook exists to quiesce the target, and neither snapshotting nor
  overwriting it after the hook failed is trustworthy. `postBackup` still runs.
- **One at a time.** A restore takes the same per-unit lock a run does, so it can never interleave
  with a scheduled fire or an apply of the same backup.

### What a restore refuses

| Refused | Why |
|---|---|
| a target inside the backup's `destination` | restoring there would overwrite the snapshot store |
| a **top-level** snapshot/target kind mismatch (file over directory, or the reverse) | publishing a file over a directory would delete the whole directory on the way to the rename. Nested names inside a directory overlay are replaced instead of refused — see above |
| a snapshot that vanished since it was listed | a concurrent prune, or a hand-deleted destination — re-checked *after* the lock is taken, so the window a confirmation prompt opens is covered |
| a failed safety snapshot | the current contents were not captured |

**Restores are not recorded.** The `backup_runs` table is the ledger retention walks, and a
restore produces no artifact for it to prune. The safety snapshot it takes *is* recorded, as an
ordinary run. `cfgd rollback` covers cfgd's own file writes and is unrelated to this table.

### Restoring by hand

A snapshot is an ordinary file or directory, so nothing stops you from doing it yourself. That
is the right call when you want mirror semantics (`rsync --delete`) rather than an overlay:

```bash
# file snapshot
cp ~/.local/state/cfgd/backups/notes-db/notes.db.20260813T061306Z ~/.local/share/notes/notes.db

# directory snapshot
rsync -a --delete ~/.local/state/cfgd/backups/journal/journal.20260813T061306Z/ ~/Documents/journal/
```

## Limitations

- A missed **cron** occurrence is skipped, not caught up: a daemon that was stopped over a `0 3 * * *`
  fire takes the next 3am, not the one it slept through. (Interval schedules do resume from the last
  recorded run; see [`schedule`](#schedule).)
- Snapshots are full copies: no incremental, deduplicating, or compressed modes.
- Symlinks inside a directory source are skipped rather than recreated. On restore, a symlink
  occupying a name the snapshot owns is replaced by the snapshot's own entry.
- A directory restore is not atomic as a whole; see [What a restore does](#what-a-restore-does).
- Concurrent runs of one backup are refused, not queued: the second caller is told who holds the
  unit (see above).
- `cfgd backup restore` overlays; it never deletes a name the snapshot does not contain, but it
  does replace one it *does* contain, even when the target now holds a directory there. Restore
  with `--to` and mirror by hand when you need the target to match the snapshot exactly.
- `spec.backups[]` is available on the YAML/TOML profile path only; CRD parity is not implemented.
