# Modules

Modules are self-contained, portable configuration packages. A module bundles everything needed for one tool (cross-platform packages, local or git-sourced config files, lifecycle scripts) into a single deployable unit.

For the complete field-by-field reference, see the [Module spec reference](spec/module.md).

## Why Modules

Without modules, profiles declare packages by manager: `brew: [neovim]`, `apt: [neovim]`. This means no portability (a profile for macOS doesn't work on Ubuntu), no granularity (you can't apply only your nvim setup), and no dependency tracking (nvim needs Node.js for LSP but that's implicit).

### Modules vs. Profile Packages

Use **modules** when the config is self-contained and shareable: a tool with its own config files, dependencies, and setup scripts. Use **profile packages** for machine-specific package lists that don't need to be portable or shared.

Rule of thumb: if you'd share it with a coworker or use it across machines with different OSes, it's a module. If it's "install these five tools on my work laptop," it's a profile package list.

## Module Spec

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  version: 1.4.0        # optional; strict semver, the module's own release version
spec:
  depends: [node, python]

  packages:
    - name: neovim
      minVersion: "0.9"
      prefer: [brew, snap, apt]
      aliases:
        snap: nvim

    - name: ripgrep

    - name: fd
      aliases:
        apt: fd-find
        dnf: fd-find

    - name: pynvim
      prefer: [pipx]

    - name: neovim
      prefer: [npm]

  files:
    - source: config/
      target: ~/.config/nvim/

    - source: https://github.com/user/nvim-config.git@v2.1.0
      target: ~/.config/nvim/

  env:
    - name: EDITOR
      value: nvim

  aliases:
    - name: vim
      command: nvim

  scripts:
    postApply:
      - nvim --headless "+Lazy! sync" +qa
      - nvim --headless -c "MasonInstallAll" -c "qa"
```

### Module Version

`metadata.version` is the module's own release version: strict semver (`1.4.0`, `2.0.0-rc.1`),
never a `v` prefix and never a two-part `0.10`. It is optional; modules without it load unchanged.

Declare it on any module you release: the workflow written by `cfgd workflow generate` cuts the tag
`<name>/v<version>` when the module changes, fails the job when the version is missing rather than
guessing one, and fails it again if that tag already exists (bump the version; published tags are
never rewritten). Read it back with:

```sh
cfgd module show nvim -o jsonpath='{.metadata.version}'
```

New modules from `cfgd module create` start at `0.1.0`.

### Module-Level Platform Filter

`spec.platforms` gates the **whole module**. When it is non-empty and the current platform matches
none of the listed tags, the entire module is skipped: packages, files, scripts, env, and aliases
included. Tags match the platform's OS (`linux`, `macos`, `freebsd`, `windows`), distro, or arch.
The canonical macOS token is `macos` (not `darwin`). A skipped module shows up as a **Skipped**
action in the plan rather than vanishing, and an active module may not `depends` on a module that
is skipped on the current platform (that is a configuration error).

Gating comes at three levels, each the same field with the same tag vocabulary:

| Level | Field | Gated-out behavior |
|-------|-------|--------------------|
| Module | `spec.platforms` | The module is skipped whole, and shows as a **Skipped** action. |
| Package | `spec.packages[].platforms` | The package is not installed; `cfgd module show` lists it as `skipped (platform filter)`. |
| Entry | `spec.env[].platforms`, `spec.aliases[].platforms` | The entry is absent from this machine's desired state entirely; the document surfaces annotate it `(platforms: macos)`. |

Use `spec.platforms` for a wholly platform-specific module, the per-package
[`platforms`](#package-entry-fields) field when only some packages within a cross-platform module
are platform-specific, and the per-entry one when a single variable or alias is.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: mac-desktop
spec:
  platforms: [macos]
  packages:
    - name: rectangle
```

A cross-platform module gates single entries instead:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  env:
    - name: PATH
      value: $HOME/.local/bin:$PATH
    - name: PATH
      value: /opt/homebrew/opt/ruby/bin:$PATH
      platforms: [macos]
  aliases:
    - name: pbcopy
      command: xclip -selection clipboard
      platforms: [linux]
```

The two `PATH` declarations concatenate on macOS (`PATH` is the one name whose surviving
declarations fold together rather than replace one another); on Linux only the first applies, and
`pbcopy` is installed there and nowhere else.

### Package Entry Fields

| Field | Required | Type | Description |
|---|---|---|---|
| `name` | yes | string | Canonical package name |
| `minVersion` | no | string | Minimum acceptable version (semver). Also checked LIVE, by the whole-machine surfaces (`cfgd diff`, `cfgd status --scan`, `cfgd verify`) and by their `--module` scoped forms alike: an installed copy below the floor is drift (`want: 0.9, have: 0.8.3`), and a version the manager cannot state (or states in a form nothing can compare against, or fails to spawn its own comparator) is a check that could not run. A manager that cannot raise a package in place at all (`brew-tap`, a config-declared scripted installer) reports a below-floor package as a check error rather than drift, since no apply could ever heal it; every other manager raises through its own upgrade or install verb. `apk`, `pacman`, `zypper` and FreeBSD `pkg` list installed names only, and `brew-tap` entries are repositories with no version to state — a `minVersion` against any of these is always such a check |
| `prefer` | no | list | Ordered list of managers to try. `"script"` uses the `script` field as a custom installer. If omitted, the available manager that already holds the package wins (the platform's native manager is asked first), and a package nobody holds installs through the native manager. |
| `deny` | no | list | Managers to never use for this package, even if available and preferred |
| `aliases` | no | map | Per-manager name overrides when the package name differs |
| `script` | no | string | Inline shell script or path. Used when `prefer` includes `"script"` |
| `creates` | no | string | Idempotency guard for a `prefer: [script]` install: skip the script if this path exists. Ignored for manager-backed installs |
| `onlyIf` | no | string | Idempotency guard for a `prefer: [script]` install: run only if this command exits zero. Ignored for manager-backed installs |
| `unless` | no | string | Idempotency guard for a `prefer: [script]` install: run only if this command exits non-zero. Ignored for manager-backed installs |
| `platforms` | no | list | Platform filter — skip on non-matching platforms. Values: OS (`linux`, `macos`), distro (`ubuntu`, `fedora`, `arch`), or arch (`x86_64`, `aarch64`) |

### File Entry Fields

| Field | Required | Type | Description |
|---|---|---|---|
| `source` | yes | string | Local path (relative to module dir), or git URL |
| `target` | yes | string | Absolute target path on the machine |

File entries also accept `strategy` (`Symlink`/`Copy`/`Template`/`Hardlink`/`Patch`), `permissions`, `private`, `encryption`, and `patch`, with the same semantics as profile managed files. See [spec.files[]](spec/module.md#specfiles) for the full table.

`encryption` is enforced before anything is written. If an entry demands
`mode: Always` under a `Symlink` or `Hardlink` strategy, or its source is not
encrypted with the declared backend, cfgd refuses that module's file work and
says so on its own row — `cannot deploy files — <reason>`, counted in the plan
and drawn as a warning (⚠) by both the plan and the apply. The module's
packages, scripts and env still apply; only the deploy is withheld. See
[Reconciliation](reconciliation.md) for how the row is counted and reported.

### Env Vars

Modules can declare env vars in their spec. These are merged with the profile's env vars during reconciliation. On a name conflict, the module's value wins over the profile's value.

```yaml
spec:
  env:
    - name: NVIM_APPNAME
      value: my-nvim
    - name: EDITOR
      value: nvim
```

#### What expands in a value

cfgd quotes every value it writes into a shell startup file, so a value can hold
quotes, backslashes, spaces, `#`, and even newlines without breaking the file or
running anything. What each shell still expands at startup differs:

| Declared value | bash / zsh | fish | PowerShell | `environment.d` (Linux `envScope: All`) |
|---|---|---|---|---|
| `$HOME/bin` | expands | literal | literal | expands |
| `${EDITOR}` | expands | literal | literal | expands |
| `$env:USERPROFILE` | literal | literal | expands | literal |
| `$(id)` | literal | literal | literal | literal |
| `` `id` `` | literal | literal | literal | literal |

A declared reference like `PATH: /opt/bin:$PATH` therefore picks up the surrounding
environment on bash/zsh and under systemd, and is a literal string on fish and
PowerShell: write the full path there, or declare a per-platform value. Command
substitution never runs, on any platform: cfgd is not a place to compute a value.

```yaml
spec:
  env:
    # /opt/bin prepended to the inherited PATH on bash/zsh and systemd;
    # the literal text "/opt/bin:$PATH" on fish and PowerShell.
    - name: PATH
      value: /opt/bin:$PATH
```

### Aliases

Modules can declare shell aliases. These are merged with profile aliases using the same conflict rules as env vars: module wins on conflict by name.

```yaml
spec:
  aliases:
    - name: vim
      command: nvim
    - name: vimdiff
      command: nvim -d
```

An alias `command` is quoted the same way and follows the same table: it runs when
you invoke the alias, never while the shell is loading its startup files.

## Cross-Platform Package Resolution

For each package entry, cfgd picks the right manager for the current machine:

```
┌─────────────────────┐
│ Package entry        │
│ name: neovim         │
│ prefer: [brew, snap] │
│ minVersion: 0.9      │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐     ┌──────────────┐
│ Try brew             │────→│ Available?   │── no ──→ try next
│ (resolve alias)     │     │ Version ≥ 0.9?│── no ──→ try next
└─────────────────────┘     └──────┬───────┘
                                   │ yes
                                   ▼
                            ┌──────────────┐
                            │ Use brew     │
                            └──────────────┘

If no candidate satisfies → interactive prompt with all options
```

### Resolution Algorithm

The full resolution logic for each package entry:

1. **Platform filter.** If `platforms` is non-empty and the current OS, distro, or arch doesn't match, the entry is skipped entirely.
2. **Determine candidate managers.** If `prefer` is specified, walk that list in order: an authored list is a statement about WHICH manager, and it is honoured even when another manager already holds the package. If `prefer` is omitted, a bare entry means "this package on this machine": the available manager that already reports it installed wins (the platform's native manager is asked first, then the others by name), so a tool that brew put on an Ubuntu host is not installed a second time through `apt`. Only when no manager holds it does the platform's native manager apply (e.g., `apt` on Ubuntu, `brew` on macOS).
3. **For each candidate manager:**
   - If the candidate is `"script"`, the `script` field must be present (error if missing). Scripts are always considered "available," and version checks are skipped (the script manages its own versioning). See [Script Execution](#script-execution) below.
   - Otherwise, check that the manager is installed and available on this machine. If not, skip to the next candidate.
   - Resolve the package name: use `aliases[manager]` if present, otherwise fall back to `name`.
   - If `minVersion` is specified, query the manager for the available version. If the package is not found or the version is below the minimum, skip this manager.
   - If all checks pass, the manager is selected.
4. **If no candidate satisfies:** cfgd collects all available managers and their versions, then presents an interactive prompt:
   ```
   Package 'neovim' (minVersion: 0.9) could not be resolved automatically.
   Available options:
     [ ] apt — neovim 0.6.1 (below minimum)
     [ ] snap — nvim 0.10.2
     [ ] brew — neovim 0.10.2 (not installed, can bootstrap)
   Select managers to use, or skip:
   ```
   You can select one or more, or skip the package (it will be recorded as skipped in the plan).
5. **When `prefer` has multiple entries and no `minVersion`:** the first available manager wins. No version check is needed.

### Version Comparison

Version strings are normalized to semver: `"0.9"` becomes `"0.9.0"`, `"18"` becomes `"18.0.0"`. This lets cfgd compare versions from different package managers consistently, even when they report versions in different formats.

### Cross-Scope Deduplication

A package declared in more than one scope (the profile and a module, or two modules) installs **once**. cfgd dedupes the combined install set keyed on `(manager, name)`: the module side contributes its alias-resolved name, the profile side its name as literally declared (profiles have no alias mechanism). A module that aliases a package away from the profile's literal name therefore does not collide, and both install.

- **Same manager + same name across scopes** → installed once; the duplicates are dropped.
- **Different managers** → both install. `ripgrep` via `brew` in the profile and via `cargo` in a module are two distinct installs.
- **Module installs win** over profile duplicates, and an **earlier module wins** over a later one. Module-owned package work is dispatched ahead of profile-owned work inside the Packages phase, so a module's own `postApply` script can rely on the package already being present.
- **`prefer: [script]` entries are never deduped.** Two same-named install scripts may differ, so both always run (subject to each entry's own `creates`/`onlyIf`/`unless` guards).
- Dedup is **silent**: no warning is emitted for a dropped duplicate.

```yaml
# profile.yaml
spec:
  packages:
    brew: [gh]          # declared here

# modules/gh-auth/module.yaml
spec:
  packages:
    - name: gh          # ...and here, same manager
```

`gh` installs once (the `gh-auth` module's install runs; the profile entry is dropped).

### Script Execution

When `prefer: [script]` is selected (or `"script"` is reached in the prefer list), cfgd runs the package's `script` field as a custom installer. The script can be inline shell or a path to a script file relative to the module directory.

The script runs with the following environment:

- **Working directory:** the module directory
- **`$CFGD_MODULE_NAME`:** name of the current module
- **`$CFGD_PACKAGE_NAME`:** canonical package name
- **`$HOME`:** user's home directory
- **Shell:** `/bin/sh -e` (exits on first error)

Example:

```yaml
packages:
  - name: custom-tool
    prefer: [script]
    script: |
      curl -fsSL https://example.com/install.sh | sh
```

**Idempotency.** A `prefer: [script]` install has no installed-package set to
query, so cfgd cannot detect whether the tool is already present: it is
invisible to drift/`verify`, and **without a guard the script runs on every
apply** (reported as changed). Make the script idempotent, either internally
or by attaching a `creates`/`onlyIf`/`unless` guard to the package entry. The
guards share the [lifecycle-script semantics](#script-lifecycle): they are
evaluated before the script (`creates` → `onlyIf` → `unless`, all must permit
running), and any guard that says "skip" turns the install into a no-op
reported as unchanged.

```yaml
packages:
  - name: rustup
    prefer: [script]
    creates: ~/.cargo/bin/rustc   # skip if rustc already installed
    script: |
      curl -fsSL https://sh.rustup.rs | sh -s -- -y
```

### Platform Detection

cfgd detects the current OS, distro, and architecture, then maps to the native package manager:

| Distro | Native Manager |
|---|---|
| macOS | brew |
| Ubuntu, Debian | apt |
| Fedora, RHEL 8+ | dnf |
| RHEL 7, CentOS 7 | yum |
| Arch, Manjaro | pacman |
| Alpine | apk |
| OpenSUSE | zypper |
| FreeBSD | pkg |

## Dependency Resolution

Modules declare `depends: [node, python]`. cfgd builds a dependency graph and installs dependencies before the modules that need them. Circular dependencies are detected and reported as errors. If two modules share a dependency (A→C, B→C), it's resolved and installed once.

Processing order: leaf dependencies first (node, python), then dependents (nvim).

Every command reporting on a resolved profile carries the same annotation convention on its
`Modules` header row: `Modules git, nvim (depends: plugins)` when a `depends` entry pulled in a
module that neither the profile nor the invocation named directly, no annotation at all when
resolution added nothing beyond the declared list. See [Profiles → Inheritance](profiles.md#inheritance)
for the `Profile` row's identical rule.

A module the resolution gated off this host follows the `depends:` one as a further clause in the
same annotation: `Modules  git, nvim (depends: plugins, rectangle skipped: platform not matched
(requires: macos))` — the name leaves the list, so the clause is where the reader is told why it
is missing. The run itself lists the module as a planned, skipped action carrying the same reason;
nothing is installed for it and no drift row is recorded against it.

## Script Lifecycle

Modules support lifecycle hooks that run at different points during apply and reconciliation. Scripts can be inline commands or file paths (relative to the module directory).

| Hook | When it runs |
|---|---|
| `preApply` | Before the module's packages and files are applied |
| `postApply` | After all of the module's packages are installed and files are deployed |
| `preReconcile` | Before the module is reconciled by the daemon |
| `postReconcile` | After daemon-initiated reconciliation of the module |
| `onChange` | After apply/reconcile, only if this module's resources actually changed |
| `onDrift` | In the daemon, when drift is detected in this module's own resources |

`onDrift` scripts are observability, not remediation: they fire before the daemon decides how to handle the drift (`autoApply`, notify, or prompt), regardless of the drift policy. A module's `onDrift` fires only when that module's own packages, files, or scripts drift, on both whole-profile and per-module reconcile ticks. Profiles also have `onDrift` (see the [Profile spec reference](spec/profile.md#specscripts)); the two are independent.

Each entry can be a simple string (`"scripts/rebuild-index.sh"`) or a full object:

```yaml
scripts:
  postApply:
    - run: scripts/rebuild-index.sh
      timeout: 5m            # default 2m
      idleTimeout: 30s       # kill after 30s with no output
      continueOnError: true
      creates: ~/.cache/index.db   # skip when this path exists
```

The guards make a script re-run-safe: `creates` skips when a path exists, `onlyIf` runs only when a command exits zero, `unless` only when it exits non-zero. Set `interactive: true` to attach a script to the terminal so it can prompt the user; it requires a TTY and is skipped with a warning when none is present (CI, piped stdin, or the daemon). See the [Module spec reference](spec/module.md#specscripts) for the complete field reference, defaults, and environment variables available to scripts.

## Profile Integration

Profiles declare which modules to use via the `modules` field. Module packages and profile-level packages coexist. If the same package appears in both, the module's version constraint and preference take priority (a module is more specific than a profile package list).

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work-mac
spec:
  modules: [nvim, tmux, git, zsh]

  # Existing fields still work; modules don't replace them
  packages:
    brew:
      formulae: [extra-tool]
  files:
    managed:
      - source: gitconfig
        target: ~/.gitconfig
```

Registry modules use `<source>/<module>` syntax:

```yaml
spec:
  modules:
    - nvim              # local module
    - community/tmux    # from "community" registry
```

## Git File Sources

File sources can be git URLs instead of local paths:

```yaml
files:
  - source: https://github.com/user/repo.git           # default branch, full repo
  - source: https://github.com/user/repo.git@v2.1.0    # pinned to tag
  - source: https://github.com/user/repo.git?ref=dev   # track a branch
  - source: https://github.com/user/repo.git//subdir    # subdirectory of repo
  - source: git@github.com:user/repo.git@v2.1.0         # SSH with tag
```

Git sources are cached in `~/.cache/cfgd/modules/` (Linux; under the cache dir on every platform, see `configuration.md`) and updated on `cfgd apply` or daemon sync.

cfgd honors your local git configuration when cloning and fetching, so
`url.<base>.insteadOf` rewrite rules, `http.proxy`, and similar settings apply.
For example, a global rule that rewrites SSH URLs to HTTPS will be respected:

```sh
git config --global url."https://github.com/".insteadOf git@github.com:
```

cfgd runs git non-interactively (no credential prompts) and clears the credential
helper, so authentication relies on your SSH agent / keys for SSH URLs and an
already-configured token for HTTPS. Pinned tags (`@v2.1.0`) and signature
verification are unaffected by these rewrites.

## Module Directory Structure

Modules live in the `modules/` directory of your config repo:

```
my-config/
  modules/
    nvim/
      module.yaml
      config/         # local file source
        init.lua
        lua/
    tmux/
      module.yaml
      config/
        tmux.conf
    node/
      module.yaml     # packages only, no files
```

## Module Registries

Registries are git repos that host multiple reusable modules: community or organization collections you browse and install from instead of writing everything yourself.

This is different from [config sources](sources.md), which provide full profiles with policy enforcement. Registries are plain directories of modules, with no policy tiers.

```
# Registry repo structure
modules/
  tmux/
    module.yaml
    files/
  nvim/
    module.yaml
    files/
```

Configure registries in cfgd.yaml or via CLI:

```sh
cfgd module registry add https://github.com/cfgd-community/modules.git
cfgd module registry add https://github.com/myorg/modules.git --name myorg
cfgd module registry list
cfgd module registry remove community
```

A registry URL may be any git URL, or the GitHub shorthand `owner/repo`:

```sh
# GitHub shorthand: expands to https://github.com/cfgd-community/modules.git,
# and the registry name defaults to the org (`cfgd-community`)
cfgd module registry add cfgd-community/modules

# Any git URL, on any host. Only GitHub URLs can supply a default name, so
# name a registry on another host with --name
cfgd module registry add https://gitlab.example.com/myorg/modules.git --name myorg
cfgd module registry add git@git.example.com:myorg/modules.git --name myorg
```

The shorthand follows the same rules as [naming a source](sources.md#naming-a-source):
a dotted first segment is a host, not a GitHub owner, and an existing local path wins
over the shorthand. Registries on non-GitHub hosts need `--name` (only a GitHub URL can
supply a default name).

### Registry Tag Convention

Registries use per-module git tags in the format `<module>/<version>` (for example `tmux/v1.0.0`, `nvim/v2.3.1`), so a single git repo can host multiple modules with independent version histories. When you install a module at a specific version, cfgd checks out the tag matching that module name.

### Module Source Configuration

Configure module registries in your `cfgd.yaml`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
      - name: myorg
        url: https://github.com/myorg/modules.git
```

The source name defaults to the GitHub org or user name extracted from the URL. Override with the `name` field or `--name` flag on the CLI.

Reference registry modules in profiles:

```yaml
spec:
  modules:
    - nvim              # local module
    - community/tmux    # from "community" registry
```

## Module Status and Drift

`cfgd status` includes a `Component Health` section with one row per owner
holding managed state, modules among them:

```
Component Health (checked 3m ago)
  ✓ profile:work — Synced (2 packages, 1 file)
  ✓ cfgd:env     — Synced (1 env file)
  ✓ module:nvim  — Synced (3 packages, 12 files, 2 scripts)
  ○ module:git   — NotApplied
```

The heading's annotation dates the recorded drift verdicts: how long ago the
machine was last checked, or `(drift never checked)` when no scan has ever
run. The counts are taken from the rows the `Managed Resources` table below
paints rather than from any declaration, so a health line and the rows under
it cannot disagree; a kind an owner holds none of is dropped rather than
rendered as `0`, and an owner holding nothing reads its bare verdict. The
verdict leads and the counts are its parenthetical: a `Failed` or `Drifted`
module reads as one at the head of the line rather than behind three numbers
a healthy module carries too.

Each line is headed by the module's owner token: the same `module:<name>` the
plan and apply trees head that module's group with.

Each module is tracked independently. cfgd stores a hash of the resolved package list and deployed file tree. When the daemon runs its reconciliation loop, it checks:

- **Package drift:** are all resolved packages still installed at the expected versions?
- **File drift:** do deployed files still match the source content?
- **Git source drift:** for modules with git file sources, have new commits appeared upstream since the last apply?

A module reads as one of five states:

| State | Meaning | Where it can appear |
|---|---|---|
| `Synced` | converged, with every check behind it answered | any status surface |
| `Drifted` | a live scan found a package missing or a file diverged | only `--scan` (and `--exit-code`, which implies it) |
| `Unknown` | a check of its own could not run, so no verdict was reached | any surface reporting an erroring check |
| `Failed` | its last apply had a failing action | any status surface |
| `NotApplied` | no apply has recorded it | any status surface |

`Unknown` outranks `Drifted`, the same ranking `--exit-code` encodes when it
exits `1` (a check that could not run) ahead of `5` (drift): what a check never
answered is not a state the report may summarize away.

The `-o json` payload's `status` field carries the stored token instead
(`installed`, `error`, `not applied`).

`cfgd status --module <name>` reports the declared counts and the drift a scan
found; `-o wide` itemizes each surface instead, and `--show-values` adds the
declared values and full script bodies (see
[`cfgd status`](cli-reference.md#cfgd-status)).

Module resources are first-class in compliance reporting, not profile-only. A module's files, packages, and system settings appear in every `cfgd compliance` surface (snapshot, export, diff, history), attributed to their module, and are counted into the compliance summary a device check-in reports: the same effective profile-plus-modules view that `cfgd verify` and `cfgd diff` use. Module file checks are content-aware: a deployed module file present on disk but whose bytes drifted from its source is reported as a violation.

## Plan Output Format

`cfgd plan` shows module actions in the phase whose kind they are, with resolved managers and file deployments:

```
Plan
  Config   ~/.config/cfgd/cfgd.yaml
  Profile  work
  Modules  nvim
  Phases   Prerequisites, Packages, Files, Post-Scripts

Phase: Prerequisites
  cfgd:managers
    - refresh apt index
    - refresh brew index

Phase: Packages
  profile:work
    - brew install extra-tool
    - apt install sl, cowsay
  module:nvim
    - brew install neovim (0.12.4)
    - npm install neovim (5.4.0, alias: neovim-npm)
    - pipx install pynvim (0.6.0)

Phase: Files
  profile:work
    - create ~/.gitconfig
  module:nvim
    - deploy 2 files
      ~/.config/nvim/init.lua      — symlink
      ~/.config/nvim/lua/opts.lua  — symlink

Phase: Post-Scripts
  module:nvim
    - postApply: nvim --headless "+Lazy! sync" +qa
    - postApply: nvim --headless -c "MasonInstallAll" -c "qa"

◉ 13 actions planned
```

A module's package line names the manager that won resolution, the manager-specific package
name being installed, and the version that manager reports. When the module entry's own
name differs from the manager-specific one, it follows after `alias:`:
`npm install neovim (5.4.0, alias: neovim-npm)` installs npm's `neovim` for a module entry
named `neovim-npm`. A profile's own package lines carry neither, since a profile names a
manager and a package directly. A package line names every operand it acts on, wrapping
under the row's own indent when the list is wider than the terminal rather than being cut
short. A deploy row states a count instead (`deploy 2 files`, never naming its targets in
the subject): every file it writes enumerates as its own child row beneath it,
`<target> — <method>`, naming the resolved `symlink`/`copy`/`template`/`hardlink`/`patch`
strategy, in manifest order. A deploy of only the files that changed says how many of the
module's declared set it left alone as the row's detail (`deploy 1 file — 5 already
deployed`). The apply row carries the same detail and the same child rows, beside the
same subject.

Each phase groups its actions by the owner that declared them (`profile:<name>`,
`module:<name>`), so a bullet's owner is visible without reading the action text.
Module-owned package work is dispatched before profile-owned work in the Packages
phase, whatever order the two read in.

## Lockfile

Remote modules (from registries or direct git URLs) are tracked in `modules.lock`. This ensures every machine gets the exact same module version, even if the upstream repo has moved forward. A module becomes "locked" the moment you install it from a remote source.

```yaml
modules:
  - name: tmux
    url: "https://github.com/cfgd-community/modules.git@tmux/v1.0.0"
    pinnedRef: "tmux/v1.0.0"
    commit: "abc123def456"
    integrity: "sha256:..."
    subdir: modules/tmux
```

The `integrity` field is a sha256 hash of the module directory contents. cfgd verifies this hash on every apply to detect tampering or corruption. The lockfile is written atomically (write to a temp file, then rename) to prevent partial writes from corrupting the lock state.

A locked module is resolved by its recorded `commit`, not by `pinnedRef`. A commit is immutable, so once the module cache holds it, every later run resolves the module from the cache with no network access: repeated applies and daemon ticks on a machine whose cache is warm fetch nothing. A cache that cannot answer the pin (a first run on a new machine, or a cache that was cleared) is populated once, then behaves the same way.

Use `cfgd module upgrade` to move to a newer version.

### Fetch behavior

A module source that names a mutable ref (a branch, or a tag someone may move) is fetched rather than read from the cache. Within one process, each repository is transferred once and every module, file, or locked entry naming that repository reads the same snapshot: one `git fetch` brings over every ref, so a second look would learn nothing. A `cfgd apply` or `cfgd plan` run is one such process and always starts with a fetch.

A source that pins nothing follows its default branch: each fetch moves its files onto whatever the branch now points at, so an upstream commit lands on the next run without re-adding the module. Pin a commit (`@<sha>`) or a tag (`@v1.2.0`) to hold a source still; `?ref=<branch>` follows that branch the same way the default one is followed.

The daemon is long-lived, so the same repository is re-fetched at most once every 30 seconds. With the default `interval: 5m` every tick fetches. Setting an interval below 30s does not fetch faster than that: a module tracking a branch converges within 30 seconds either way.

## Modules from Config Sources

[Config sources](sources.md) can deliver module bodies via `spec.provides.modules` in their `cfgd-source.yaml` manifest. This makes the source a **module library** in addition to (or instead of) providing profiles. The `provides.modules` list is the delivery allow-list: only modules named there are made available to subscribers.

Resolution order when a profile references a module by name:

1. **Local modules** (`<config-dir>/modules/`) always win over source-delivered modules.
2. **Source priority**: when the module exists in multiple subscribed sources, the higher-priority source wins. Equal priority is tie-broken alphabetically by source name.

Referencing a module that is neither local nor offered by any subscribed source is a **fatal error**. A source-delivered module's plan lines end with the delivering source, so its provenance is visible where its work is:

```
Phase: Packages
  module:dev-tools
    - brew install ripgrep (15.2.0) <- team
  module:localmod
    - brew install jq (1.8.2)
```

`cfgd source show <name>` lists the modules a source offers:

```
Show source:team-config
  URL                 https://github.com/team/config
  ...

Modules
  ◉ dev-tools
  ◉ shell
```

A source that delivers only modules (no profiles) is valid; see [Source-Delivered Module Bodies](sources.md#source-delivered-module-bodies) for the full contract.

## CLI Commands

```sh
cfgd module list                    # list modules and their status
cfgd module show nvim               # show details: packages, files, deps, resolved managers
cfgd module show nvim --show-values # reveal full env variable values (masked by default)
cfgd module create my-tool          # create a new local module
cfgd module update nvim --package ripgrep  # modify a module
cfgd module edit nvim               # open in $EDITOR
cfgd module delete nvim             # restore adopted files, delete module
cfgd module delete nvim --purge     # remove deployed target files, delete module
```

The same discover → edit → preview → apply loop drives every authoring command:

![the CLI authoring loop: explain a field, update the config, plan, apply](../demo/cfgd-author.gif)

### File Adoption

When you create a module with `--file`, cfgd **adopts** the file: it copies it into the module directory (`modules/<name>/files/`) and replaces the original with a symlink pointing back to the repo copy. This means the file is now version-controlled in your cfgd repo while still accessible at its original location.

`cfgd module delete` reverses this: any target that is still a symlink pointing into the module directory is restored to a regular file before the module is removed. Use `--purge` to instead remove all deployed target files entirely (skipping restoration).

### Adding Modules

Add a local module to your profile, or reference remote modules in your profile YAML:

```sh
cfgd module create nvim                       # create a new local module
cfgd profile update --module nvim              # add local module to active profile
```

For registry or git-hosted modules, pass the reference to `profile update --module` to fetch, lock, and add it in one step:

```sh
cfgd profile update --module community/tmux             # registry module, latest tag
cfgd profile update --module community/tmux@tmux/v2.0   # registry module, pinned tag
cfgd profile update --module https://github.com/jane/cfgd-tmux@v2.0   # git URL
```

The remote-module install prompts for confirmation before writing the lockfile. In non-interactive contexts (CI, Dockerfiles, scripts, `-o json`) pass `-y` / `--yes` (or set `CFGD_YES`) to skip the prompt, and `--allow-unsigned` to install a module without a valid signature when `requireSignatures` is enabled:

```sh
cfgd profile update --module community/tmux --yes
CFGD_YES=1 cfgd profile update --module community/tmux
cfgd profile update --module community/experimental-tool --yes --allow-unsigned
```

You can also reference remote modules directly in your profile YAML; cfgd resolves them on the next apply:

```yaml
spec:
  modules:
    - nvim                                    # local module (from modules/ dir)
    - community/tmux                          # from "community" registry
```

When cfgd encounters a registry reference during apply, it clones or fetches the registry repo, checks out the appropriate tag, copies the module, and creates a lockfile entry.

### Upgrading Modules

Upgrade a locked remote module to a new version (re-fetches from git, updates lockfile):

```sh
cfgd module upgrade tmux                     # latest published version
cfgd module upgrade tmux --ref tmux/v2.0.0   # specific version
```

Without `--ref`, "latest" is the **highest published version tag** for the
module. Module versions are git tags named `<module>/<version>` (e.g.
`tmux/v2.0.0`), and cfgd queries the remote (`git ls-remote --tags`) so a newer
tag is found even when the local cache holds only the installed version. The
lockfile is re-pinned to the full resolved tag. If the repo exposes no
`<module>/v*` tags, the upgrade fails rather than tracking a branch: remote
modules must always resolve to a pinned tag.

### Searching

Search registries for modules matching a query:

```sh
cfgd module search tmux
```

### Apply/Plan by Module

```sh
cfgd apply --module nvim                 # nvim + deps, isolated from the profile
cfgd apply --module nvim --with-profile  # full profile PLUS nvim
cfgd apply --dry-run --module nvim       # preview module changes
```

### Bootstrap a Single Module

```sh
cfgd init --from jane/dotfiles --apply-module nvim                       # GitHub shorthand
cfgd init --from git@github.com:jane/dotfiles.git --apply-module nvim
cfgd init --from https://gitlab.example.com/jane/dotfiles.git --apply-module nvim
```

Clones the repo, finds the module, resolves deps, detects platform, and applies only that module.

## Security

### Signature Verification

Remote modules can be signed with GPG or SSH keys. cfgd verifies signatures when present and supports three trust modes:

- **Verify if present (default).** If a module has a signature, cfgd verifies it. If verification fails, the module is rejected. If no signature is present, the module is accepted with a warning.
- **Require signatures.** All remote module tags must carry a valid GPG/SSH signature. Unsigned or lightweight tags are rejected. Enable this in `cfgd.yaml`:
  ```yaml
  spec:
    modules:
      security:
        requireSignatures: true
  ```
- **Skip verification.** Use `--allow-unsigned` on the CLI to bypass signature checks for a single operation. This is intended for development and testing, not production use.
  ```sh
  cfgd module upgrade community/experimental-tool --allow-unsigned
  ```

### OCI Artifact Signing (cosign)

Modules published to an OCI registry (`cfgd module push`/`pull`/`build`) are signed and verified
with [cosign](https://github.com/sigstore/cosign). cfgd uses two distinct trust models:

- **Keyed (offline PKI).** When you pass `--key`, signing is fully offline: cfgd does **not**
  upload the signature to the public Rekor transparency log, and verification skips the tlog
  lookup. This keeps private module signatures off public infrastructure and works
  non-interactively (CI, headless hosts).
  ```sh
  cfgd module keys generate -d ./keys           # writes keys/cosign.key + keys/cosign.pub
  cfgd module push ./mymod --artifact ghcr.io/org/mymod:v1 --sign --key ./keys/cosign.key
  cfgd module pull ghcr.io/org/mymod:v1 --dir ./out --require-signature --key ./keys/cosign.pub
  ```
  `--key` also accepts a KMS URI (`awskms://`, `azurekms://`, `gcpkms://`, `hashivault://`,
  `k8s://`) or a PKCS#11 URI (`pkcs11:token=...;object=...`, RFC 7512, HSM-backed keys); both
  are passed straight through to cosign. cfgd cannot derive a public key from a sibling
  `cosign.pub` file for these (there is no filesystem path to look next to), so `cfgd module push
  --sign --key <kms-or-pkcs11-uri>` warns and leaves `spec.signature.cosign.publicKey` unset:
  run `cosign public-key --key <uri>` and set it manually if the operator enforces
  `disallowUnsigned`.
- **Keyless (Fulcio/Rekor).** Omit `--key` to sign with a short-lived certificate from the public
  Sigstore infrastructure; the signature is recorded in the Rekor transparency log. Verify with
  certificate identity/issuer constraints:
  ```sh
  cfgd module push ./mymod --artifact ghcr.io/org/mymod:v1 --sign
  cfgd module pull ghcr.io/org/mymod:v1 --dir ./out --require-signature \
    --certificate-identity ci@org.com --certificate-oidc-issuer https://token.actions.githubusercontent.com
  ```

`cfgd module keys rotate` generates a fresh pair, backs up the old keys, and re-signs the artifacts
named in `--artifacts`. SLSA provenance attestations follow the same keyed/keyless split via
`--attest` (push) and `--verify-attest` (pull).

### Lockfile Integrity

The lockfile (`modules.lock`) stores a sha256 hash of each module's directory contents. On every apply, cfgd recomputes the hash and compares it to the locked value. A mismatch means the module content has changed since it was locked: cfgd refuses to apply and reports the discrepancy. Run `cfgd module upgrade` to re-lock at the new content.
