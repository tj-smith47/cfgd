# Reconciliation Model

cfgd follows the same pattern as Kubernetes controllers: declare desired state, diff against actual state, generate a plan, apply it, watch for drift. You never tell cfgd "install ripgrep": you declare "ripgrep should be installed" and cfgd figures out what needs to change.

![cfgd catching and healing drift](../demo/cfgd-drift.gif)
*A file edited outside cfgd is caught by `cfgd status`, explained by `cfgd diff`, and healed by `cfgd apply`.*

## Phases

Apply runs in a fixed phase order:

1. **Modules**: modules skipped because they do not apply to this host (a `platform:` gate that excluded it), reported before any work starts
2. **Pre-Scripts**: `preApply` or `preReconcile` hooks (context-dependent)
3. **Prerequisites**: everything the run needs before it can install anything. Refresh the index of each package manager that keeps one, provision the managers that are missing (and install the tools their installers shell out to), then write env vars, shell aliases, and the PATH entries cfgd recorded as its own for a package manager (bootstrapped by cfgd, or a prefix cfgd created for it during an install) to `~/.cfgd.env`, and inject shell rc source lines
4. **Packages**: install/uninstall across all package managers
5. **Files**: copy, template, set permissions
6. **System**: shell, macOS defaults, launch agents, systemd units, gsettings, kdeConfig, xfconf, environment, Windows registry, Windows services, sysctl, kernelModules, containerd, kubelet, apparmor, seccomp, certificates
7. **Secrets**: decrypt SOPS files, resolve external provider references
8. **Post-Scripts**: `postApply` or `postReconcile` hooks, `onChange` hooks

**A module's work applies in the phase whose kind it is.** A module's packages are in
`Packages` beside the profile's, its files in `Files`, its lifecycle scripts in
`Pre-Scripts`/`Post-Scripts`. The `Modules` phase holds only the modules that were skipped,
so `cfgd apply --phase files` deploys module-sourced files, and `--phase packages` installs
module-declared packages.

**Files precedes System** so a file is on disk before anything that consumes it: a unit file
deployed through `files:` exists before `systemctl enable` names it.

Within a phase, work is grouped by **owner** (the thing that declared it), and each group
is labelled `kind:name`:

| Owner | Means |
|---|---|
| `profile:<name>` | declared by the active profile |
| `module:<name>` | declared by that module |
| `cfgd:managers` | package-manager work cfgd runs on its own initiative: an index refresh, a manager it provisions, a tool that provisioning needs |
| `cfgd:env` / `cfgd:session` | the generated env file / the live-session refresh |

Groups read profile-first, then `cfgd:`, then modules by name. cfgd's own three groups
read producer-before-consumer: `cfgd:managers` creates the binaries, `cfgd:env` publishes
where they live, `cfgd:session` broadcasts it. **Execution order in `Packages` is
deliberately not the displayed order**: module-owned package work runs first, then
profile-owned package work, so a module's dependency is present before a module's own
hooks need it. `Prerequisites` is the other exception: its `cfgd:managers` group is a
graph, described below. Everywhere else, execution follows the displayed order.

Those three tiers are also a barrier: a tier starts only once every action in the tier
above it has *finished*. Inside a tier, package work runs **concurrently, one lane per
package manager family**, so `brew install` and `apt install` proceed at the same time
while a single manager still runs one operation at a time. Each row's `(23.8s)` is that
lane's own span; the closing line's total is wall-clock time and says so (`(278.2s wall)`),
which is why the rows of a concurrent phase can add up to more than the run took. A *family* is the managers
sharing one binary: `brew`, `brew-tap` and `brew-cask` are three names for one `brew`, so
they share a lane and cfgd never runs two `brew` processes at once. Three more rules narrow
that:

- A module's packages wait for the packages of every module it `depends` on.
- An action for a manager cfgd has to install first drains the phase: it runs alone, and
  nothing else starts until it finishes, because the install changes `PATH` for everything
  after it.
