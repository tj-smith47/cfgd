# Package Managers

cfgd manages packages across 18 package managers (Homebrew manages taps, formulae, and casks as separate sub-managers). Each is implemented behind a trait, so the reconciler works the same way regardless of which managers are available. You can also define custom script-based managers for tools that don't fit any built-in manager.

## Supported Managers

| Manager | Platforms | Config Key | What It Does |
|---|---|---|---|
| Homebrew | macOS, Linux | `brew` | Manages taps, formulae, and casks separately |
| apt | Debian/Ubuntu | `apt` | `apt-get install` with sudo handling |
| dnf | Fedora/RHEL 8+ | `dnf` | `dnf install` |
| yum | RHEL 7/CentOS 7 | `yum` | `yum install` |
| pacman | Arch/Manjaro | `pacman` | `pacman -S` |
| apk | Alpine | `apk` | `apk add` |
| zypper | OpenSUSE | `zypper` | `zypper install` |
| pkg | FreeBSD | `pkg` | `pkg install` |
| Cargo | Any (with Rust) | `cargo` | `cargo install` |
| npm | Any (with Node) | `npm` | `npm install -g` |
| pipx | Any (with Python) | `pipx` | `pipx install` |
| Snap | Linux (with snapd) | `snap` | `snap install` |
| Flatpak | Linux (with flatpak) | `flatpak` | `flatpak install` |
| Nix | Any (with Nix) | `nix` | `nix profile install` |
| Go | Any (with Go) | `go` | `go install` |
| winget | Windows | `winget` | Windows Package Manager (Microsoft Store + winget repo) |
| Chocolatey | Windows | `chocolatey` | Community package manager; cfgd bootstraps it automatically |
| Scoop | Windows | `scoop` | User-directory installs; cfgd bootstraps it automatically |

Package managers that aren't installed on the current system are silently skipped. `cfgd apply --dry-run` shows which managers will be used and which packages will be installed or removed.

## npm global-install prefix

Global npm installs (`npm install -g`) write into npm's configured `prefix`.
On a system where Node came from a package manager (apt, dnf, an msi), that
prefix is root-owned (e.g. `/usr/local`), so an unelevated `npm install -g`
fails with `EACCES`. cfgd resolves a writable prefix once per operation and
applies the same one to `install`, `uninstall`, `update`, and both listing
calls, so state never drifts between what got installed and what cfgd sees
as installed:

1. If `npm_config_prefix` / `NPM_CONFIG_PREFIX` is already set in the
   environment, cfgd leaves it alone.
2. If cfgd is running elevated, cfgd leaves npm's prefix alone.
3. Otherwise cfgd asks npm for its configured prefix and write-probes it
   (create-and-remove a temp entry, never a mode-bit read, which lies under
   ACLs and is meaningless on Windows). A writable answer is used as-is.
4. If the probe fails, cfgd falls back to `$HOME/.npm-global`, creating it if
   absent, and passes `--prefix $HOME/.npm-global` on the npm command line.

The first time the fallback is used, `cfgd apply` prints a one-time notice
naming the fallback prefix. Nothing is asked of you: `$HOME/.npm-global` is a
directory cfgd created, so its `bin` directory is written into the generated
env file (`~/.cfgd.env`) like every other `PATH` entry cfgd owns, whether cfgd
installed npm itself or you did.

Once resolved, the decision (prefix + whether it was the fallback) is
persisted in cfgd's state store and reused by every later `install` /
`uninstall` / `update` / listing call, so a package installed under one
resolved prefix stays visible even if a later run's live inputs (elevation,
write-probe result, project-local npm config) would resolve differently.
Revalidation covers both directions automatically: a persisted prefix that
becomes unwritable is discarded and re-resolved, and while cfgd is on the
fallback it re-checks npm's configured prefix on each resolve: fix the
permissions that pushed npm onto `$HOME/.npm-global` (say, on `/usr/local`)
and the next `install`/`uninstall`/`update`/list call promotes back onto the
configured prefix on its own. Nothing needs to be cleared by hand.

