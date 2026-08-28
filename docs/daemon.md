# Daemon

The daemon runs as a long-lived process that watches for drift and optionally auto-corrects it. For the complete daemon configuration field reference, see the [Config spec reference](spec/config.md#specdaemon).

## What It Does

1. **File watching**: uses the OS's file change notification system (inotify on Linux, FSEvents on macOS) to detect when managed files change. Rapid changes are batched (500ms window), so saving a file in your editor does not trigger three reconciles.

2. **Reconciliation loop**: on a configurable interval (default 5 minutes), diffs the entire desired state against actual state and reports or fixes drift.

3. **Sync loop**: pulls from the git remote on interval. Optionally auto-commits and pushes local changes. When using [multi-source config](sources.md), syncs each source independently.

4. **Backup timers**: runs each `spec.backups[]` entry that declares a `schedule`, on its own interval or cron. See [Declarative Backups](backups.md#daemon-scheduling).

![an edit committed on machine A landing on machine B through the daemon's sync and reconcile loops](../demo/cfgd-sync.gif)

## Architecture

```
                 ┌─────────────┐
                 │ daemon main │
                 └──────┬──────┘
        ┌──────────┬────┴─────────┬──────────────┐
┌───────▼──────┐ ┌─▼──────────┐ ┌─▼────────────┐ ┌▼───────────────┐
│ file watcher │ │ sync timer │ │ backup timers│ │ health API     │
└───────┬──────┘ └─┬──────────┘ └─┬────────────┘ │ (unix socket)  │
        └────┬─────┘              │              └────────────────┘
     ┌───────▼─────┐      ┌───────▼───────┐
     │ reconcile   │      │ backup engine │
     │ + notify    │      └───────────────┘
     └─────────────┘
```

The daemon runs as a single tokio async runtime. Shutdown is graceful via SIGTERM/SIGINT (Unix) or the Windows Service control manager stop signal (Windows).

The signal handlers are installed **before** the `daemon: running` line is logged: a supervisor that starts cfgd and signals it immediately gets the clean shutdown path, not an abrupt kill.

## Configuration

```yaml
spec:
  daemon:
    enabled: true
    reconcile:
      interval: 5m          # drift check interval
      onChange: true        # reconcile immediately on file change
    sync:
      autoPull: true        # pull from remote on interval
      autoPush: false       # auto-commit and push local changes
      interval: 5m           # sync interval
    notify:
      drift: true
      method: Desktop        # Desktop | Stdout | Webhook
      webhookUrl: https://hooks.example.com/cfgd
```

## Auto-Apply Policies

When the daemon is running with `autoApply: true` and a source pushes an update, new items need decisions. The `policy` block controls this behavior:

```yaml
spec:
  daemon:
    reconcile:
      autoApply: true
      policy:
        newRecommended: Notify    # Notify | Accept | Reject
        newOptional: Ignore       # Notify | Ignore
        lockedConflict: Notify    # Notify | Accept
```

When `policy` is omitted entirely, the defaults are:

| Setting | Default | Meaning |
|---|---|---|
| `newRecommended` | `Notify` | New recommended items create a pending decision and send a notification |
| `newOptional` | `Ignore` | New optional items are silently skipped |
| `lockedConflict` | `Notify` | Conflicts with locked items create a pending decision |

When `autoApply: false`, policies have no effect: no decision row is created and every source item applies.

Setting `lockedConflict: Accept` causes the daemon to automatically remove your local overrides when they conflict with a locked item from a source. This is destructive: your local value is replaced without confirmation. The `Notify` default is safer: cfgd flags the conflict and waits for you to resolve it with `cfgd decide`.

See [sources.md](sources.md#automatic-apply-decisions) for the full decision workflow.

## What a Tick Prints

A foreground `cfgd daemon` reports a reconcile tick with the same header, phase and owner
tree, and rollup that `cfgd apply` prints, so a tick and a manual run read the same way.
Only the title differs, and the header gains a `Trigger` row naming what woke the tick:

```console
$ cfgd daemon
14:32:05  INFO daemon: starting cfgd 0.9.0
14:32:05  INFO daemon: health endpoint at /run/user/0/cfgd/cfgd.sock
14:32:05  INFO daemon: running — reconcile every 5s
→ Press Ctrl+C to stop
14:32:10  INFO watch: config changed profiles/driftdemo.yaml
14:32:10  INFO reconcile: drift detected in 1 resource

Reconcile
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  driftdemo
  Trigger  drift (1 resources)
  Phases   Files
  Actions  1 planned

Phase: Files
  profile:driftdemo
    ✓ update /home/you/.gitconfig

✓ Reconcile complete — 1 action succeeded (0.1s wall)
14:32:10  INFO reconcile: complete — 1 action succeeded
```

Every line the daemon logs is `HH:MM:SS  INFO <subsystem>: <sentence>` in local time, with
`daemon:`, `sync:`, `reconcile:` and `watch:` the four subsystems that speak. Operands are
spelled into the sentence rather than appended as `key=value`; the field form lives on the
`debug!` event beside each info line, so `-v` still gives a machine-parseable stream. The
`press Ctrl+C to stop` hint is the one piece of the startup that is not a log line: it is
printed only when stdout is a terminal, because a service under systemd has no keyboard.

The `tracing` lines around it are unchanged, so existing log consumers keep working; the
tree is strictly additional. Under `driftPolicy: NotifyOnly` (or `Prompt`, which has no
terminal to prompt at in daemon context) the same header renders with the *preview* tree
(what drifted, never what was done) and closes on
`⚠ Drift detected — N actions; policy is notify-only, nothing applied` instead of a
completion rollup. Daemon lifecycle events (startup, SIGHUP reload, shutdown) are `daemon:` log
lines rather than parts of the tree: they describe the process, not a run over a plan.

Scheduled `spec.backups[]` fires render the same way, as a `Backup` run over a `Backups`
group per unit; see [Declarative Backups](backups.md#daemon-scheduling).

## Drift Hooks

When the daemon detects drift, it runs any `onDrift` scripts defined in the active profile before deciding how to handle the drift (`autoApply`, notify, or prompt). This fires regardless of the drift policy: `onDrift` is observability, not remediation.

```yaml
# In your profile
scripts:
  onDrift:
    - scripts/notify-slack.sh
    - run: scripts/snapshot-state.sh
      timeout: 30s
      continueOnError: true
```

Environment variables available to onDrift scripts: `CFGD_CONFIG_DIR`, `CFGD_PROFILE`, `CFGD_CONTEXT=reconcile`, `CFGD_PHASE=onDrift`. See the [Profile spec reference](spec/profile.md#specscripts) for the full script entry schema, timeout defaults, and `continueOnError` behaviour.

They run before the reconcile header, under a `Drift Hooks` heading with one owner group
per declaring thing, so a hook is attributed the same way a planned action is:

```
Drift Hooks
  profile:driftdemo
    ✓ onDrift: scripts/notify-slack.sh   (0.1s)
  module:nvim
    ✓ onDrift: scripts/snapshot-state.sh (0.1s)
```

The heading carries no `Phase: ` prefix: `Phase: ` marks a phase the plan produced, and
hooks are not planned. An `onDrift` failure renders as a warning, never as a run failure.

## Drift Accounting

The `driftCount` reported by `cfgd daemon status` is the **current** number of managed targets diverging from desired state, not a lifetime total. It rises when targets drift and returns to `0` once everything is healed or clean:

- A reconcile tick that finds **no drift** resets the count to `0`.
- With `driftPolicy: Auto`, a successful heal applies the fix and drives the count back to `0` in the same tick. A partial-failure heal leaves only the still-diverging targets counted.

Only a managed target diverging **out-of-band** (edited or removed outside cfgd) counts as drift. Changes to your config **sources** (the git-synced config directory: `.git/`, `profiles/`, `files/`, `cfgd.yaml`) are desired-state updates: they *trigger* a reconcile (the GitOps pull-then-apply path) but are never counted as drift themselves.

The `/status` endpoint's `driftCount` and the `/drift` endpoint's events list always reflect the same current set of unresolved drift, so the count and the event detail stay consistent.

## Reconcile Patches

Override reconcile settings for specific modules or profiles. Patches live in your `cfgd.yaml`: you control your machine's sync behavior regardless of what upstream profiles or modules recommend.

```yaml
spec:
  daemon:
    reconcile:
      interval: 5m
      driftPolicy: NotifyOnly
      patches:
        - kind: Module
          name: certificates
          interval: 1m
          driftPolicy: Auto
        - kind: Module
          name: shell-theme
          interval: 1h
          autoApply: false
        - kind: Module
          interval: 30s
        - kind: Profile
          name: base
          autoApply: true
```

Each patch targets by `kind` (`Module` or `Profile`). When `name` is provided, the patch applies only to that entity. When `name` is omitted, the patch applies to all entities of that kind (kustomize semantics). Named patches take priority over kind-wide patches. Override any combination of `interval`, `autoApply`, and `driftPolicy`. Omitted fields inherit from the next level up.

### Precedence

Most specific wins, fields resolve independently:

```
Named Module patch > Kind-wide Module patch > Named Profile patch > Kind-wide Profile patch > Global
```

When multiple Profile patches match the inheritance chain (e.g., `base` and `work`), the leaf profile (the active one) wins, consistent with how profile inheritance resolves other conflicts.

### Conflict resolution

| Scenario | Result |
|---|---|
| Module patch and Profile patch both set `autoApply` | Module patch wins |
| Two Profile patches in inheritance chain set `interval` | Leaf profile wins |
| Module patch sets `driftPolicy: Auto`, global is `NotifyOnly` | Module patch wins |
| Same module patched twice in the list | Last entry wins (warning logged) |
| Patch references a module/profile that does not exist | Silently ignored |

## Notifications

When drift is detected, the daemon notifies via:

- **Desktop** (default): native OS notification APIs
- **Stdout**: logs to stdout (useful under systemd, which captures journal output)
- **Webhook**: POSTs a JSON payload to a configured URL

## Health API

The daemon exposes a health endpoint on a per-user Unix socket. The socket is
placed in the first writable runtime directory:

- **Linux**: `$XDG_RUNTIME_DIR/cfgd/cfgd.sock` (typically `/run/user/<uid>/cfgd/cfgd.sock`),
  falling back to `~/.cache/cfgd/cfgd.sock` if `$XDG_RUNTIME_DIR` is unset.
- **macOS**: `~/Library/Application Support/cfgd/cfgd.sock`.
- **Windows**: named pipe `\\.\pipe\cfgd` (per-session in the kernel namespace).

The parent directory is created with mode `0700` and the bound socket is
chmodded to `0600` before the first connection is accepted, so the IPC surface
is reachable only by the daemon's own user. Set `CFGD_DAEMON_IPC_PATH` to
override the path for advanced setups (test harnesses, multi-instance
isolation). Query with `cfgd daemon status` to get:

- Whether the daemon is running
- The config file and profile the loop was started against, and the modules that profile resolves to (`modules` in `-o json`), the same header rows `cfgd status`, `cfgd diff`, `cfgd sync` and every run header open with
- How long ago the last reconcile ran (`2m ago`; the stored ISO 8601 instant stays in `-o json`)
- The reconcile and sync intervals the loop is currently on (`reconcileIntervalSecs` / `syncIntervalSecs` in `-o json`), so a SIGHUP reload can be confirmed without reading the log
- Drift count
- Per-source sync status in the same `Sources` table `cfgd source list` renders, each row with its own `Last Sync` age and the commit the daemon's last pull landed on (`Commit`, shortened; the full id is `sources[].lastCommit` in `-o json`). The implicit `local` layer declares no origin, priority or signing demand, so those cells read `-` beside a declared source and the columns are left off when it is the only row

## CLI Commands

```sh
cfgd daemon                # run in foreground (default)
cfgd daemon run            # run in foreground (explicit)
cfgd daemon install        # install as launchd (macOS), systemd (Linux), or Windows Service
cfgd daemon status         # check running state, last reconcile, drift count
cfgd daemon uninstall      # stop the running daemon and remove the service
```

`uninstall` is the exact inverse of `install`: it stops and disables the
running service (`systemctl --user disable --now` on Linux, `launchctl bootout`
on macOS, `sc stop` on Windows) **before** removing the unit/plist/registration,
so no orphaned daemon process is left running. The stop step is best-effort: if
the session cannot reach its service manager (a headless SSH login with no user
systemd), `uninstall` still removes the file and prints a warning plus the
manual stop command rather than aborting.

## Live config reload (SIGHUP)

Sending `SIGHUP` to the running daemon reloads the **reconcile and sync timer
intervals and the scheduled-backup timer set**. The reload is intentionally
narrow:

```sh
kill -HUP "$(cfgd daemon status --output json | jq .pid)"
# → status: "Reloading configuration (SIGHUP) — timer intervals and backup
#            schedules only; other fields require restart"
# → status: "Timer intervals reloaded: reconcile=300s, sync=600s
#            (other field changes require restart)"
# → status: "Backup schedules reloaded: 1 added, 0 removed, 1 rescheduled"
```

Fields that **do** reload on SIGHUP:
- `daemon.reconcile.interval`
- `daemon.sync.interval`
- `backups` (add, remove, or reschedule a `spec.backups[]` entry; a unit whose
  `schedule` did not change keeps its pending deadline rather than restarting
  the clock)

The backup-timer swap is all-or-nothing. A reload whose config does not fully
resolve (a profile saved mid-edit, a source cache being rewritten) keeps the
schedules already running and retries on its own, rather than swapping in a
partial set:

```sh
# → status: "Backup schedules NOT reloaded: config did not fully resolve —
#            keeping the 2 running schedules, retrying automatically"
```

The same retry covers startup: a daemon that boots while its profile is
unreadable starts with no backup timers, reports `backups=0 scheduled (profile
unresolved)` in its banner, and re-resolves on its own. With no timers running
there is nothing to protect, so the first resolution that produces a set is
adopted even if sources are still unavailable. That adoption is reported as a
warning naming what is still missing, never as an all-clear:

```sh
# → status: "Backup schedules reloaded: 1 added, 0 removed, 0 rescheduled
#            (source composition unavailable)"
```

Fields that **require a daemon restart** to take effect:
- `profile` (active-profile change)
- `sources` list (add / remove / re-prioritize)
- `daemon.drift_policy`, `daemon.notify_on_drift`, `daemon.on_change_reconcile`
- `daemon.compliance` block
- `packages`, `files`, `system` (managed-path set is wired into the file
  watcher at startup)

Restart with `cfgd daemon` (foreground) or the service-manager equivalent
(`systemctl --user restart cfgd`, `launchctl kickstart -k gui/$UID/com.cfgd.daemon`,
`sc.exe stop cfgd && sc.exe start cfgd`).

> Why so narrow? Intervals and backup deadlines can be swapped mid-flight
> without races. The other fields are baked into the file watcher and the
> daemon's loop state at startup; reloading them in-flight would race against
> running reconciles, so SIGHUP refuses to touch them and tells you to restart.

## Service Management

`cfgd daemon install` creates a native service definition:

- **macOS**: LaunchAgent plist in `~/Library/LaunchAgents/`
- **Linux**: systemd user unit in `~/.config/systemd/user/`
- **Windows**: Windows Service registered via `sc.exe`, running as `LocalSystem`

The service is configured to start at login (macOS/Linux) or at system boot (Windows).

### System scope

Pass `--scope system` (or `CFGD_SCOPE=system`) to install cfgd as a privileged,
machine-wide daemon using FHS paths. This is the right choice for servers, k8s nodes,
and any host where cfgd is managed by infrastructure tooling rather than a personal login.

```bash
sudo cfgd --scope system daemon install    # install the system service
sudo cfgd --scope system daemon uninstall  # stop and remove it
cfgd --scope system daemon status          # check state (no root needed)
```

`install` and `uninstall` require root (`sudo` on Linux/macOS). Running without root
prints a clear error and the exact `sudo` command to re-run. `daemon status` never
requires root.

On **Linux** the service is written to `/etc/systemd/system/cfgd.service` and activated
with `systemctl enable --now cfgd`. The unit includes
`ConfigurationDirectory=cfgd`, `StateDirectory=cfgd`, `CacheDirectory=cfgd`, and
`RuntimeDirectory=cfgd` directives; systemd creates and owns those directories and
injects `$CONFIGURATION_DIRECTORY`, `$STATE_DIRECTORY`, `$CACHE_DIRECTORY`,
`$RUNTIME_DIRECTORY` into the process environment. cfgd reads them in preference to
the FHS defaults, so any systemd override (`SystemdConfigurationDirectory`,
`TemporaryFileSystem`, `BindPaths`) is fully honored.

On **macOS** the plist is written to `/Library/LaunchDaemons/com.cfgd.daemon.plist`
and loaded with `launchctl bootstrap system`. Logs go to `/var/log/cfgd.log` and
`/var/log/cfgd.err`.

The generated service bakes `--scope system` into `ExecStart` (Linux) and `ProgramArguments`
(macOS), so the daemon and any `cfgd --scope system <command>` admin-CLI invocations resolve
the same roots. Any `--state-dir` / `--runtime-dir` the install itself ran under is baked in the
same way: the installed service is a fresh process with none of the invoking shell's flags, so
without that the daemon would write its state somewhere the CLI never looks:

```bash
sudo cfgd --scope system --state-dir /srv/cfgd/state daemon install
# ExecStart=/usr/local/bin/cfgd --config /etc/cfgd/cfgd.yaml --scope system \
#           --state-dir /srv/cfgd/state --quiet daemon
```

Path defaults under system scope:

| Root | Linux | macOS |
|---|---|---|
| Config | `/etc/cfgd` | `/Library/Application Support/cfgd` |
| State | `/var/lib/cfgd` | `/Library/Application Support/cfgd/state` |
| Cache | `/var/cache/cfgd` | `/Library/Caches/cfgd` |
| Runtime | `/run/cfgd` | `/Library/Application Support/cfgd/runtime` |

All four roots can still be relocated with `--<role>-dir` flags or `CFGD_<ROLE>_DIR`
env vars, or via the systemd `$*_DIRECTORY` injection described above. Use
`cfgd --scope system paths` to confirm the resolved roots on any host.

### Headless installs on Linux

On Linux the unit is a systemd **user** unit, so `cfgd daemon install`
(and `cfgd init --install-daemon`) starts it with `systemctl --user`, which
needs the per-user bus at `$XDG_RUNTIME_DIR`. A non-interactive bootstrap (ssh
non-login shell, CI, container, provisioning script) usually has no
`XDG_RUNTIME_DIR`. cfgd detects this and self-sets `XDG_RUNTIME_DIR` to
`/run/user/<uid>` when that directory exists, noting it in the output. If no
user session bus exists at all, cfgd installs the unit, reports that it could
not start it, and points you at lingering. Enable it so the user service can
run without an active login:

```bash
loginctl enable-linger $USER
cfgd daemon install
```

### Windows Service

On Windows, `cfgd daemon install` registers cfgd as a Windows Service named `cfgd`, running as `LocalSystem`. The service starts automatically on boot.

```sh
cfgd daemon install    # register and start the Windows Service
cfgd daemon status     # show service state, last reconcile, drift count
cfgd daemon uninstall  # stop and remove the Windows Service
```

Requires running in an elevated (Administrator) prompt for install and uninstall. `cfgd daemon status` works without elevation.

#### Logging on Windows

cfgd writes daemon logs to a file by default and can mirror them into the Windows Event Log on demand. Both sinks are supported; pick whichever fits how you operate the host.

**File log (default).** Every install captures stdout, stderr, and the structured `tracing` stream into `%LOCALAPPDATA%\cfgd\daemon.log`. Open with `Get-Content`, `notepad`, your editor, or stream over PSRemoting:

```powershell
Get-Content -Wait -Tail 200 $env:LOCALAPPDATA\cfgd\daemon.log
```

When the service runs as the default `LocalSystem`, the file lives under
`C:\Windows\System32\config\systemprofile\AppData\Local\cfgd\daemon.log`
instead. Change the service's logon account (Services → cfgd → Properties → Log On) if you want logs under your interactive user profile.

**Event Log (opt-in).** Mirror the same stream into Event Viewer / Windows Event Forwarding / SIEM ingestion by setting:

```yaml
# cfgd.yaml
spec:
  daemon:
    windowsEventLog: true
```

Then run `cfgd daemon install` (or `uninstall` then `install`, if the service was already registered). Install does two things when the flag is set:

1. Bakes `--enable-event-log` into the service binPath, so the daemon installs a second `tracing` Layer that writes to the `cfgd` Event Log source on every event.
2. Creates `HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd` pointing `EventMessageFile` at `%SystemRoot%\System32\EventCreate.exe`, which is what makes Event Viewer show your messages cleanly instead of "The description for Event ID 1 cannot be found." Both writes require elevation; `cfgd daemon install` already runs elevated, so no extra step is needed.

Once the service is reinstalled, find cfgd events in:

```
Event Viewer → Windows Logs → Application → Source: cfgd
```

…or query from the command line:

```powershell
Get-WinEvent -LogName Application -ProviderName cfgd -MaxEvents 50
```

`Get-WinEvent` works against remote machines too (`-ComputerName host`), which is why this path matters for fleet ops.

**Mode switching.** The flag is read at install time. Flip it by editing `cfgd.yaml` and running:

```sh
cfgd daemon uninstall
cfgd daemon install
```

For ad-hoc testing without reinstalling, set `CFGD_WINDOWS_EVENT_LOG=1` in the running process's environment: the daemon consults both the CLI arg and the env var on every start.

`cfgd daemon uninstall` removes the Event Log registry source automatically, so reverting to file-only is also a single command pair.

| Need | Sink |
|---|---|
| Solo developer machine, `Get-Content` workflow | File only (default) |
| Enterprise fleet → SIEM ingest | File + Event Log |
| WEF central collector | File + Event Log |
| Admins look at Event Viewer for service health | File + Event Log |

The two are additive: enabling Event Log never disables the file appender, so file-based diagnostics keep working even if the Event Log channel is full or filtered upstream.