- An owner already holding a lane takes a second one only after every other owner with
  ready work has taken one, so two modules share the machine rather than one filling it.
  A lone owner still fills every lane.

The concurrency bound is the number of distinct manager families with work in the phase.
There is nothing to tune: a machine that declares only `brew` packages still runs one
`brew` at a time, and one that declares `brew`, `apt` and `cargo` runs three.

Index refreshes and manager provisioning are actions in the `Prerequisites` phase, named in
the plan and reported where they ran; see [packages.md](packages.md#index-refresh). A run
that filters that phase out does not refresh behind your back: the refresh belongs to the
phase you excluded, and a run narrowed some other way (a per-module daemon tick, a
package awaiting a source decision) keeps only the refreshes its surviving work reads.
A refresh that fails is a warning and never fails the run.

Those actions are a **graph**, not a list, and they run across the same family lanes: an
index refresh, a tool the provisioning shells out to, and the provisioning itself are
edges cfgd plans (`apt(index) → curl(tool) → brew → npm`), and everything whose edges are
satisfied runs at once. The "runs alone" rule above does NOT apply here: it exists to
serialize around an install that changes `PATH` mid-`Packages`, and a manager that is
missing is exactly what this phase is for. What still holds is one operation per manager
family, so two nodes never drive one binary at once. **A node whose dependency failed
never runs**: it is reported as a failure naming the root cause (`did not run — brew
failed earlier in this phase`), never as a silent success. `cfgd:env` and `cfgd:session`
run after that group finishes, in order, because they publish what it created.

On a terminal the live region **is** the phase's tree, drawn while it happens: each
action takes a row the moment the scheduler has something to say about it, and that row
then changes state in place (waiting, running with its command's output beneath it,
settled) without ever moving.

```text
Phase: Prerequisites
  cfgd:managers
    ✓ refresh apt index                       (9.5s)
    ⠹ provision brew via homebrew installer
        ==> Downloading and installing Homebrew…
    ○ provision pipx via apt — waiting on refresh apt index
```

A row held back is dimmed and names the row holding it, with a verb that names the relation.
A row behind something it needs is `waiting on` it: a node it depends on (`provision npm via
brew — waiting on provision brew via homebrew installer`), or its family's source
registration, which runs first because the formula may only exist in the repository the tap
adds (`brew install gum — waiting on brew-tap install charmbracelet/tap`). A row that only
needs its family's lane is `queued behind` the action occupying it (`brew-cask install
firefox — queued behind brew install neovim`), depending on nothing that action produces. A node held behind more than one edge
names the last of them to finish, so the line never has to take back what it said. A line
with no action in front of it (`waiting on modules`) stands for its whole group, held by
the tier barrier. Waiting is a live state only: it is never logged, never in `-o json`,
and never in the scrollback a settled row leaves behind.

Rows are appended in dispatch order and settle in place, so **the order you read is the
order the work started**, whatever order it finished in. What scrolls into the permanent
scrollback reads in exactly the order the screen did.

On a terminal too short for the phase, settled rows give up their screen lines so the
running rows at the foot stay visible. A single muted line at the top says how many are
in that state:

```text
  … 5 settled rows held for commit
  cfgd:managers
    ⠹ brew install neovim
    ⠹ cargo install just
```

No line is lost: every held row is still written to the scrollback, in dispatch order,
once the rows ahead of it commit, and the count falls as they land. Running rows and
group headings are never given up, and the count also covers an action that never got a
line at all, such as one swept before it ran by a failed dependency. Each group's heading
is written once: it opens as that group's first action is dispatched and stays open until
the group can gain no more rows, so a late-arriving row is still filed under its owner.

Off a terminal (a pipe, a log, `-o json`, `--quiet`) nothing is drawn live; the phase
writes its tree in plan order when it closes.

Each phase can be applied independently with `cfgd apply --phase <name>`; `--phase modules`
selects every module-owned action in every phase. A phase-scoped apply only touches the
surfaces that phase owns: manager provisioning and index refresh both belong to the
`Prerequisites` phase, so `--phase packages` performs no manager work at all: an install
whose manager is not yet available is reported as blocked on it rather than bootstrapping
it on the spot.

A full apply needs no second run for that: the plan already folds a to-be-provisioned manager's
declared PATH directories into the `Prerequisites` phase's `~/.cfgd.env` write, so for most
managers the file is correct before `Packages` even runs. The one manager whose install location
is only knowable once its bootstrap finishes (npm's global prefix) still converges inside the same
apply: cfgd re-derives the file once every phase completes and the real directory is recorded.

**What a `postApply` script sees in the env files.** The `Prerequisites` phase runs long before
`Post-Scripts`, so every `spec.env` value cfgd could resolve up front is already in `~/.cfgd.env`
when a post-script reads it. The re-derivation described above is the exception: it runs after
*every* phase, `Post-Scripts` included, because its two inputs only exist once the phases have run
(a value resolved from a `secrets:` reference, decrypted in the `Secrets` phase, and a package
manager's real install directory when it differs from what the plan declared).
A `postApply` script that reads `~/.cfgd.env`, `~/.config/environment.d/cfgd.conf` or a shell rc
file for one of those two therefore observes the file as it was *before* this run's re-derivation,
and sees the new value only from the next run on. Read a secret-backed variable in a post-script
through the secret itself rather than through the generated env file.

## Apply vs Reconcile Context

cfgd distinguishes between user-initiated apply and daemon-initiated reconciliation:

- **Apply context** (`cfgd apply`, `cfgd plan`): runs `preApply`/`postApply` hooks
- **Reconcile context** (daemon auto-reconcile): runs `preReconcile`/`postReconcile` hooks

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
  Phases   Prerequisites, Packages, Files, System, Post-Scripts

Phase: Prerequisites
  cfgd:managers
    - refresh brew index
    - provision nix via nix installer

Phase: Packages
  profile:work
    - brew install extra-tool
    - nix install hello
  module:nvim
    - brew install neovim (0.12.4)

Phase: Files
  profile:work
    - create ~/.gitconfig
  module:nvim
    - deploy 2 files
      ~/.config/nvim/init.lua      — symlink
      ~/.config/nvim/lua/opts.lua  — symlink

Phase: System
  profile:work
    - set sysctl.net.core.somaxconn: 4096 → 8192

Phase: Post-Scripts
  module:nvim
    - postApply: nvim --headless "+Lazy! sync" +qa

Backups (run on apply)
  ◉ mydata

◉ 9 actions planned
```

The header block states the scope every line below is read against: which
config and profile produced the plan, which modules are in play, which phases
hold in-scope work, and, on an executing run (`cfgd apply`), an
`Actions  N planned` row in place of the closing count.

The `Packages` bullets are the group order in miniature: the profile's own
installs, then `module:nvim`. Execution reverses those two (see the note above).

## Filtering

```sh
cfgd apply --phase packages              # single phase
cfgd apply --phase modules               # every module-owned action, in every phase
cfgd apply --phase prerequisites.managers  # one owner group within a phase
cfgd apply --module nvim                 # nvim + deps, isolated from the profile
cfgd apply --module nvim --with-profile  # full profile PLUS nvim
cfgd apply --only packages.brew          # dot-notation filter (the brew manager)
cfgd apply --only packages.module:nvim   # a module's package work
cfgd apply --skip module:nvim            # one module, every phase
cfgd apply --skip cfgd:managers          # every index refresh and manager cfgd provisions
cfgd apply --skip prerequisites.session  # skip the live-session broadcast
cfgd apply --skip prerequisites.brew     # skip one manager (family-collapsed)
cfgd apply --skip system.sysctl          # skip specific items
```

The owner segment (`module:nvim`) is what keeps a module named `brew` distinct from the
`brew` package manager: `--only packages.brew` selects the manager, `--only
packages.module:brew` selects the module. The pre-routing spellings `modules` and
`modules.<name>` still work and print a deprecation naming their replacement.

`--phase`/`--skip`/`--only` also take the dotted grammar one level up, scoped to a
single phase: `<phase>.<selector>`, where the selector names an owner group
(`managers`, `env`, `session`: the three `Prerequisites` always carries), a
manager (family-collapsed, so `prerequisites.brew` also covers `brew-tap`/`brew-cask`,
but never a prerequisite tool a manager's installer merely depends on), or that
tool itself: `curl` is keyed on its own name, `prerequisites.curl`, not on
whichever manager needed it.
`prerequisites.managers` is the whole-group equivalent of `cfgd:managers`, scoped to
that one phase. Managers one mediator delivers by an ordinary package install share
a single node (`provision npm, pipx via apt`), and a manager selector still names
exactly one of them: `--skip prerequisites.npm` leaves `provision pipx via apt`
behind, `--phase prerequisites.pipx` provisions `pipx` alone. A selector is only valid scoped to `prerequisites`; naming one after
any other phase (`--phase packages.brew`) errors rather than silently matching
nothing, and points at the phase the selector actually belongs to.

Skipping a manager's bootstrap (`prerequisites.managers`, `prerequisites.brew`,
`cfgd:managers`) leaves the installs that needed a provisioned manager in the plan:
cfgd cannot drop those on your behalf, so it strands them, warns, and prints the
`--skip packages.<manager>` flags that would drop them too. A prerequisite tool that
existed only for the skipped manager's own bootstrap is pruned along with it, silently.
The opposite direction is silent too: dropping the last package install that named a
manager prunes that manager's now-purposeless bootstrap node with no warning, since
nothing in the plan needs it anymore.

`--only` never prunes for lack of consumers, in either case above: an `--only`
selector is explicit selection, so `--only prerequisites.managers` (the recovery
command the stranding warning itself prints) keeps every manager bootstrap node
even though it empties `Packages` of every install that used to justify them. The
consumer-prune is a `--skip`-side behavior only.

## Failure Handling

Failed actions within a phase do not abort the entire apply. They are logged, skipped, and reported at the end. A broken Homebrew tap does not prevent your SSH config from being placed.

## State Store

cfgd tracks state in a SQLite database at `~/.local/state/cfgd/state.db` (Linux; see the file-locations table in `configuration.md` for macOS/Windows). This is what lets cfgd detect drift, show history, and know what it is responsible for.

**What cfgd tracks:**

| Category | What's stored | Used for |
|---|---|---|
| **Apply history** | Timestamp, profile, status (success/partial/failed), summary | `cfgd log`, rollback context |
| **Drift events** | What changed, expected vs actual value, whether it was resolved | `cfgd status`, daemon notifications |
| **Managed resources** | Every file, package, and setting cfgd is responsible for | Knowing what to diff on next reconcile |
| **Module state** | Per-module install time, package/file hashes, git source commits | Detecting when a module is outdated |
| **Source tracking** | Per-source fetch time, commit, version, sync status | Multi-source sync and conflict history |
| **Pending decisions** | Unresolved recommended/optional items from source updates | `cfgd decide`, daemon policy |

### What a drift row names

Every producer of a drift row (the daemon's reconcile tick, `cfgd status --scan`,
`cfgd diff`, `cfgd verify`) writes the same identity for the same finding, and an
apply clears the rows for the work it just did. One row stands for one thing you
can check:

| Kind | Row identity | Example |
|---|---|---|
| Module file | `<module>/<target>` | `nvim/home/u/.config/nvim/init.lua` |
| Package | `<manager>:<package>` | `brew:ripgrep` |
| System setting | `<configurator>.<key>` | `sysctl.net.ipv4.ip_forward` |
| Managed file | the target path | `/etc/hosts` |

A module that declares five files has five rows, not one: a row that stood for a
whole module could never be cleared by a per-file check, so a module you had just
deployed kept reporting drift. A module's packages are rows of the package kind
for the same reason, spelled exactly as a package you declared in a profile is:
the check that clears them is the same one. The package row is spelled in the
manager's own naming (a Go package `rsc.io/2fa` is `go:2fa`, the name `go list`
reports), so the producer that records the row and the check that clears it always
agree.