## Reaching a manager cfgd bootstrapped mid-apply

A manager cfgd installs during an apply lands in a prefix that did not exist
when the `cfgd` process started, so nothing in the inherited `PATH` names it.
cfgd records that manager's `PATH` directories the moment the bootstrap
returns and uses them for the rest of the run, which is what makes this
sequence work in a single `cfgd apply` on a machine with none of it installed:

```yaml
packages:
  brew:
    formulae:
      - pipx        # brew is bootstrapped, then installs pipx
  pipx:
    packages:
      - pynvim      # resolves through brew's prefix, same apply
```

A manager that is not on the machine yet is provisioned in the `Prerequisites`
phase, which runs before any package work:

```
Phase: Prerequisites
  cfgd:managers
    - refresh apt index
    - provision nix via nix installer
```

so every prefix an install needs exists before the `Packages` phase starts.
Those manager nodes are a graph (a provision waits for the tool it shells out
to, and for the manager it installs through) and everything whose edges are
satisfied provisions at the same time. A node whose dependency failed does not
run at all; its line names the failure that stopped it.

Managers one mediator delivers by an ordinary package install collapse onto a
single node, and a single command:

```
Phase: Prerequisites
  cfgd:managers
    ✓ provision npm, pipx via apt (12.4s)
```

is one `apt-get install nodejs npm pipx`, not two `apt-get` runs queued behind
each other for the dpkg lock. The line names every manager the command
delivers, and `--skip` / `--only` / `--phase` still address them one at a time
(`--skip prerequisites.npm` leaves `provision pipx via apt` behind). Only a
plain install collapses: a manager that bootstraps through a vendor script
(`brew` via the Homebrew installer, `npm` via `nvm`, `cargo` via `rustup`)
keeps its own node and its own command. Provisions that stay separate but share
a mediator still take that mediator's lane and run one at a time.
Inside `Packages`, work runs one lane per manager family concurrently. The lane
is per *family* rather than per name because `brew`, `brew-tap` and `brew-cask`
drive one binary: formulae, taps and casks queue behind each other so only one
`brew` process ever runs.

**The `via` on a provision line is binding, not a preview.** cfgd resolves the
mediator while planning (that is the manager named on the line you read, and
the lane the node is serialized on) and execution runs exactly that one:

```
Phase: Prerequisites
  cfgd:managers
    ✗ provision npm via apt — apt could not install npm: exit code 100: E: Unable to locate package nodejs
```

If the named mediator has gone away or its install fails, the provision fails
naming it. cfgd does not fall through to whatever else happens to be installed:
a substitute would run outside the lane the node holds (two dpkg-class installs
at once is exactly what the lane prevents) and would install through a manager
the line never mentioned. Re-run to re-plan against the host as it is now.

For the same reason a manager is only planned through a mediator this host can
actually run: on a machine with none of them, cfgd says the manager cannot be
provisioned and why, instead of naming one and failing on it.

The same directories reach lifecycle scripts (see
[lifecycle-scripts.md](lifecycle-scripts.md)), the generated env file, and the
environment of every package-manager command cfgd runs afterwards, so a
`postApply` step, an `npm install` that shells out to `node`, and your next
login shell all resolve the binary identically.
Your *current* shell is the one exception: it predates the env file, which is
why `cfgd apply`, `cfgd init --apply*`, and `cfgd module create --apply` all end
by naming the file to source.

## Index refresh

cfgd refreshes the package index of every manager that is already on the machine,
has work in this run, and keeps a local index at all. The refresh is an action of
its own in the `Prerequisites` phase, so it is named in the plan before it happens
and reported where it ran:

```
Phase: Prerequisites
  cfgd:managers
    ✓ refresh apt index (1.0s)
      Hit:1 http://deb.debian.org/debian stable InRelease
      Reading package lists... Done
```

