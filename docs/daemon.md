# Daemon

The daemon runs as a long-lived process that watches for drift and optionally auto-corrects it.

## What It Does

1. **File watching** — Uses the OS's built-in file change notification system (inotify on Linux, FSEvents on macOS) to detect when managed files change. Multiple rapid changes are batched together (500ms window) to avoid redundant work — saving a file in your editor won't trigger three reconciles.

2. **Reconciliation loop** — On a configurable interval (default 5 minutes), diffs the entire desired state against actual state and reports or fixes drift.

3. **Sync loop** — Pulls from the git remote on interval. Optionally auto-commits and pushes local changes. When using [multi-source config](sources.md), syncs each source independently.

## Architecture

```
┌──────────────────────┐
│     Daemon Main      │
│  tokio::select!      │
└──────┬───────────────┘
       │
  ┌────┼────────────┐
  │    │            │
┌─▼──┐ ┌─▼──┐ ┌────▼───┐
│File│ │Sync│ │Health  │
│Watch│ │Timer│ │API     │
│    │ │    │ │(socket)│
└─┬──┘ └─┬──┘ └────────┘
  │      │
  └──┬───┘
     ▼
 ┌────────┐
 │Reconcile│
 │+ Notify │
 └────────┘
```

The daemon runs as a single tokio async runtime. Shutdown is graceful via SIGTERM/SIGINT.

## Configuration

```yaml
spec:
  daemon:
    enabled: true
    reconcile:
      interval: 5m          # drift check interval
      on-change: true        # reconcile immediately on file change
    sync:
      auto-pull: true        # pull from remote on interval
      auto-push: false       # auto-commit and push local changes
      interval: 5m           # sync interval
    notify:
      drift: true
      method: desktop        # desktop | stdout | webhook
      webhook-url: https://hooks.example.com/cfgd
```

## Auto-Apply Policies

When the daemon is running with `auto-apply: true` and a source pushes an update, new items need decisions. The `policy` block controls this behavior:

```yaml
spec:
  daemon:
    reconcile:
      auto-apply: true
      policy:
        new-recommended: notify    # notify | accept | reject
        new-optional: ignore       # notify | ignore
        locked-conflict: notify    # notify | accept
```

When `policy` is omitted entirely, the defaults are:

| Setting | Default | Meaning |
|---|---|---|
| `new-recommended` | `notify` | New recommended items create a pending decision and send a notification |
| `new-optional` | `ignore` | New optional items are silently skipped |
| `locked-conflict` | `notify` | Conflicts with locked items create a pending decision |

When `auto-apply: false`, policies have no effect. In manual mode, `cfgd plan` shows all items and you decide interactively.

Setting `locked-conflict: accept` causes the daemon to automatically remove your local overrides when they conflict with a locked item from a source. This is destructive — your local value is replaced without confirmation. The `notify` default is safer: cfgd flags the conflict and waits for you to resolve it with `cfgd decide`.

See [sources.md](sources.md#auto-apply-decisions) for the full decision workflow.

## Notifications

When drift is detected, the daemon notifies via:

- **Desktop** (default) — native OS notification APIs
- **Stdout** — logs to stdout (useful under systemd, which captures journal output)
- **Webhook** — POSTs a JSON payload to a configured URL

## Health API

The daemon exposes a health endpoint on a Unix socket at `/tmp/cfgd.sock`. Query it with `cfgd daemon --status` to get:

- Whether the daemon is running
- Last reconcile time
- Drift count
- Per-source sync status (when using multi-source config)

## CLI Commands

```sh
cfgd daemon                # run in foreground
cfgd daemon --install      # install as launchd (macOS) or systemd (Linux) service
cfgd daemon --status       # check running state, last reconcile, drift count
cfgd daemon --uninstall    # remove the service
```

## Service Management

`cfgd daemon --install` creates a native service definition:

- **macOS**: LaunchAgent plist in `~/Library/LaunchAgents/`
- **Linux**: systemd user unit in `~/.config/systemd/user/`

The service is configured to start at login and restart on failure.
