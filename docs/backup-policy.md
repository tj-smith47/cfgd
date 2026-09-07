# BackupPolicy

A namespaced CRD that sets the cadence of backup units the machines it selects already define.
A policy overrides a named unit's `schedule` and `retention`. It never defines a unit: `source`
and `destination` are machine-local paths a cluster object cannot know, so a unit the policy
names on a machine whose profile does not define it is reported and otherwise left alone.

The unit itself stays in the machine's own profile, under [`spec.backups[]`](backups.md). The
schedule is evaluated on the machine's local clock, so a fleet-wide `0 3 * * *` means 3am where
each machine sits (not 3am in the cluster's timezone), and an interval schedule seeds from that
machine's own last recorded run.

```sh
kubectl get backuppolicies          # short name: bpol
```

## Fields

`spec`:

| Field | Type | Required | Description |
|---|---|---|---|
| `selector` | object | no | Which MachineConfigs in this namespace the policy schedules backups for. Empty (the default) matches all of them. Same `matchLabels` / `matchExpressions` shape as [`ConfigPolicy.spec.targetSelector`](spec/configpolicy.md) |
| `units` | list | yes | Schedule overrides, each naming a backup unit the matched machine's own profile defines. At least one entry: a policy that schedules nothing sets no cadence anywhere, so the CRD schema requires the field and an empty list is refused at admission |

`spec.units[]`:

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Name of the unit in the machine's `spec.backups[]`. Unique within the list: two entries sharing a name leave no answer for which schedule the unit runs on, and the CRD merges the list server-side by this field |
| `schedule` | string | yes | Cron expression (`0 3 * * *`) or interval (`6h`), evaluated on the machine's local clock |
| `retention` | integer | no | How many snapshots the unit keeps. Omitted, the machine's own profile decides. Must be at least 1: a `0` would prune every snapshot the unit takes |

## Precedence

| Layer | Owns | Wins |
|---|---|---|
| local profile `spec.backups[]` | the unit: `source`, `destination`, `namePattern`, `retention`, `preBackup`/`postBackup` | always — the cluster never defines a unit |
| cluster `BackupPolicy.spec.units[]` | `schedule`, `retention` override for a named unit | when the profile does not set `scheduleOwner: Local` |

[`scheduleOwner: Local`](backups.md#scheduleowner) is the user-facing control, and it exists
because there is a real case the cluster cannot know: a laptop is asleep at 3am, so a fleet-wide
nightly window is wrong for it and only its owner knows that. A unit pinned `Local` still appears
in `status.units` with `owner: local`, so a policy reports the unit it declined to schedule
rather than appearing to have applied.

## Status

Written by the operator, never set in `spec`:

| Field | Type | Description |
|---|---|---|
| `observedGeneration` | integer | The `metadata.generation` the rest of the status was computed from |
| `units` | list | One row per (machine, unit): `name`, `hostname`, `owner` (`cluster` or `local`), `schedule`, `retention`, `lastRun`, `nextRun`, `message`. Sorted by (hostname, name) and capped at 500 rows. The rows are merged by (hostname, name), so two MachineConfigs naming one hostname describe one machine and produce one row |
| `units[].schedule` | string | The schedule this policy set, absent on a row the machine pins: a policy states no cadence it did not choose |
| `units[].lastRun` / `units[].nextRun` | string | When the unit last ran and is next due. Absent until a device reports them |
| `unitsSummary` | string | The unit names in `units`, deduplicated and comma-joined. What the `Units` printer column shows |
| `machinesMatched` | integer | How many machines the selector matched, exact and never capped |
| `conditions` | list | Standard condition list, carrying `Applied`: `True` / `Projected` once the selector matches a machine, `False` / `NoMatchingMachines` while it matches none. The message counts what the policy scheduled apart from what the machines pinned |

The operator reconciles a policy every 60 seconds, and retries a reconcile that failed (an invalid spec, an unreachable API server) after 30 seconds.

## How a machine pins a unit

A machine reports the units it schedules itself in `MachineConfig.status.backupScheduleOwners`,
a map of unit name to owning layer:

```yaml
status:
  backupScheduleOwners:
    dotfiles: local          # this machine keeps its own window
```

The field is written by the device gateway on every check-in, from the machine's own
[`spec.backups[].scheduleOwner`](backups.md#scheduleowner). No controller computes it, and a
reconcile that cannot observe it carries it forward rather than blanking it.

The BackupPolicy controller reads it and nothing else: a unit reported `local` gets a row with
`owner: local`, no `schedule`, and a message saying the policy declined to apply. Every other
unit is projected with the policy's own schedule. A machine that never checks in through a
gateway reports nothing, so its units are projected as `cluster`.

The word is read case-insensitively, so `local`, `Local` and `LOCAL` are one answer. A word no
layer spells is read the safe way round: the unit is reported with `owner: local` and a message
naming the value, and the policy raises a `Warning` event `UnreadableScheduleOwner` against
itself naming the machine. A policy never claims a schedule it cannot show the machine took.

## How the cadence reaches the machine

The gateway answers every check-in with the schedules the cluster owns for that machine, read
live from the `status.units` rows the controller wrote. The owner word is read
case-insensitively, and only a row whose owner reads `cluster` is sent: a unit the machine pinned
is never projected back at it, and a word no layer spells is an answer the policy could not be
read for, so it projects nothing. Two policies scheduling one unit for one machine is a conflict
the gateway cannot settle on merit, so it settles it stably, the older `creationTimestamp`
winning and the collision logged at `warn`, and the machine sees one cadence rather than
alternating between two.

A check-in that never reached the gateway, or whose answer could not be read, records nothing:
the machine keeps the cadences it was last given, because one unreachable gateway must not
retire what the cluster still owns. A gateway that answered but could not read the cluster
itself, its MachineConfig or BackupPolicy list failing, sends no `backupSchedules` at all for the
same reason, and a standalone gateway with no cluster behind it sends none either. An answer
that carries the field, empty included, is a read that succeeded, and the machine replaces its
whole recorded set from it, which is how a cadence the cluster stopped owning is retired.

The machine holds the answer alongside its profile and never inside it: the projection decides
when a cluster-owned unit is next due, and `cfgd backup list` shows it in the Schedule and
Retention cells with `cluster` in the Schedule Owner column. `-o json` carries the declared value
always and the effective one only when the cluster CHANGED it, so the presence of
`effectiveSchedule` or `effectiveRetention` states an override rather than restating a cadence the
profile already declared. Nothing rewrites `spec.backups[]`, so a machine that stops matching a
policy falls back to its own declaration on the next check-in.

## Example

```yaml
# Cluster: fleet-wide schedule policy, selector-matched.
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: nightly-dotfiles
spec:
  selector:
    matchLabels:
      cfgd.io/profile: workstation
  units:
    - name: dotfiles          # matches a unit in the machine's local profile
      schedule: "0 3 * * *"
      retention: 14
status:
  unitsSummary: dotfiles
  machinesMatched: 2
  units:
    - name: dotfiles
      hostname: laptop-01
      owner: local
      message: the machine pins this unit's schedule; this policy reports it and does not apply
    - name: dotfiles
      hostname: nuc-01
      owner: cluster
      schedule: "0 3 * * *"
      retention: 14
```

The unit the policy schedules, as the machine's own profile defines it:

```yaml
# Local profile: still the definition of what a unit IS.
spec:
  backups:
    - name: dotfiles
      source: ~/.config
      destination: /var/backups/cfgd
      schedule: "0 3 * * *"
      scheduleOwner: Local     # this machine keeps its own window
      retention: 7
```

## See also

- [Declarative Backups](backups.md): the unit definition, hook ordering, retention, restoring
- [Operator](operator.md): CRD installation, the admission webhook, controllers