A refresh touches METADATA ONLY. cfgd never upgrades a package you did not
declare on its way to installing one you did, so a manager whose only "update"
command is a machine-wide upgrade (`npm update -g`, `pipx upgrade-all`,
`choco upgrade all -y`, `winget upgrade --all`, a bare `snap refresh`) gets no
refresh action at all. Neither does a manager that resolves its remote on every
install and so has no index to go stale: `cargo`, `go`, `nix`. Where a manager
has both forms, cfgd runs the metadata half: `scoop update` (bucket manifests,
not `scoop update *`) and `flatpak update --appstream -y` (remote metadata, not
a bare `flatpak update -y`). Nothing in the plan or the tree claims a refresh
that never ran.

| Refreshed | Not refreshed (no local index) |
|---|---|
| `apt`, `dnf`, `yum`, `zypper`, `pacman`, `apk`, `pkg`, `brew`, `scoop`, `flatpak`, a custom manager declaring `update:` | `cargo`, `go`, `nix`, `npm`, `pipx`, `snap`, `chocolatey`, `winget`, `brew-cask`, `brew-tap` |

`brew-cask` and `brew-tap` are the same binary and the same index as `brew`, so
the family is refreshed once by `brew` rather than three times.

Filters filter: a run that leaves the phase out (`--phase packages`) or drops one
node from it (`--skip prerequisites.apt`) does not refresh that index behind your
back. The refresh is the phase's, so excluding the phase excludes the refresh.

The rule holds for anything else that narrows a run: a per-module daemon tick
(`reconcile.modules`) and a package withheld awaiting a source decision both take
the refresh with them once nothing left in the run reads that index.

A refresh that fails is reported as a warning naming the manager that failed and
leaves its line `unchanged`; it never fails the run, because a stale index is a
reason for an install to be out of date, not a reason to stop.

A manager cfgd cannot provision on this host says so in the same phase, naming
the cause rather than disappearing from the run:

```
Phase: Prerequisites
  cfgd:managers
    ✗ cannot provision pipx — pip3 is missing and apt does not install it under that name
```

## Profile Usage

```yaml
packages:
  brew:
    taps:
      - homebrew/cask-fonts
    formulae:
      - git
      - ripgrep
    casks:
      - visual-studio-code
  apt:
    packages:
      - build-essential
      - curl
  cargo:
    - bat
    - eza
  npm:
    global:
      - typescript
  pipx:
    - httpie
  dnf:
    - gcc
  winget:
    - Microsoft.VisualStudioCode
    - Git.Git
    - Mozilla.Firefox
  chocolatey:
    - nodejs
    - python
    - 7zip
  scoop:
    - ripgrep
    - fd
    - bat
```

## Two equivalent forms: list or struct

Every package manager accepts **both** a bare list of package names and a struct
with named fields. The two forms below are identical:

```yaml
# list form: shortest; the list maps to the manager's primary package list
packages:
  flatpak: [org.gnome.Calculator, com.spotify.Client]
```

```yaml
# struct form: same packages, plus access to manager-specific knobs
packages:
  flatpak:
    packages: [org.gnome.Calculator, com.spotify.Client]
    remote: flathub
```

This holds uniformly: `cargo: [bat, eza]` equals `cargo: {packages: [bat, eza]}`,
`apt: [curl]` equals `apt: {packages: [curl]}`, `nix: [hello]` equals
`nix: {packages: [hello]}`, and so on for all 18 managers. The bare list maps to
each manager's primary list (`packages` for most, `global` for npm, `formulae`
for brew):

```yaml
packages:
  npm: [typescript]          # == npm: {global: [typescript]}
  brew: [git, ripgrep]       # == brew: {formulae: [git, ripgrep]}
```

Use the struct form when you need a manager's extra fields: brew `taps`/`casks`,
a `file` manifest (Brewfile, package.json, Cargo.toml, apt list), flatpak `remote`,
or snap `classic`. The struct form still rejects unknown keys, so a typo like
`flatpak: {packges: [...]}` is reported loudly rather than silently dropped.

## Windows Package Managers

### winget