### Rows an older daemon wrote

Before cfgd settled on the identities above, a reconcile tick could record a
whole-module row (`module|nvim`) or a batched package row
(`package|brew:jq,ripgrep`). Nothing writes either spelling now, and no check can
re-find one, so `cfgd status`, `cfgd diff` and `cfgd verify` leave them standing
rather than clearing a finding they never looked at — rendered beside the live
findings under their stored `expected`/`actual`, which keeps `cfgd status
--exit-code`, `cfgd diff --exit-code` and `cfgd verify --exit-code` all at `5`.
A `--module` run leaves standing only the rows its own scope owns (a bare
module id matching the module chain, a package id the module's own resolved
set claims); a row outside that scope is neither rendered nor priced by the
narrower check. A running daemon clears them on its next tick (the rows it can
no longer account for are resolved with the plan it just ran). On a machine
that never runs `cfgd daemon`, start it once in the foreground and stop it
after its first tick:

```bash
cfgd daemon run    # ctrl-c after the first reconcile
```

A block cfgd skipped (a package manager that is not available, a system
configurator this host does not have) keeps its row through an apply: the run did
not converge it, so nothing about it was proven. The next reconcile that can check
it is what clears it.

A module skipped whole (a platform gate) records no row at all, because nothing
under it was probed: the skip says
something about the host, not about a resource that diverged. Neither tree draws
a block for it: the header's `Modules` row names it and the reason
(`Modules  git, nvim (mac skipped: platform not matched (requires: macos))`),
so a daemon tick's closing count names only rows you can see. It is counted
nowhere either: the header's `Actions N planned`, the apply's rollup clauses and
that closing count all price the actions this run will attempt, and a module
skipped whole is not one. `-o json` still lists the skip under its phase, with
its kind and reason.

