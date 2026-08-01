# Declarative Backups

`spec.backups[]` declares snapshots of a file or directory that cfgd takes on your behalf,
retaining the newest N and pruning the rest. It is the "keep a copy of my app's data before I
touch it" surface — distinct from the automatic pre-overwrite `file_backups` that power
`cfgd rollback` (see [File Safety](safety.md#file-backups)).

> **Not yet reachable.** The engine described here is implemented and tested, but nothing drives
> it yet: the `cfgd backup ...` command surface and the daemon scheduling that honours `schedule`
> are separate, unlanded work. Declaring `spec.backups[]` today validates the config and nothing
> more.

| | `spec.backups[]` (this document) | Pre-overwrite backups |
|---|---|---|
| Declared by | you, in a profile | nobody — automatic |
| Covers | any file or directory on the machine | only files cfgd is about to write |
| Stored | on the filesystem, under `destination` | inline in the state DB |
| Retained | newest `retention` per backup | last 10 applies |
| Restored by | you (`cp`, `tar`, your tooling) | `cfgd rollback` |

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

**One run at a time per backup.** Two concurrent runs of the same `name` can render the same
snapshot name and prune against the same history; the delete paths are idempotent, so the worst
outcome today is a duplicated warning, but concurrent runs of one backup are not a supported
configuration. The command and daemon surfaces that drive the engine serialize their work.

## Restoring

cfgd does not restore declarative backups for you; the snapshot is an ordinary file or directory,
so restoring is whatever your data needs:

```bash
# file snapshot
sudo systemctl stop openlist
sudo cp ~/.local/state/cfgd/backups/openlist-db/data.db.20260801T031500Z /var/lib/openlist/data.db
sudo systemctl start openlist

# directory snapshot
rsync -a --delete ~/.local/state/cfgd/backups/photos/Pictures.20260801T031500Z/ ~/Pictures/
```

## Limitations

- No command or scheduler drives the engine yet — see the note at the top.
- Snapshots are full copies — no incremental, deduplicating, or compressed modes.
- Symlinks inside a directory source are skipped rather than recreated.
- Concurrent runs of one backup are unsupported (see above).
- `spec.backups[]` is available on the YAML/TOML profile path only; CRD parity is not implemented.