Windows Package Manager (`winget`) manages packages from the Microsoft Store and the winget community repository. Package IDs use the `Publisher.Package` format.

```yaml
spec:
  packages:
    winget:
      - Microsoft.VisualStudioCode
      - Git.Git
      - Mozilla.Firefox
```

### chocolatey

Chocolatey is a community-driven Windows package manager. cfgd bootstraps it automatically if it isn't installed.

```yaml
spec:
  packages:
    chocolatey:
      - nodejs
      - python
      - 7zip
```

### scoop

Scoop installs programs to your user directory without requiring elevated privileges. cfgd bootstraps it automatically if it isn't installed.

```yaml
spec:
  packages:
    scoop:
      - ripgrep
      - fd
      - bat
```

> **Case-insensitive matching.** `winget`, `chocolatey`, and `scoop` treat package
> names case-insensitively, so `Wget` and `wget` refer to the same package. cfgd
> matches your declared name against installed state without regard to case: a
> package listed as `Wget` in your profile stays converged even though `choco list`
> reports it as `Wget` and the tracking key is normalized to `wget`. The Unix-side
> managers (`apt`, `dnf`, `brew`, `cargo`, `npm`, `pipx`) are case-**sensitive**:
> declare those exactly as the manager expects.

## Module Packages

In [modules](modules.md), packages use cross-platform resolution instead of manager-specific lists:

```yaml
packages:
  - name: neovim
    minVersion: "0.9"
    prefer: [brew, snap, apt]
    aliases:
      snap: nvim
```

cfgd picks the first available manager that satisfies the version constraint, using `aliases` to map package names where they differ.

A `minVersion` is a standing declaration, not a one-time resolution check: every drift surface (`cfgd diff`, `cfgd status --scan`, `cfgd verify`, and each of their `--module` scoped forms) compares the version the manager reports INSTALLED against the floor, so a package that ages out of its constraint is drift rather than convergence. A manager that cannot state an installed version (apk, pacman, zypper and FreeBSD `pkg` list names only) makes that floor unanswerable: the surfaces report it as a check that could not run and exit `1`, never as clean. The same holds for a version stated in a form nothing can compare against (a `git-20240101` snapshot tag, say), and for the DECLARATION itself: a `minVersion` written in a form its manager cannot read (`>=1.2`, `1.2.x`) is reported as a check that could not run rather than as a package permanently below its floor. A leading `v` is not such a form: `minVersion: "v1.2.0"` is the same floor as `1.2.0`.

Distro managers do not report upstream versions: `apt` states `vim` as `2:8.2.3995-1ubuntu2`, where `2:` is dpkg's epoch and `-1ubuntu2` the distro's own packaging revision. Neither part is the software's version, so for the distro families (`apt`, `dnf`, `yum`, `apk`, `pacman`, `zypper`) cfgd compares the upstream part alone: `minVersion: "8.2"` is met by `2:8.2.3995-1ubuntu2`.

Homebrew states its own packaging fields the same way: a formula carries the tap's revision as `neovim 0.12.5_1` and a cask the vendor's build as `1.2.3,4567`. Those are compared on the upstream part too, so `minVersion: "0.11"` is met by `0.12.5_1`. Semver-native managers (`cargo`, `npm`, `pipx`, `go`, …) keep full semver ordering, prereleases included, so `1.0.0-rc1` still loses to `minVersion: "1.0.0"`.

## Declarative removal

