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

## Reconcile Patches

Override reconcile settings for specific modules or profiles. Patches live in your `cfgd.yaml` — you control your machine's sync behavior regardless of what upstream profiles or modules recommend.

```yaml
spec:
  daemon:
    reconcile:
      interval: 5m
      drift-policy: NotifyOnly
      patches:
        - kind: Module
          name: certificates
          interval: 1m
          drift-policy: Auto
        - kind: Module
          name: shell-theme
          interval: 1h
          auto-apply: false
        - kind: Module
          interval: 30s
        - kind: Profile
          name: base
          auto-apply: true
```

Each patch targets by `kind` (`Module` or `Profile`). When `name` is provided, the patch applies only to that entity. When `name` is omitted, the patch applies to all entities of that kind (kustomize semantics). Named patches take priority over kind-wide patches. Override any combination of `interval`, `auto-apply`, and `drift-policy`. Omitted fields inherit from the next level up.

### Precedence

Most specific wins, fields resolve independently:

```
Named Module patch > Kind-wide Module patch > Named Profile patch > Kind-wide Profile patch > Global
```

When multiple Profile patches match the inheritance chain (e.g., `base` and `work`), the leaf profile (the active one) wins — consistent with how profile inheritance resolves other conflicts.

### Conflict resolution

| Scenario | Result |
|---|---|
| Module patch and Profile patch both set `auto-apply` | Module patch wins |
| Two Profile patches in inheritance chain set `interval` | Leaf profile wins |
| Module patch sets `drift-policy: Auto`, global is `NotifyOnly` | Module patch wins |
| Same module patched twice in the list | Last entry wins (warning logged) |
| Patch references a module/profile that doesn't exist | Silently ignored |

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
