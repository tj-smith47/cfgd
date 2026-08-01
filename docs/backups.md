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
[configuration.md](configuration.md#directory-layout) for where the state dir lands on each
platform. Set it explicitly to put snapshots on another disk:

```yaml
backups:
  - name: photos
    source: ~/Pictures
    destination: /mnt/nas/backups/photos
```

Snapshot payloads never go into the state database, and there is no size cap.

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

An unknown `{var}` is rejected when the config is parsed. A pattern that renders to an absolute
path, an empty string, or anything containing `..` is rejected at run time — a snapshot always
lands inside `destination`.

Two runs that render the same name (a pattern with no `{timestamp}`, or two runs inside one
second) resolve as "newest wins": the older snapshot is replaced.

### `retention`

How many snapshots to keep, defaulting to 10 and required to be at least 1. Pruning walks the
recorded runs — not a filename glob — and deletes both the artifact on disk and its record.

Retention is counted **per outcome**: the newest `retention` runs that produced a snapshot are
kept, and independently the newest `retention` that did not. A run of failures therefore never
deletes a good snapshot, and a permanently broken backup cannot grow the run table without bound.

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
preBackup hooks ──fail──► run recorded FAILED, no copy, no postBackup
      │ ok
      ▼
copy source → destination/<rendered-name>   (atomic: temp + rename)
      │
      ▼
postBackup hooks     ← always attempted, even when the copy failed
      │
      ▼
record the run  ──►  prune to `retention`
```

Three rules follow from that ordering:

1. **A `preBackup` failure aborts the unit.** The hook exists to make the source consistent (stop
   the service, flush the buffer); if it failed, the source is not in the state you asked for, so
   copying it would produce a snapshot you cannot trust. The run is recorded as failed with no
   artifact.
2. **`postBackup` always runs after the copy step** — including when the copy failed. It is
   normally the counterpart that restarts whatever `preBackup` stopped, so skipping it on a bad
   copy would leave the machine down.
3. **A `postBackup` failure after a good copy leaves the run successful, with the failure
   recorded.** The snapshot is complete and restorable, so it stays retention-eligible; marking the
   run failed would strand a valid artifact that pruning could never reclaim. The failure is still
   surfaced — the run is not *clean*, and the command reporting it says so.

Copies are atomic where the filesystem allows: a file streams into a sibling temp file that is
fsynced and renamed into place, and a directory is built under a `.<name>.partial` staging
directory published with a single rename. An interrupted run never leaves a half-written snapshot
under a name a restore would trust.

Every run — success or failure — is recorded in the `backup_runs` table of the state database with
its source, destination, size, status, error, and start/finish timestamps.

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

- Snapshots are full copies — no incremental, deduplicating, or compressed modes.
- Symlinks inside a directory source are skipped rather than recreated.
- `spec.backups[]` is available on the YAML/TOML profile path only; CRD parity is not implemented.