A module whose FILE work is refused is the opposite case, and reads differently
on purpose. When an entry declares `encryption.mode: Always` under a `Symlink`
or `Hardlink` strategy, or its source is not encrypted with the declared
backend, or the encryption check cannot run, cfgd will not deploy that module's
files — but the module itself is fine and its other phases proceed. That is a
finding you have to act on, so it is an action row like any other: counted by
`Actions N planned`, drawn under `Phase: Files` in both trees with its reason,
and settled by the apply as a warning-roled skip the rollup counts.

```
Phase: Files
  module:ssh
    ⚠ cannot deploy files — file ~/.config/cfgd/modules/ssh/id_rsa requires encryption (backend: sops) but is not encrypted
```

It still records nothing: cfgd refused to write the files rather than finding
them diverged, so there is no `Managed Resources` row and no drift row under it.
`-o json` spells it as its own action kind (`refuse`).

The same holds for an action this host declined outright — the live-session
publish on a box with no session manager. It is drawn in the plan and apply
trees with its reason, but it put nothing on the machine, so it leaves no
`Managed Resources` row and `cfgd status` lists no `cfgd:session` owner there.

## Provenance Tracking

When using [multi-source config](sources.md), every action carries an `origin` field so the
plan shows where each change comes from. A source-delivered action ends with ` <- <source>`;
your own declarations carry no suffix, because "local" is the absence of a source rather
than a source of its own:

```
Phase: Packages
  module:dev-tools
    - brew install ripgrep (15.2.0) <- team
  module:localmod
    - brew install jq (1.8.2)
```

The suffix is carried by each action rather than by the owner heading, so a plan mixing
local and source-delivered work reads its provenance line by line.