cfgd tracks the packages it installs. When a package leaves the desired set (you remove it from a profile, or remove the last module that required it) the next full `cfgd apply` (and the daemon's reconcile loop) uninstalls it through the owning manager.

Removal is deliberately conservative:

- **Only packages cfgd installed are ever removed.** A package already present the first time cfgd would have installed it is treated as pre-existing: cfgd never recorded it, so it is never uninstalled, even if it appears in no profile. cfgd will not remove software you installed yourself.
- **Shared packages survive until the last consumer is dropped.** The desired set is the merge of the active profile and *all* its modules, so a package required by more than one module is removed only when the final module that wants it is removed.
- **Only a full apply prunes.** A scoped run (`--module`, `--phase`, `--only`, `--skip`) never uninstalls, because it sees only part of the desired set and a package it omits may still be needed by something not applied this run.
- **Tracking self-heals.** If a tracked package is removed out of band, cfgd drops its tracking on the next full apply, so it is not "re-removed" or otherwise acted on.
- **Custom managers can prune even after their definition is deleted.** Built-in managers derive their uninstall command from code, but a custom (scripted) manager's uninstall lives only in its config block, so cfgd handles it specially:
  - **Persist + delete-block flow.** cfgd persists the uninstall script alongside each package a custom manager installs. If you later delete the whole custom-manager block, the next full apply (and the daemon reconcile) still runs the persisted script to remove its packages, then drops the tracking.
  - **Legacy rows have no script.** A package tracked by a custom manager *before* this behavior existed has no persisted script: cfgd reports it (it cannot guess how to remove it) and leaves it for you to remove manually, rather than silently dropping the tracking.
  - **`--dry-run` previews both cases.** `cfgd apply --dry-run` prints `would uninstall orphaned <manager>/<pkg> via persisted script` for packages it can prune, and `orphaned <manager>/<pkg> — no persisted uninstall; manual removal needed` for legacy rows.

```yaml
# before: jq is installed by cfgd
packages:
  brew:
    formulae: [git, jq]
```
```yaml
# after: drop jq, then `cfgd apply` → brew uninstall jq (cfgd installed it; nothing else wants it)
packages:
  brew:
    formulae: [git]
```

## Version Queries

Each manager supports querying available package versions without installing:

| Manager | How version is queried |
|---|---|
| apt | `apt-cache policy <pkg>` — Candidate line |
| brew | `brew info --json=v2 <pkg>` — stable version |
| dnf | `dnf info <pkg>` — Version field |
| pacman | `pacman -Si <pkg>` — Version field |
| apk | `apk policy <pkg>` |
| snap | `snap info <pkg>` — latest/stable channel |
| npm | `npm view <pkg> version` |
| pipx | PyPI JSON API |
| cargo | `cargo search <pkg> --limit 1` |
| winget | `winget show --id <pkg>` — Version field |
| chocolatey | `choco info <pkg>` — Title line |
| scoop | `scoop info <pkg>` — Version field |

## Dry Run

`cfgd apply --dry-run` shows the full package plan without making changes. `toolbox`
below is a custom manager declaring an `update:` command, which is why it takes a
refresh node of its own:

```
Plan
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  pkgdemo
  Phases   Prerequisites, Packages

Phase: Prerequisites
  cfgd:managers
    - refresh brew index
    - refresh toolbox index

Phase: Packages
  profile:pkgdemo
    - brew install ripgrep
    - toolbox install delta
    - toolbox uninstall beta
    - skip absent: 'absent' not available — cannot auto-install on this platform

◉ 6 actions planned
```

Every manager's work is one line per operation: an install names the manager and the
packages it takes in one call, an uninstall names what leaves the desired set, and a
manager that is neither present nor installable is a `skip` line carrying the reason
rather than a silent omission.

A package already at its desired version produces no action, so it gets no line:
the plan lists what would change, not the full inventory.

## Adding packages from the CLI

`--package` mirrors the schema path it writes to, so a sub-list is reachable without opening
the file:

```sh
cfgd profile update work --package brew:ripgrep              # spec.packages.brew.formulae
cfgd profile update work --package brew.taps:charmbracelet/tap  # spec.packages.brew.taps
cfgd profile update work --package brew.casks:firefox        # spec.packages.brew.casks
cfgd profile update work --package snap.classic:code         # spec.packages.snap.classic
cfgd profile update work --package ripgrep                   # the platform's native manager
```

A prefix that names no manager is refused rather than taken as a package name. See the
[CLI reference](cli-reference.md#package-tokens) for the full grammar, and for
`cfgd profile update --package` / `cfgd module update --package`.
