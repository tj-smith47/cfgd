# Reconciliation Model

cfgd follows the same pattern as Kubernetes controllers: declare desired state, diff against actual state, generate a plan, apply it, watch for drift. You never tell cfgd "install ripgrep" — you declare "ripgrep should be installed" and cfgd figures out what needs to change.

## Phases

Apply runs in a fixed phase order:

1. **Modules** — modules skipped because they do not apply to this host (a `platform:` gate that excluded it), reported before any work starts
2. **Pre-Scripts** — `preApply` or `preReconcile` hooks (context-dependent)
3. **Env** — write env vars, shell aliases, and the PATH entries recorded for every package manager cfgd itself bootstrapped to `~/.cfgd.env`; inject shell rc source lines
4. **Packages** — install/uninstall across all package managers
5. **Files** — copy, template, set permissions
6. **System** — shell, macOS defaults, launch agents, systemd units, gsettings, kdeConfig, xfconf, environment, Windows registry, Windows services, sysctl, kernelModules, containerd, kubelet, apparmor, seccomp, certificates
7. **Secrets** — decrypt SOPS files, resolve external provider references
8. **Post-Scripts** — `postApply` or `postReconcile` hooks, `onChange` hooks

**A module's work applies in the phase whose kind it is.** A module's packages are in
`Packages` beside the profile's, its files in `Files`, its lifecycle scripts in
`Pre-Scripts`/`Post-Scripts`. The `Modules` phase holds only the modules that were skipped,
so `cfgd apply --phase files` deploys module-sourced files, and `--phase packages` installs
module-declared packages.

**Files precedes System** so a file is on disk before anything that consumes it: a unit file
deployed through `files:` exists before `systemctl enable` names it.

Within a phase, work is grouped by **owner** — the thing that declared it — and each group
is labelled `kind:name`:

| Owner | Means |
|---|---|
| `profile:<name>` | declared by the active profile |
| `module:<name>` | declared by that module |
| `cfgd:managers` | a package-manager bootstrap cfgd runs on its own initiative |
| `cfgd:env` / `cfgd:session` | the generated env file / the live-session refresh |

Groups read profile-first, then `cfgd:`, then modules by name. **Execution order in
`Packages` is deliberately not that order**: module-owned package work runs first, then
package-manager bootstraps, then profile-owned package work — so a module's dependency is
present before a module's own hooks need it, and a manager is installed before the profile
package that needs it. Everywhere else, execution follows the displayed order.

Each phase can be applied independently with `cfgd apply --phase <name>`; `--phase modules`
selects every module-owned action in every phase. A phase-scoped apply only touches the
surfaces that phase owns: bootstrapping a package manager under `--phase packages` records
its PATH entries but leaves `~/.cfgd.env` and your shell rc files alone. The record is
durable, so the next full `cfgd apply` folds those entries in.

A full apply needs no second run for that: the Env phase runs before Packages, so cfgd
regenerates `~/.cfgd.env` once at the end of the apply that bootstrapped the manager, and
the file is correct when that run finishes.

## Apply vs Reconcile Context

cfgd distinguishes between user-initiated apply and daemon-initiated reconciliation:

- **Apply context** (`cfgd apply`, `cfgd plan`) — runs `preApply`/`postApply` hooks
- **Reconcile context** (daemon auto-reconcile) — runs `preReconcile`/`postReconcile` hooks

Both contexts run `onChange` hooks when actions produce changes. `onDrift` hooks fire only in the daemon's drift detection path, before any reconciliation plan is generated.

The context also reaches `spec.backups[]` hooks as `$CFGD_CONTEXT`: a backup fired by the daemon's [schedule timer](backups.md#daemon-scheduling) sees `reconcile`, one run by `cfgd apply` or `cfgd backup run` sees `apply`. The engine itself is the same either way.

Use `cfgd plan --context reconcile` to preview what the daemon would run.

## Plan Output

`cfgd plan` (or `cfgd apply --dry-run`) shows the full plan before any changes. Use `-o json` for structured output in CI pipelines.

```
Plan
  Config   ~/.config/cfgd/cfgd.yaml
  Profile  work
  Modules  nvim
  Phases   Packages, Files, System, Post-Scripts

Phase: Packages
  profile:work
    - brew install extra-tool
    - apt install ripgrep (14.1.0)
  cfgd:managers
    - bootstrap pipx via pip
  module:nvim
    - snap install nvim (0.10.2)

Phase: Files
  profile:work
    - update /home/you/.gitconfig
  module:nvim
    - deploy /home/you/.config/nvim/init.lua, /home/you/.config/nvim/lua/opts.lua (12 files)

Phase: System
  profile:work
    - set macosDefaults.com.apple.dock.autohide: false → true

Phase: Post-Scripts
  module:nvim
    - postApply: nvim --headless "+Lazy! sync" +qa

Backups (run on apply)
  ⊙ mydata

⊙ 8 action(s) planned
```

The header block states the scope every line below is read against: which
config and profile produced the plan, which modules are in play, which phases
hold in-scope work, and — on an executing run (`cfgd apply`) — an
`Actions  N planned` row in place of the closing count.

The `Packages` bullets are the group order in miniature: the profile's own
install, then `cfgd:managers`' bootstrap, then `module:nvim`. Execution reverses
the first and last of those — see the note above.

## Filtering

```sh
cfgd apply --phase packages              # single phase
cfgd apply --phase modules               # every module-owned action, in every phase
cfgd apply --module nvim                 # single module + deps
cfgd apply --only packages.brew          # dot-notation filter (the brew manager)
cfgd apply --only packages.module:nvim   # a module's package work
cfgd apply --skip module:nvim            # one module, every phase
cfgd apply --skip cfgd:managers          # every package-manager bootstrap
cfgd apply --skip system.sysctl          # skip specific items
```

The owner segment (`module:nvim`) is what keeps a module named `brew` distinct from the
`brew` package manager: `--only packages.brew` selects the manager, `--only
packages.module:brew` selects the module. The pre-routing spellings `modules` and
`modules.<name>` still work and print a deprecation naming their replacement.

Skipping a bootstrap leaves the installs that needed it in the plan; cfgd warns when that
happens and prints the `--skip packages.<manager>` flags that would drop them too.

## Failure Handling

Failed actions within a phase don't abort the entire apply. They're logged, skipped, and reported at the end. A broken Homebrew tap won't prevent your SSH config from being placed.

## State Store

cfgd tracks state in a SQLite database at `~/.local/state/cfgd/state.db` (Linux; see the file-locations table in `configuration.md` for macOS/Windows). This is what lets cfgd detect drift, show history, and know what it's responsible for.

**What cfgd tracks:**

| Category | What's stored | Used for |
|---|---|---|
| **Apply history** | Timestamp, profile, status (success/partial/failed), summary | `cfgd log`, rollback context |
| **Drift events** | What changed, expected vs actual value, whether it was resolved | `cfgd status`, daemon notifications |
| **Managed resources** | Every file, package, and setting cfgd is responsible for | Knowing what to diff on next reconcile |
| **Module state** | Per-module install time, package/file hashes, git source commits | Detecting when a module is outdated |
| **Source tracking** | Per-source fetch time, commit, version, sync status | Multi-source sync and conflict history |
| **Pending decisions** | Unresolved recommended/optional items from source updates | `cfgd decide`, daemon policy |

## Provenance Tracking

When using [multi-source config](sources.md), every action carries an `origin` field ("local" or source name) so the plan output shows where each change comes from:

```
  + brew install git-secrets  <- acme-corp (required)
  ~ EDITOR = "nvim"           <- local (overrides acme-corp)
```
