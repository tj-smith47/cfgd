# CLI Reference

Complete command reference for `cfgd`. All commands respect [global flags](configuration.md#global-flags).

**Every top-level command has an entry here**, and each entry is the same depth: what the command
does in a sentence, a synopsis of its subcommands and arguments, the flags that are specific to it,
and a link to the topic document when one exists. Global flags are listed once, in
[Configuration](configuration.md#global-flags), and are never repeated per command. Semantics
(what a backup run does when a hook fails, how a source's constraints are enforced) live in the
topic document, which links back here; this file does not restate them. `cfgd help <command>` and
`cfgd <command> --help` print the same synopsis from the binary itself.

## Core Commands

### `cfgd generate`

AI-guided configuration generation. Interactively scans your system and generates organized cfgd profiles and modules.

#### Usage

```sh
cfgd generate                      # Full flow: scan, propose structure, generate all
cfgd generate module <name>        # Generate a module for a specific tool
cfgd generate profile <name>       # Generate a profile
```

#### Flags

| Flag | Description |
|---|---|
| `--model <model-id>` | Override AI model (default: from config or `claude-sonnet-5`) |
| `--provider <name>` | Override AI provider (default: claude) |
| `--scan-only` | Only scan dotfiles and shell config; print findings without AI generation |
| `--shell <name>` | Shell to scan for aliases and exports (default: auto-detect from `$SHELL`) |
| `--home <path>` | Home directory to scan (default: `$HOME`) |

The AI scans your installed packages, dotfiles, shell config, and system settings, then proposes a cfgd module and profile structure. Each generated file is shown to you for review before it is written. You can accept, reject, or give feedback. The session ends when all modules and profiles have been written or you exit.

Requires `ANTHROPIC_API_KEY` set in your environment, or `spec.ai.apiKeyEnv` in `cfgd.yaml` to name the environment variable holding the key.

See [ai-generate.md](ai-generate.md) for the full walkthrough, MCP server setup, and troubleshooting.

### `cfgd mcp-server`

Start the MCP server for AI editor integration. Exposes cfgd's scan, inspect, and write tools over the Model Context Protocol (JSON-RPC stdin/stdout).

```sh
cfgd mcp-server
```

The server runs until stdin is closed. Configure your AI client to launch it automatically rather than running it directly. See [ai-generate.md](ai-generate.md#mcp-server-setup) for Claude Code and Cursor setup.

### `cfgd mcp`

Serve cfgd's own CLI as MCP tools, so an assistant can run cfgd rather than only write config for it. Distinct from `cfgd mcp-server`, which serves the generation toolset.

```sh
cfgd mcp claude enable       # register with Claude Desktop (also: vscode, cursor, zed)
cfgd mcp start               # stdio
cfgd mcp stream --port 8080  # streamable HTTP
cfgd mcp tools               # export the tool list to mcp-tools.json
cfgd mcp tools --groups      # list the command groups
```

Serves the `core` group (12 reconcile tools) by default. `--group` / `--command` / `--tool` widen it, the matching `--hide-` flags narrow it, and `--all` serves every group. `cfgd mcp tools --groups` lists the groups and their tool counts. See [Serving the CLI itself](ai-generate.md#serving-the-cli-itself).

### `cfgd init`

Initialize a new cfgd configuration repository.

```sh
cfgd init                                          # interactive setup in current directory
cfgd init ~/dotfiles                               # scaffold in specific directory
cfgd init --from you/config                        # GitHub shorthand for owner/repo
cfgd init --from git@github.com:you/config.git     # clone and scaffold
cfgd init --from https://gitlab.example.com/you/config.git  # any git host
cfgd init --from ~/existing/config                 # use local config directory
cfgd init --from <source> --branch dev                # specify branch
cfgd init --from <source> --apply-profile work-mac    # clone, activate profile, apply
cfgd init --from <source> --apply-module nvim         # clone, apply one module
cfgd init --from <source> --apply --yes --install-daemon  # full one-liner bootstrap
cfgd init --from <source> --apply --yes --on-conflict fail  # stop at the first stranger
```

`--from` accepts any git URL, a local path, or the GitHub shorthand `owner/repo`,
all equally supported. Only a bare `owner/repo` is expanded, and an existing path
always wins over the shorthand: run inside a directory that holds `you/config`,
`--from you/config` means that directory. A first segment carrying a dot
(`gitlab.example.com/you/config`) is a URL for that host and is passed through
untouched; a dotless first segment cannot be told from a GitHub owner by the
value alone, so name a host like `gitserver` with a scheme
(`http://gitserver/config`). The same rule applies everywhere cfgd takes a
repository reference: `cfgd apply --from`, `cfgd plan --from`,
`cfgd source add`, `cfgd source replace` and `cfgd module registry add`.

| Flag | Description |
|---|---|
| `[path]` | Target directory (default: current directory) |
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--branch <name>` | Git branch (default: master) |
| `--name <name>` | Config name in metadata (default: directory name) |
| `--apply` | Apply configuration after scaffolding |
| `--dry-run` | Preview the `--apply` step without applying (used with `--apply`/`--apply-profile`/`--apply-module`) |
| `--apply-profile <name>` | Activate and apply a specific profile (implies --apply, exits `6` if not found) |
| `--apply-module <name>` | Apply a specific module (repeatable, implies --apply, errors if not found) |
| `--on-conflict <ask\|backup\|overwrite\|skip\|fail>` | What the `--apply` step does with a target that already holds a file cfgd never wrote (default `ask`; see [`cfgd apply`](#unmanaged-files-at-a-managed-target)) |
| `--install-daemon` | Install daemon service after init |
| `--theme <name>` | Theme name (default, dracula, solarized-dark, solarized-light, nord, monokai, adventure-time, catppuccin-mocha, gruvbox-dark, tokyo-night, one-dark, minimal) |

`init` never writes over a config directory that already has a `cfgd.yaml`: it
reports `Already initialized at <dir>` and neither clones nor re-scaffolds. With
`--from`, the run continues to the `--apply` / `--apply-module` step against the
existing config; `--name` / `--theme` are applied as overrides.

See [bootstrap.md](bootstrap.md) for the full init flow.

### `cfgd apply`

Apply the configuration plan.

```sh
cfgd apply                          # apply with confirmation
cfgd apply --dry-run                # preview without applying
cfgd apply --yes                    # skip confirmation
cfgd apply --phase packages         # single phase
cfgd apply --phase modules          # every module-owned action, in every phase
cfgd apply --phase prerequisites.managers  # one owner group within a phase
cfgd apply --module nvim            # nvim + deps, isolated from the profile
cfgd apply --module nvim --module tmux   # union of both, still isolated
cfgd apply --module nvim --with-profile  # full profile PLUS nvim
cfgd apply --only packages.brew     # dot-notation filter (the brew manager)
cfgd apply --only packages.module:nvim  # a module's package work
cfgd apply --skip module:nvim       # one module, every phase
cfgd apply --skip cfgd:managers     # every package-manager bootstrap
cfgd apply --skip prerequisites.session  # skip the live-session broadcast
cfgd apply --skip prerequisites.brew     # skip one manager (family-collapsed)
cfgd apply --skip system.sysctl     # skip specific items
cfgd apply --skip-scripts           # apply without running any hooks
cfgd apply --yes --on-conflict backup    # copy every stranger aside, then write
cfgd apply --yes --on-conflict fail      # refuse to touch a file cfgd never wrote
```

| Flag | Description |
|---|---|
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--dry-run` | Preview changes without applying (supports `-o json`) |
| `--phase <name>` | Apply only a specific phase; takes a dotted `<phase>[.<selector>]` path (see below) |
| `--module <name>` | Resolve and apply ONLY this module and its dependencies, isolated from the active profile: every profile-owned contribution (env, aliases, packages, files, system settings, secrets, scripts, backups) is zeroed, not composed. Repeatable: unions several modules |
| `--with-profile` | Compose `--module`'s named module(s) WITH the full active profile instead of isolating them. Rejected (with an error) if passed without `--module` |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Apply only items matching dot-notation paths (repeatable) |
| `--skip-scripts` | Skip all script hooks (pre/post/onChange) |
| `--context <ctx>` | `apply` (default) or `reconcile` — selects which hooks run |
| `--shell <auto\|sh\|bash\|zsh\|pwsh\|cmd>` | Force every *inline* lifecycle script under this interpreter, overriding each entry's own `shell:`. File and shebang scripts are unaffected. For debugging a script that behaves differently under another shell |
| `--on-conflict <ask\|backup\|overwrite\|skip\|fail>` | What to do with a managed target that already holds a file cfgd never wrote (default `ask`) |

#### Unmanaged files at a managed target

A target that already exists but that cfgd has never written (your own `.zshrc`,
a config another tool dropped) is a **conflict**. `--on-conflict` decides what
happens to it:

| Policy | What happens |
|---|---|
| `ask` (default) | Prompt per file. With `--yes`, under `-o json`, or with no terminal at stdin, resolves to `backup` |
| `backup` | Copy the file to `<target>.cfgd-backup`, then write the managed version |
| `overwrite` | Write the managed version, keeping no copy |
| `skip` | Leave the file alone — content *and* permissions; the action is reported as skipped |
| `fail` | Abort the apply (exit `1`) without touching anything |

The prompt offers the same four outcomes, so nothing is reachable only by
re-running with a flag:

```console
$ cfgd apply
⚠ Target exists as unmanaged file: /home/u/.zshrc
? How should cfgd handle this file?
> Backup (copy to <target>.cfgd-backup, then overwrite)
  Overwrite (replace it, keeping no copy)
  Skip (leave the file untouched)
  Abort (stop the apply without touching the file)
```

Interrupting that prompt (Ctrl-C, or Esc) aborts the run; it is never read as
"nobody to ask" and resolved to `backup`.

The copy is reported on the action row that displaces the file, not as a line of
its own ahead of the run: one row per action, naming where the copy landed.

```console
$ cfgd apply --yes
...
    ✓ update /home/u/.config/app/app.conf — backed up to /home/u/.config/app/app.conf.cfgd-backup

$ cfgd apply --yes --on-conflict fail
✗ target exists as unmanaged file: /home/u/.config/app/app.conf (--on-conflict fail)
```

A target that already holds **exactly** the bytes cfgd would write is not a
conflict under any policy: nothing is prompted, nothing is copied aside, and
nothing is rewritten. See [File Safety](safety.md#unmanaged-file-adoption) for
what the backup copy guarantees.

`cfgd init --apply` takes the same flag and runs the same pass: the first apply
on a machine is the one that meets the most files cfgd never wrote. The daemon's
auto-apply runs the same pass under the `backup` policy — it cannot prompt, so
every unmanaged target it meets is copied aside before it is displaced; see
[File Safety](safety.md#what-the-daemon-does-with-a-conflict).

`apply` reconciles exactly what `plan` previews, so a [source item awaiting a
decision](sources.md#automatic-apply-decisions) is not installed by `apply --yes` either.
`cfgd decide` is the only command that resolves one. The same holds for an item your
auto-apply policy declines outright (`newRecommended: Reject`): `plan`, `apply` and the
daemon read one policy, so a manual apply cannot install what the daemon skips. A `Notify`
item (the default) is withheld from the first run that sees it, before any row exists;
`apply` records the row so `cfgd decide` can answer it without waiting for a daemon tick,
while `plan` withholds it read-only.

### `cfgd plan`

Preview the reconciliation plan without applying. This is the canonical preview command; `apply --dry-run` is a convenience that delegates to the same logic.

```sh
cfgd plan                               # preview with default (apply) context
cfgd plan --context reconcile           # preview what the daemon would run
cfgd plan --module nvim                 # nvim + deps, isolated from the profile
cfgd plan --module nvim --with-profile  # full profile PLUS nvim
cfgd plan --phase prerequisites.managers  # one owner group within a phase
cfgd plan --skip prerequisites.session  # skip the live-session broadcast
cfgd plan --skip-scripts                # exclude all script hooks
cfgd plan -o json                       # structured plan output
```

| Flag | Description |
|---|---|
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--phase <name>` | Show only a specific phase; takes a dotted `<phase>[.<selector>]` path (see below) |
| `--module <name>` | Resolve and plan ONLY this module and its dependencies, isolated from the active profile: every profile-owned contribution (env, aliases, packages, files, system settings, secrets, scripts, backups) is zeroed, not composed. Repeatable: unions several modules |
| `--with-profile` | Compose `--module`'s named module(s) WITH the full active profile instead of isolating them. Rejected (with an error) if passed without `--module` |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Plan only items matching dot-notation paths (repeatable) |
| `--skip-scripts` | Exclude all script hooks from the plan |
| `--context <ctx>` | `apply` (default) or `reconcile` — selects which hooks to include |

A module delivered by a [ConfigSource](sources.md) is tagged with its origin
exactly as source-delivered files and packages are: the human plan line ends with
` <- <source>`, and each action in the `-o json`/`-o yaml` payload carries an
`origin` field (omitted for consumer-local modules).

A module's work is planned into the phase whose kind it is, beside the
profile's: packages in `Packages`, files in `Files`, lifecycle scripts in
`Pre-Scripts`/`Post-Scripts`. The `Modules` phase holds only the modules that
were skipped. `dev-tools` below is delivered by the source `team`; `localmod`
is consumer-local and carries no origin.

```sh
$ cfgd plan
Plan
  Config   /home/you/.config/cfgd/cfgd.yaml
  Sources  team
  Profile  work
  Modules  dev-tools, localmod
  Phases   Prerequisites, Packages, Post-Scripts

Phase: Prerequisites
  cfgd:managers
    - refresh brew index

Phase: Packages
  module:dev-tools
    - brew install ripgrep (15.2.0) <- team
  module:localmod
    - brew install jq (1.8.2)

Phase: Post-Scripts
  module:localmod
    - postApply: jq --version

◉ 4 actions planned
```

`--phase modules` selects every module-owned action wherever it was planned, so
it stays the way to preview the modules alone.

Dot-notation paths give the owner its own segment, so a module named `brew` and
the `brew` package manager never collide:

| Pattern | Selects |
|---|---|
| `packages.brew` | the brew package manager's work |
| `packages.module:nvim` | the `nvim` module's package work |
| `module:nvim` | everything the `nvim` module declares, in every phase |
| `profile:work` | everything the `work` profile declares |
| `cfgd:managers` | every package-manager bootstrap |

`--phase`/`--skip`/`--only` all accept the same dot-notation one level up,
scoped to a single phase: `<phase>.<selector>`, where the selector is either
an owner group (`managers`, `env`, `session`: the three `Prerequisites`
always carries), a manager name (family-collapsed, so `prerequisites.brew`
also covers `brew-tap`/`brew-cask`), or a prerequisite tool a registered
manager's installer shells out to (`prerequisites.curl`). A selector is only valid on
`prerequisites`: a group or manager name after any other phase errors,
naming the input and the legal shapes; `--phase packages.brew` errors
pointing at `--phase prerequisites.brew` instead, since manager work lives in
`Prerequisites`, not `Packages`:

| Pattern | Selects |
|---|---|
| `prerequisites.managers` | every provisioned/refreshed package manager, INCLUDING any prerequisite tool a manager's own installer depends on (equivalent to `cfgd:managers`, scoped to `Prerequisites`) |
| `prerequisites.env` | the `~/.cfgd.env`/rc-file write group |
| `prerequisites.session` | the live-session broadcast (`RefreshLiveSession`) |
| `prerequisites.brew` | only the brew manager's own node — NOT a prerequisite tool brew's installer shells out to (e.g. `curl`), which is keyed on its own name (`prerequisites.curl`) rather than on whichever manager's installer happens to need it |

A manager name still selects exactly one manager when several share a node.
Managers one mediator delivers by an ordinary package install collapse onto a
single node (`provision npm, pipx via apt`; see
[Package Managers](packages.md)), and every selector still addresses them one
at a time: `--skip prerequisites.npm` leaves `provision pipx via apt` behind,
and `--phase prerequisites.pipx` provisions `pipx` alone.

`modules` and `modules.<name>` still work and print a deprecation naming their
replacement.

Skipping a manager's **bootstrap** (`prerequisites.managers`, `prerequisites.brew`,
`cfgd:managers`) leaves the package installs that needed it in the plan:
`cfgd` cannot know whether you meant to drop those too, so it strands them,
warns with `printer.alert(...)`, and prints the `--skip packages.<manager>`
flags that would drop those too. A prerequisite tool that existed only to
satisfy the skipped manager's own bootstrap (`curl` for `brew`, say) is
silently pruned along with it: nothing still depends on it, so there is
nothing to strand. Skipping the other direction, the last package install
that named a manager (`--skip packages.brew.ripgrep` when it's the only brew
package left), silently prunes that manager's now-purposeless bootstrap node
instead: nothing in the plan needs it anymore, so there is nothing to warn
about.

**`--only` never prunes for lack of consumers.** `--only prerequisites.managers`
(the recovery command the alert above prints) keeps every manager bootstrap
node even though it drops every package install that used to justify them:
an `--only` selector is explicit selection, and a node you named directly is
its own justification. The consumer-prune described above applies to the
`--skip` direction alone; `--only cfgd:managers` and `--only
prerequisites.managers` both keep the full manager set with an empty
`Packages` phase, never an empty plan.

The `-o json` payload carries the same axes the tree draws: a phase holds owner
groups, each group holds its actions. `token` is the rendered owner label, so a
consumer never rebuilds the `kind:name` grammar itself, and groups arrive in the
same order the tree prints them (`profile` before `cfgd` before `module` before
`backup` before `source`, then by name).

The payload is the complete inventory, so it holds one phase the tree does not
print: `Modules`, carrying the platform-gated skips that the human render folds
into the header's `Modules` row instead. A consumer diffing plans across hosts
therefore sees that a module was gated out on one of them.

```jsonc
// cfgd plan -o json  →  phases[].groups[]
{
  "phase": "Packages",              // the kind phase; module work routes here too
  "groups": [
    {
      "owner": { "kind": "module", "name": "dev-tools" },
      "token": "module:dev-tools",  // exactly what the tree prints
      "actions": [
        {
          "type": "install",
          "description": "brew install ripgrep (15.2.0) <- team",
          "targets": ["ripgrep"],
          "origin": "team"
        }
      ]
    }
  ]
}
```

An action that produces a count carries it as `detail`, the same string the
tree hangs off the row after the em-dash (`- deploy 6 files — 5 already
deployed`, `- write ~/.cfgd.env — 3 vars, 1 alias`); `description` names the
subject alone and never folds the count in. A `Files` deploy's subject is
always a count of the files it writes, never their names — each target
enumerates as its own child row beneath the action, `<target> — <method>`,
naming the resolved `symlink`/`copy`/`template`/`hardlink`/`patch` strategy.
A subset counts against the module's declared set (`5 already deployed`); a
full deploy carries no count at all, its subject already stating how many it
writes.

A `Prerequisites` action carries a structured `manager` sub-object beside its
`description`, so a consumer classifies a manager's state without parsing the
sentence: `state` is `present` (an already-installed manager's index refresh),
`provisioned` (a manager this run installs, `via` naming its bootstrap method),
`prerequisite` (a tool a provision's installer shells out to; `manager` names
the tool, `via` names the installer), or `refused` (a manager that can't be
provisioned, `reason` naming why: a refusal is still something the run
decided, so `-o json` carries it rather than dropping it silently).
`requires` holds the full node ids of the actions this one depends on,
resolving one-to-one against a sibling action's own `description`:

```jsonc
// cfgd plan -o json  →  phases[] entry for Prerequisites
{
  "phase": "Prerequisites",
  "groups": [
    {
      "owner": { "kind": "cfgd", "name": "managers" },
      "token": "cfgd:managers",
      "actions": [
        {
          "type": "refresh",
          "description": "refresh brew index",
          "manager": { "manager": "brew", "state": "present" }
        },
        {
          "type": "provision",
          "description": "provision pipx via pip install pipx",
          "manager": {
            "manager": "pipx",
            "state": "provisioned",
            "via": "pip install pipx",
            "requires": ["manager:prereq:curl"]
          }
        },
        {
          "type": "refuse",
          "description": "cannot provision snap — no available system manager",
          "manager": {
            "manager": "snap",
            "state": "refused",
            "reason": "no available system manager"
          }
        }
      ]
    }
  ]
}
```

A run that composed one or more [sources](sources.md) names them, in layering order,
in a `sources` array beside `totalActions` (omitted when the run was purely local).
The human header carries the same list as a **Sources** row under **Profile**:

```jsonc
{
  "sources": [
    { "name": "team", "profile": "team" },
    { "name": "infra" }
  ]
}
```

A [source](sources.md#automatic-apply-decisions) item still awaiting `cfgd decide` (or one
you rejected) is withheld from the plan: it is absent from the phases, from
`totalActions`, and from what `apply` executes (with `--yes` or with the
confirmation prompt). Both states are named rather than silently missing: the
human render lists them under **Pending Decisions (1 item, not included in this
plan)** and **Declined Decisions (1 item, not included in this plan)** (the same
title `cfgd decide` and `cfgd status` render, with the plan's qualifier beside the
count), and the payload carries
the same rows as `pendingDecisions` and `rejectedDecisions`, each omitted when
empty, so a structured consumer can tell "in sync" from "waiting on you" from
"you said no":

```jsonc
{
  "totalActions": 1,
  "pendingDecisions": [
    {
      "id": 1,
      "source": "acme-corp",
      "resource": "packages.brew.k9s",
      "tier": "recommended",
      "action": "install",
      "summary": "recommended packages.brew.k9s (from acme-corp)",
      "createdAt": "2026-08-09T17:04:11Z",
      "resolvedAt": null,
      "resolution": null
    }
  ],
  "rejectedDecisions": [
    {
      "id": 2,
      "source": "acme-corp",
      "resource": "packages.brew.stern",
      "tier": "recommended",
      "action": "install",
      "summary": "recommended packages.brew.stern (from acme-corp)",
      "createdAt": "2026-08-09T17:04:11Z",
      "resolvedAt": "2026-08-09T17:09:52Z",
      "resolution": "rejected"
    }
  ]
}
```

An `id` of `0` marks an item classified this run but **not yet recorded** in the
decision store: `plan` is read-only, so the row is minted later: by `cfgd
decide` when you answer it, or by the `cfgd apply` / daemon tick that follows.
Every other field carries the same shape either way, and the item is withheld
identically; only a recorded row has a real (non-zero) `id`. The same
discriminator reaches the other two read surfaces: `cfgd status -o json`
(`pendingDecisions`) and the bare `cfgd decide -o json` listing (`decisions`)
fold in the same classified-but-unrecorded rows, `id: 0` included.

A `Notify`-tier package the machine **already satisfies** never lands in
`pendingDecisions` at all: the run's own package enumeration answers the
question and the item is [auto-accepted](sources.md#edge-cases): previewed as
included by `plan`, recorded as a resolved row with resolution `auto-accepted`
by the writing paths. "Satisfies" is judged against the version the manager's
listing reports (an entry with no version spec is satisfied by any installed
version; `tool@v1.2.3` pins with caret semantics, i.e. `^1.2.3`). A version
conflict instead stays pending with the conflict annotated in the row's
`summary` (e.g. `… — installed 13.0, source wants ^14`), on the recorded row
and the `id: 0` shape alike; a manager whose listing reports no version reads
`installed (version unknown), source wants ^14`.

In the human render, an unrecorded item keeps the usual ``Run `cfgd decide accept
<resource>` or `cfgd decide reject <resource>` to answer`` instruction only where
that command could actually record it. On a config that cannot mint the row (a
foreign `--config` without `--state-dir`) the hint says so instead (*Not yet
recorded — answer from the machine's own config, or pass --state-dir*); a
recorded row resolves without a mint, so its instruction holds on every config.

Only a source you are still subscribed to can withhold anything: a decision
whose source has been removed from `spec.sources` is inert, and a real `cfgd
apply` discards it, unless you pointed that run at a FOREIGN config (a
`--config`, `--config-dir`, or `CFGD_CONFIG` that resolves somewhere other
than the default config location) while leaving `--state-dir` at its default,
in which case the rows are left alone because they belong to another config's
picture of the machine. Ownership follows the resolved path, not the spelling:
`--config ~/.config/cfgd/cfgd.yaml` names the machine's own config (the same
`--config` every installed service unit bakes into its invocation) and still
discards. The default location is the **run's scope's**: a user-scope run does
not treat `/etc/cfgd/cfgd.yaml` as its own config, because the store it opened
is the per-user one and the system picture's subscription list must not sweep
it (and vice versa for a system-scope run). The store itself follows the same
scope: with no `--state-dir`, a `--scope system` run opens the machine-wide
state root (Linux `/var/lib/cfgd`, macOS `/Library/Application Support/cfgd/state`,
Windows `%ProgramData%\cfgd\state`) rather than the per-user one, so the store
a run judges ownership against is always the store it opened.

### `cfgd status`

Show configuration status, drift, and pending decisions.

```sh
cfgd status                                 # human-readable table
cfgd status -o json                         # full status as JSON
cfgd status -o jsonpath='{.drift}'          # extract drift events
cfgd status --module nvim                   # status for a single module (no profile required)
cfgd status --module nvim -o wide           # itemized inventories instead of counts
cfgd status --module nvim --show-values     # inventories with declared values (implies -o wide)
cfgd status --scan                          # live scan of this machine right now
cfgd status --scan --module nvim            # live scan of one module
cfgd status --module nvim --exit-code       # live scan: exit 5 if the module has drifted
```

`cfgd status` (fleet-wide and `--module`) is a fast RECORDED-drift dashboard by
default: it reads what a prior `apply`/`diff`/`verify`/`status --scan`/daemon run
already wrote to state, so on a host with no daemon and no prior scan it reports no
drift however far the machine has actually drifted. `-o json`'s `drift` array and
`driftCheckedLive` flag say which of those two you are holding.
`driftCheckedLive: false` means `drift` is only what was previously recorded, not a
claim about the machine right now:

```jsonc
{
  "driftCheckedLive": false,
  "drift": []
}
```

The recorded dashboard dates itself on the verdicts the date qualifies: the
Component Health heading carries `checked 6m ago` (`drift never checked` when
no scan is on record, `checked live now` after `--scan`), the freshest of the
machine-wide scan stamp and the recorded rows' own timestamps, since a scoped
scan records rows without moving the stamp. Each unresolved recorded finding
nests under the health row of the owner it belongs to, stating its terse
cause, and the owner's verdict flips to `Drifted` with the shortfall as its
parenthetical:

```
Component Health (checked 6m ago)
  ⚠ module:nvim — Drifted (1 of 6 files)
    ~/.config/nvim/init.lua — content differs
  ✓ module:git  — Synced (1 file)
```

Once that freshest evidence passes the daemon's default reconcile interval the
report closes on a hint pointing at `cfgd diff`, so a stale dashboard says so
rather than reading as a clean machine. `-o json` carries the stamp itself as
`lastScanAt` (an ISO 8601 timestamp, absent when there has been no
machine-wide scan) and each row's `timestamp` beside it, with the findings as
the flat `drift` array.

A recorded `env-var` or `alias` row is re-read against the machine before it is
shown. If the declared line is the line the managed env file now holds, the row
healed since it was recorded and is not reported at all (state clears it on the
next apply or scan). If it still differs, the human row shows the real operands
(`want: export EDITOR="vim" …, have: missing` — a reader healing drift needs
the declared value in front of them), and `-o json` adds the same pair as
`want` and `have` beside the row's stored `expected` and `actual`. The stored pair keeps the bytes the row was written with (the opaque
`current` / `missing or changed` markers a keyed record describes itself by); the
additive pair carries what the re-read found. Both additive fields are omitted
when nothing was re-read, which includes every live `--scan` finding: those are
minted by the scan itself, so their `expected` and `actual` already are the
re-read.

`--scan` performs the live, read-only scan `diff`/`verify` do and folds its
findings into the display: `driftCheckedLive` flips to `true` and `drift`
reflects what the scan actually found. A fleet-wide `--scan` also records the
scan, so its `lastScanAt` is the stamp this run wrote; `--scan --module` does
not record one, because a single module's check is not evidence the machine was
scanned. It composes with `--module` (scanning that one module) and with
`--exit-code`. `--exit-code` / `-e` implies `--scan` and additionally exits `5`
when the scan found drift, or `1` when a system configurator check itself
failed: the same split `cfgd diff --exit-code` and `cfgd verify --exit-code`
report, since an unknown state outranks a known one (see
[Exit Codes](#exit-codes)); `--scan` on its own never changes the exit code.
A failed check is never silently dropped: the report renders it as its own
row (`gpgKeys: error checking drift — keyring unavailable`) and `-o json`
carries it in `systemErrors` (the same `{key, error}` entries `cfgd diff` and
`cfgd verify` report), so an empty `drift` array beside a non-empty
`systemErrors` reads as "unknown", not "clean". The live scan costs real time
(each run is a full package/file check, roughly 10-15s per module in a
typical container), so reach for it deliberately rather than in an
interactive dashboard refresh.
`status --module <name> --scan` scans that module's own files, missing
packages, and the env vars and aliases its chain owns (the same per-item check
`cfgd diff --module` runs, so a scoped workflow records AND heals its own
shell rows): it does not evaluate the module's system-config contribution
(`effective_system_map` folds that into the profile-wide scan) or manager
drift. A scoped scan whose env probe itself fails (the managed env file exists
but cannot be read) reports the same `error checking drift` row, exits `1`
under `--exit-code`, and its `-o json` carries the failure in `systemErrors` —
the same `{key, error}` entries the fleet payload uses for the same fact.
`cfgd diff --module` shares the whole scope (see [`cfgd diff`](#cfgd-diff)).

The fleet report's `Managed Resources` table names an owner per row, in the same
vocabulary the plan and apply trees head their groups with and `cfgd diff`
reports drift under: `profile:<name>` for a resource the profile declared,
`module:<name>` for one a module declared, and `cfgd:env` / `cfgd:session` for
what cfgd manages on its own behalf (the generated env file and the rc source
line; the live-session publish).

The rows are ordered by owner the way a plan or apply tree orders its groups:
the profile first, then cfgd's own groups in the order they run, then the
modules. Within one owner the table sorts by type and resource.

```
Managed Resources
  Type     Owner             Resource                           Source
  ────────────────────────────────────────────────────────────────────
  file     profile:work      ~/.bashrc                          local
  package  profile:work      brew: bat, ripgrep                 local
  env      cfgd:env          /home/you/.cfgd.env                local
  env      cfgd:session      session env                        local
  file     module:nvim       /home/you/.config/nvim (12 files)  local
```

The default module report is a summary: one count per declared surface, then
what the scan found. `Status` and `Scope` lead the block: `Scope` names what
the run that last touched this module was scoped to, in the same tri-colour
`kind:name` token the apply tree and the Managed Resources Owner column use,
and is absent when that run applied a whole profile. `Shell` is the total of
the module's declared aliases and env vars, broken down one indented row per
half; `Scripts` is the total, broken down one indented row per hook in the
order the hooks run, so a module's `preApply` work is distinguishable from its
`postApply` work without opening the module:

```
Status: nvim
  Status        Drifted
  Scope         module:nvim
  Last Applied  3h ago
  Packages      27
  Files         6
  Shell         6
    Aliases     3
    Env         3
  Scripts       9
    preApply    3
    postApply   6

Drift
  ⚠ module:nvim:files ~/.config/nvim/stylua.toml — content differs
  ⚠ module:nvim:packages ripgrep                        — version mismatch
```

Without `--scan`, the module Drift section is the recorded read the fleet
dashboard performs, filtered to this module's chain (the module and its
transitive `depends`, minus platform-gated members), covering every
producer's recorded grammar: module-file rows whose owner is in the chain, the
daemon's whole-module rows (a bare `module:<name>` verdict with no file
granularity), package rows whose packages the chain declares under the
recorded manager, and `env-var`/`alias` rows attributed to the last chain
module declaring the name (the merge's own winner). The verdict carries the
same freshness vocabulary the fleet's Component Health heading does — `checked
2h ago`, dated by the freshest of the machine-wide stamp and the rows' own
timestamps, or `No drift recorded — drift never checked` when there is
neither — and the module
`-o json` payload carries the same `lastScanAt` the fleet payload does. A
recorded module-file finding also marks its Deployed Files row `drifted`
rather than `not scanned`. A `--scan` replaces the recorded rows with what
the live check found; the payload's `lastScanAt` still names the recorded
stamp, because a single module's scan never writes one.

`-o json` carries the same breakdown as `scriptCounts`, an ARRAY in execution
order rather than an object (a JSON object is a sorted map on the way out, and
alphabetical is not the order the hooks run in). The `scripts` field beside it
keeps the hook names it always carried:

```jsonc
{
  "scripts": ["preApply", "postApply"],
  "scriptCounts": [
    { "hook": "preApply", "count": 3 },
    { "hook": "postApply", "count": 6 }
  ]
}
```

Each drift row names the module, the `spec` block the finding is on, the item
itself, and the KIND of divergence (`content differs`, `version mismatch`,
`missing` for a declared file, `not installed` for a declared package,
`unmanaged file at target` for a target holding a file cfgd never wrote — see
[`--on-conflict`](#unmanaged-files-at-a-managed-target)). The
bytes themselves are `cfgd diff`'s job. Rows are grouped by surface
(files, then packages, then any other surface alphabetically) and sorted by
item within each group, so two runs that found the same drift render the same
section rather than reordering it by whatever the scan reached first.
`✓ No drift detected` is claimed only after `--scan`; without one the row says
nothing was checked.

`-o wide` replaces the counts with the inventories, each row carrying its own
verdict, and drops the `Drift` section: every finding is already inline on the
row for the thing it was found on:

```
Status: nvim
  Status        Drifted
  Scope         module:nvim
  Last Applied  3h ago

Installed Packages
  ✓ neovim  — brew
  ⚠ ripgrep — not installed (brew)

Deployed Files
  ✓ ~/.config/nvim/init.lua
  ⚠ ~/.config/nvim/stylua.toml — content differs
  ✗ ~/.gitconfig               — missing

Shell
  Aliases
    ✓ gs
  Env
    ✓ EDITOR
    ✓ PAGER

Scripts
  ✓ preApply: set -euo pipefail …
  ✓ postApply: nvim --headless '+Lazy! sync' +qa
```

Packages, files, aliases and env vars list alphabetically; scripts stay in
execution order, because that order is the fact. Aliases precede env vars
wherever the pair is named — the counts, these inventories, `cfgd module
show`'s sections, the profile inventory `cfgd profile show`, `cfgd source show`
and `cfgd source add` render, and `-o json`'s field order alike. `--show-values` renders the
same inventories with each declared value (`EDITOR="nvim"`, quoted the way the
generated env file writes it) and each script's whole body instead of its
condensed first line, and implies `-o wide`.

Without `--scan` nothing has asked a manager and nothing has read a file's
content, so every package row and every present file reads `not scanned`
(absence is still definite: a file the module deployed and that is gone reads
`missing` either way). A package the module's own `platforms` gate rules out
on this host reads `skipped (platform filter)` instead, the same words `cfgd
module show` uses for it: nothing was ever going to install it, scan or no
scan. `-o json` carries the same verdicts as `packageState[].state`
(`installed`, `notInstalled`, `notScanned`, `platformSkipped`) and
`deployedFiles[].state` (`deployed`, `drifted`, `missing`, `notScanned`), and
is identical under every view: `-o wide` and `--show-values` change the human
render only.

The payload carries two words for the module itself. `status` is the token the
state store holds (`installed`, `error`, or one of the no-record spellings).
`state` is the verdict the human Status row shows, always present, one of
`Synced`, `Drifted`, `Failed`, `NotApplied`. `Drifted` needs a live scan: both
words come from one derivation, so a `state` of `Drifted` always has the
findings under `drift` to back it.

`pendingDecisions` lists the same rows `cfgd decide` offers, including
classified-but-unrecorded items with `id: 0` (see [`cfgd plan`](#cfgd-plan)).
The dashboard degrades rather than failing on that classification: if it cannot
be built (a malformed package manifest, say), status still renders everything
else and prints a warning naming what it could not read. Under `-o json` the
warning line is suppressed, so the degradation is part of the payload instead:
`classificationDegraded` is always present, and the code and reason fields
appear only when it is `true`, so a broken classification is never mistaken
for a clean machine with nothing pending:

```jsonc
{
  "classificationDegraded": true,
  "classificationDegradedCode": "manifestUnreadable",
  "classificationDegradedReason": "cargo manifest manifests/Cargo.toml: TOML parse error at line 1",
  "pendingDecisions": []   // recorded rows only; the unrecorded ones could not be read
}
```

`classificationDegradedCode` is the machine-stable half, a closed set a
consumer can branch on: `decisionStoreUnreadable` (fix the state directory /
database), `sourceUnreadable` (re-sync or inspect the source's cached
config), `manifestUnreadable` (fix the referenced Brewfile / `package.json` /
`Cargo.toml` / apt list), or `classificationFailed` (anything else). The
reason string is the human detail and carries no stability promise.

A source batch no decision row can name (packages under a
[dotted custom manager](sources.md#edge-cases)) is withheld fail-closed, and
the dashboard names it the same way `cfgd plan` does: the human render carries
the warning line, and the payload carries a `warnings` array (omitted when
empty) with the same strings the plan payload's `warnings` holds:

```jsonc
{
  "warnings": [
    "custom manager 'pip3.11' cannot carry source decisions — its name contains '.', which the decision path grammar splits on. Withheld from this run until the manager is renamed: requests (from acme-corp)"
  ]
}
```

The bare `cfgd decide -o json` listing carries the same fields (the
degradation pair and `warnings` alike) alongside its `decisions` array.

### `cfgd diff`

Show detailed file diffs with syntax highlighting.

```sh
cfgd diff                    # human drift report
cfgd diff --module nvim      # a single module's resources
cfgd diff --exit-code        # exit 5 on drift, for CI gating
cfgd diff -o json            # structured drift payload
```

The report is differences-only: a converged surface prints nothing at all, so what
is on screen is what needs fixing. Each drifted surface names its owner group: the
same coordinate the plan and apply trees would use to fix it:

```
Diff
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  work
  Modules  nvim

Files
  profile:work
    ◉ /home/you/.gitconfig (new file)
    [user]
    name = You
  module:nvim
    ◉ /home/you/.config/nvim/stylua.toml
    -# not mine
    +indent_type = "Spaces"

Packages
  profile:work
    ⚠ brew: not installed — extra-tool
    ⚠ nix: not installed  — hello
  cfgd:managers
    ⚠ pipx: not installed — can bootstrap via pip install pipx
    ⚠ snap: not installed — cannot bootstrap: no available system manager

Shell
  profile:work
    ⚠ alias: ll — want: alias ll="ls -la", have: alias ll="ls -lah"

System
  profile:work
    ⚠ sysctl.net.core.somaxconn — want 8192, have 4096

⚠ Drift detected — 2 files, 2 packages, 1 shell item, 1 system setting
```

Surfaces render in a fixed order (files, packages, shell, system) and items sort
alphabetically within each, so two runs that found the same drift read the same
rather than reordering by whatever the check reached first.

The closing line carries the tally, and names the surfaces that were checked and
came back clean, the only place a converged surface is mentioned at all:

```
⚠ Drift detected — 1 file (packages, shell, system clean)
```

A surface whose check could not RUN is named by neither half; the reason it could
not is on the same line (`⚠ Drift detected — 1 file (packages, shell clean); a system
check could not run`). A run with nothing to report is a single line:

```
✓ No drift detected
```

`--module <name>` scopes the run to one module: its files, its declared
packages, and its own declared env vars and aliases (checked against the
primary managed env file the machine-wide Shell surface reads — the rc lines
and platform siblings stay the machine-wide walk's — attributed by the
declaring module). The heading carries it, and the header names the config and
the module the way `apply --module` does. The closing line never calls
`system` clean (a module run evaluates no system configurator), and never
calls `shell` clean either: the run checked only the entries its own modules
declare, not the whole surface. A scoped run records the env findings it can
vouch for but never heals a recorded env row: the deployed line is
machine-wide (a module outside this chain can own it), so only the
machine-wide check clears one. A profile-owned entry that has drifted is
invisible to a module run by design; `cfgd diff` reports it:

```
Diff: nvim
  Config   /home/you/.config/cfgd/cfgd.yaml
  Modules  nvim

Files
  module:nvim
    ◉ /home/you/.config/nvim/stylua.toml
    -# not mine
    +indent_type = "Spaces"

⚠ Drift detected — 1 file (packages clean)
```

An isolated run still reads `cfgd.yaml`: the `Sources` row names what the config
subscribes to whether or not a profile resolved, so a missing or unparsable config
refuses the run exactly as it does for `apply --module`.

The Shell surface checks the declared `spec.env` vars and `spec.aliases` against the managed env
files cfgd owns (`~/.cfgd.env` and its platform siblings) and the rc source lines that load
them. It never reads a live shell session: a var or alias exported only by hand, outside those
files, is invisible to this check by design.

Per-item rows are labelled `env:` or `alias:` (`env: EDITOR`); the whole-file row
is labelled `env file:` (`env file: /home/you/.cfgd.env`), so the file and the
entries inside it do not read as one kind. The whole-file row is omitted whenever
the item rows below it already name which entry the file is missing, and the
closing tally counts only the rows the report showed.

`cfgd:managers` reports package **managers** the plan itself would provision or
refuse: not something the profile declared missing, but something `apply` would
still change. It draws from the same planner the `Prerequisites` phase uses (see
[Reconciliation](reconciliation.md#phases)), so a manager never reads
"converged" here while `apply` still has work to do on it. A manager `apply` can
self-heal reads `not installed — can bootstrap via <method>`; one it cannot reads
`not installed — cannot bootstrap: <reason>`.

A file's body renders directly under the line that names it, so a hunk reads with
its target rather than above it.

The payload carries `files[]`, `packages[]`, `system[]`, `env[]`, and a `summary`. `files[]` lists only the managed files that do NOT match desired state, in the same shape `cfgd verify` reports a resource:

```json
{
  "files": [
    {
      "resourceType": "file",
      "resourceId": "~/.config/acme/app.ini",
      "matches": false,
      "expected": "content satisfies patch spec",
      "actual": "cannot evaluate patch spec: file error: patch script for ~/.config/acme/app.ini is blocked: source 'acme' is not allowed to run scripts (constraints.noScripts); set subscription.allowScripts: true to opt in"
    }
  ]
}
```

A file cfgd could not evaluate (an unparseable target, a filter that exited non-zero, a patch script a source is [barred from running](sources.md#noscripts)) appears with the reason as its `actual`, so the cause is visible without reading the terminal rendering.

Every file entry also carries `unmanaged` (a bool): `true` when the target holds a file cfgd has never written, in which case `actual` reads `unmanaged file at target` rather than `content differs`. The two are different problems with different fixes — one is resolved by [`--on-conflict`](#unmanaged-files-at-a-managed-target), the other by re-applying — so a consumer can tell them apart without matching on prose.

A managed file whose `source` cannot be found is reported as drift here and by `cfgd verify` / `cfgd status`: the desired content could not be determined, which is never the same as convergence.

`packages[]` entries carry `manager`, `shape` (`missing` | `extra` | `provision` | `refused`), and `packages` (empty for the two manager-drift shapes). `shape: "provision"` matches the machine vocabulary `plan -o json`'s `Prerequisites` phase already uses for the same fact (`type: "provision"`); the mechanism itself still keeps the "bootstrap" word, in `bootstrapMethod` and in the human render above. A `provision` entry adds `bootstrapMethod`; a `refused` entry adds `reason` instead: the same fields [`cfgd doctor`](#cfgd-doctor)'s manager checks use, so a script reading either surface for "can this manager self-heal" reads one field name:

```json
{
  "packages": [
    { "manager": "cargo", "shape": "missing", "packages": ["ripgrep"] },
    { "manager": "pipx", "shape": "provision", "bootstrapMethod": "pip install pipx" },
    { "manager": "snap", "shape": "refused", "reason": "no available system manager" }
  ]
}
```

`env[]` entries carry `kind` (`env-var` | `alias` | `env` | `env-rc`), `name`, `expected`, and
`actual`. `kind` matches `cfgd verify`'s `resourceType` for the same check byte-for-byte, so a
consumer joining this against a `cfgd verify` or recorded-drift row needs no second vocabulary.
`env-var` and `alias` are per-declared-item checks (a mismatched line in `~/.cfgd.env`); `env`
and `env-rc` are whole-file and rc-source-line checks that predate them. Both operands of a
per-item check are real values: `expected` is the line the declaration renders as, `actual` is
the line the managed file holds right now, or `missing` when no line in it claims that name:

```json
{
  "env": [
    {
      "kind": "alias",
      "name": "ll",
      "expected": "alias ll=\"ls -la\"",
      "actual": "alias ll=\"ls -lah\""
    }
  ]
}
```

### `cfgd verify`

Check that all managed resources match desired state.

```sh
cfgd verify -o json          # structured pass/fail results
cfgd verify --module nvim    # verify only a single module's resources (no profile required)
```

Each entry in `results[]` carries `resourceType`, `resourceId`, `matches`, `expected`, `actual`, and `unmanaged`, alongside the top-level `passCount` / `failCount`. `unmanaged` is `true` only for a `file` result whose target holds a file cfgd never wrote; see [`cfgd diff`](#cfgd-diff)'s `files[]`.

A system configurator whose check itself fails is reported as its own row
(`gpgKeys: error checking drift — keyring unavailable`) rather than aborting
the run: every other resource is still verified and recorded, `-o json`
carries the failure in `systemErrors` (the same `{key, error}` entries
`cfgd diff` and `cfgd status` report), and the closing tally counts it as its
own clause (`5 passed, 0 failed, 1 check could not run`). With `--exit-code`
such a run exits `1` ahead of `5`: an unanswered check reads as "unknown",
not "clean" (see [Exit Codes](#exit-codes)).

### `cfgd doctor`

Check system health: available package managers, configurators, module status, dependency versions.

```sh
cfgd doctor -o json   # structured health report
```

Exits non-zero when the verdict fails (an invalid config, a config missing at an
explicitly-given `--config`/`CFGD_CONFIG`/`--config-dir` path, an unresolvable module, or a
hard-broken profile such as [ambiguous layout forms](profiles.md#layout)), so
`cfgd doctor && cfgd apply` stops instead of proceeding into a broken apply. A config
missing at the *default* path is the fresh-machine state and stays a warning (exit 0),
as does a supported legacy-flat layout; warnings do not affect the exit code.

### `cfgd log`

Show apply history from the state store.

```sh
cfgd log                    # last 20 entries
cfgd log -n 50              # last 50 entries (also --limit)
cfgd log -o json            # JSON apply history
cfgd log --show-output 42   # show captured script output for apply #42
```

The `Scope` column names what each run was scoped to: the profile it applied, or the
`module:<name>` list a `--module` run isolated itself to. A run that named neither shows `-`.

In `-o json` that value stays in the `profile` field (`cfgd log -o json`, and
`.lastApply.profile` in `cfgd status -o json`): the field name is a wire contract and
does not change with the column heading. Read it as the run's scope, not as a profile
name: it holds `module:nvim` for an isolated run and an empty string for a run that
resolved no profile, and older rows may still hold the literal `unknown`.

### `cfgd rollback <apply-id>`

Restore the file backups cfgd took before a previous apply, undoing that apply's file writes.

```sh
cfgd log                          # find the apply ID
cfgd rollback 42                  # restore the files that apply overwrote
cfgd rollback 42 --yes            # skip the confirmation
cfgd rollback 42 -o json          # structured result
```

Rollback covers **cfgd's own file writes**: the pre-overwrite backups stored inline in the state
database, not the declarative `spec.backups[]` snapshots that
[`cfgd backup restore`](#cfgd-backup) puts back. Packages installed and system settings changed by
that apply are not reverted. See [File Safety](safety.md#file-backups) for what is captured and for
how long.

### `cfgd sync`

Pull from all remotes, show changes, prompt for apply.

The header opens on `Config`, `Sources`, `Profile` and `Modules` before the
first fetch, so every row describes the configuration this run started from and
a failed pull is still attributed to a named one. `Sources` names what
`spec.sources[]` declares, whether or not any of them has been fetched yet — a
cold cache changes nothing about the header. The body below reports what the
pull changed; the plan the closing hint invites reads the new set.

The `Local Repo` section appears only for a config directory under version
control — there is nothing to pull from one that is not.

```
Sync
  Config   /home/you/.config/cfgd/cfgd.yaml
  Sources  team
  Profile  work
  Modules  core, editor

Local Repo
  ✓ Pulled new changes from remote — commit: 9b1c3d4e5f60 -> 4f2a8c1d9e07

✓ Synced
→ Run `cfgd plan` to preview changes, then `cfgd apply`
```

A leg that refused withholds the success verdict and exits non-zero, the same
way `cfgd apply` reports a partial run. A source the reader declined at the
permission prompt is an answered question rather than a refusal, so it keeps
exit 0.

```
Sync
  Config   /home/you/.config/cfgd/cfgd.yaml
  Sources  team
  Profile  work
  Modules  core, editor

Local Repo
  ⚠ Pull failed — find remote: remote 'origin' does not exist
  → Add the remote with `git remote add origin <url>`, then re-run `cfgd sync`

⚠ Sync incomplete — local repo not pulled
```

The cause is the message git raised, without libgit2's `class=…; code=…`
tail, and the hint names the fix for that kind of refusal: a missing remote, a
diverged branch, an unreachable one, an empty repository, or the general case.

`-o json` carries `localPullError` beside `localPulled`, so a consumer sees
the same fact the verdict withheld `Synced` over.

The header's own resolution is a leg like the pull. A configuration that will
not resolve at all (an unknown module, an invalid source name, a cache directory
cfgd cannot create) is reported as `⚠ Sync incomplete — configuration not
resolved`, exits non-zero, and carries `configResolutionError` in `-o json`.
The `Sources` row survives that refusal: the fetch below is about those very
subscriptions, so the row a reader needs most is the one a failed resolution
must not take away. Only `Modules` is missing, nothing having resolved.

The exception is what the header reads off a **cached checkout** and the fetch
then re-judges: a HEAD whose signature the subscription now refuses, a manifest
that will not parse or offers nothing. Those are starting points, not verdicts:
an informational line reports the reading, the `source:<name>` row settles it,
and the run that repairs a broken cache exits 0 (a fault the fetch does not
clear comes back as a failed source row, so the exit is still non-zero). See
[Demanding signed commits](sources.md#demanding-signed-commits) for the recovery.

### `cfgd pull`

Pull remote changes (git pull only, no apply).

`cfgd pull` and `cfgd sync`'s `Local Repo` leg read one seam, so they agree on
what a config directory is and on what a refusal costs: a directory under no
version control has nothing to pull and exits 0, and a pull that failed exits
1 from either verb.

```
Pull
∅ Nothing to pull — the config directory is not a git repository
```

```
Pull
⚠ Pull failed — find remote: remote 'origin' does not exist

→ Add the remote with `git remote add origin <url>`, then re-run `cfgd pull`
```

`-o json` carries `status`: `pulled`, `up_to_date`, `not_a_repository` or
`failed`, with `error` filled on the last.

### `cfgd upgrade`

Check for and install cfgd updates from GitHub releases.

```sh
cfgd upgrade                   # download and install latest
cfgd upgrade --check           # check only (exit 0 = current, 2 = update available, 1 = error)
cfgd upgrade --require-cosign  # fail if cosign signature cannot be verified
CFGD_REQUIRE_COSIGN=1 cfgd upgrade
```

#### Signature verification

Each release artifact is signed with keyless cosign (Fulcio/OIDC + Rekor).
`cfgd upgrade` verifies the keyless signature over the per-artifact
`<archive>.sha256` file, pinned to a canonical-repo workflow identity (the
`publish-crate.yml` legs that `release.yml` invokes do the signing), then
confirms the downloaded archive matches that trusted checksum. This is the
same recipe documented for manual verification in
[installation.md](installation.md#verifying-downloads).

By default, if the `cosign` CLI isn't installed locally (or the release lacks
the cosign bundle), verification emits a `WARN` and falls back to **SHA256-only**,
which trusts GitHub Releases asset hosting alone.

`--require-cosign` (or `CFGD_REQUIRE_COSIGN=1`) flips the policy from
"warn and proceed" to "block the upgrade." Any condition that would trigger the
fallback fails the upgrade with exit 1 and emits an error_doc with
`error: "cosign_required"` plus `requireCosign: true` in the payload, so
alerting can route strict-mode failures separately from generic install
errors. Recommended for unattended / CI updaters where a silent SHA256-only
fallback should never happen.

The structured-output payload on a successful upgrade carries
`verificationMode` so downstream consumers can detect a fallback even when
strict mode is not requested:

| `verificationMode`       | Meaning                                              |
|--------------------------|------------------------------------------------------|
| `cosign`                 | full cosign signature verified (default policy)      |
| `sha256-only`            | cosign artifacts unavailable → SHA256-only fallback  |
| `strict-cosign-required` | strict mode was requested and honored                |
| `null`                   | no install performed (already at latest)             |

### `cfgd explain`

Show schema and field documentation for resource types.

```sh
cfgd explain module                        # show local Module spec
cfgd explain module-crd                    # show cluster-side Module CRD spec
cfgd explain profile                       # show Profile spec
cfgd explain profile.spec.packages         # show specific field
cfgd explain --recursive machineconfig     # expand all fields
```

Schemas are derived from the live resource types (the `cfgd-core` kind
registry), so `explain` always matches what cfgd actually accepts. Every page
carries a `Docs` row pointing at the page in `docs/` that describes it in
prose, through the same link slot: a kind's header (`cfgd explain module`)
and a field drilldown's (`cfgd explain profile.spec.packages.brew`) alike. A
drilldown's row points at the field's OWN heading when the doc has one
(`### spec.packages.brew` becomes the anchor `#specpackagesbrew`: lowercase,
every character that is not a letter, digit, space or hyphen dropped, spaces
turned to hyphens), and otherwise falls back to the kind's own `#fields`
anchor (the same pointer the kind page's row carries). `--recursive` keeps
this one row at the top of the page; the expanded subtree carries none of its
own.

That row is a link, pinned to the release of cfgd that printed it, so the page
it opens documents the schema the binary just explained rather than whatever
`master` has moved on to. On a terminal that renders OSC 8 hyperlinks the row
shows the short repo-relative path and a click opens the page; everywhere else
(a pipe, a file, a terminal cfgd does not recognize) the row is the full URL,
because a repo-relative path is something no terminal auto-links and no reader
can paste into a browser:

```
Docs        https://github.com/tj-smith47/cfgd/blob/v<version>/docs/spec/module.md#fields
```

`<version>` is the running binary's, so the page a row opens documents the
schema that binary just explained.

Hyperlinks are detected from the terminal, never from a flag: iTerm2, WezTerm,
VS Code's terminal, Ghostty, Hyper, Windows Terminal, kitty, Alacritty, Konsole,
and any VTE-based terminal (GNOME Terminal, Tilix) from VTE 0.50 on. Inside `tmux`
or `screen` the row is the plain URL whatever the outer terminal is, since a
multiplexer may not forward the escape. Colour is a separate gate —
`--color never`, `NO_COLOR` and a non-terminal stdout all withhold the escape
along with every other one.

`-o json` carries the bare pointer as `docs` and the same release-pinned URL as
an additive `docsUrl` field beside it, on both page shapes: a kind page's
top-level object and a field drilldown's alike.

A field's type is rendered as the named type it resolves to
(`files <[]ModuleFileEntry>`, `scripts <ScriptSpec>`); a field whose schema is an
inline anonymous object keeps the shape word (`system <object>`). The `-o json`
`type` field is unchanged and still carries the shape word: the named type is an
additive `typeName` field beside it.

A field that accepts a fixed set of values lists them:

```
strategy  <FileStrategy> — How the file is deployed. enum: Symlink, Copy, Template, Hardlink, Patch
```

`-o json` carries the same list in an additive `enum` field, present only on
fields that have one.

`--recursive` renders the structure alone: one `name <type> (required)` row per
field, children indented under their own row, accepted values on an indented
`enum:` line, and descriptions omitted. Drop the flag for the described view.

Drilling into an array-of-object field (`profile.backups`) lists the
element's own fields, same as `kubectl explain`. A field that accepts more
than one shape (e.g. `profile.scripts.preApply`, where each entry is either a
bare string or a `{ run, timeout, … }` object) shows every accepted shape
under a `Variants` section, each named by its type (the `$defs` name where the
schema has one, `BrewSpec`; the shape word, `[]string`, where it does not). When
exactly one shape has fields of its own, those fields are the page's `Fields`
list, listed by name and drillable (`profile.spec.packages.brew.taps`) exactly as
a plain object's would be; `--recursive` expands every shape.

Resource types: `module`, `profile`, `configsource`, `config` (aliases:
`cfgdconfig`, `cfgd`), `machineconfig`, `configpolicy`, `clusterconfigpolicy`,
`driftalert`, `module-crd` (the cluster-side Module CRD), `teamconfig`.

### `cfgd <kind> validate`

Validate a resource document against its schema before committing or applying
it. The validating kinds are the author-facing ones:

```sh
cfgd module validate module.yaml              # validate a file
cfgd profile validate profiles/work/profile.yaml
cfgd source validate cfgd-source.yaml
cfgd machineconfig validate mc.yaml
cfgd configpolicy validate policy.yaml
cfgd clusterconfigpolicy validate -           # read from stdin
cat mc.yaml | cfgd machineconfig validate - -o json
```

Validation checks the document's `apiVersion`, rejects unknown fields, and runs
the kind's cross-field rules. For the CRD kinds (`machineconfig`,
`configpolicy`, `clusterconfigpolicy`) those rules are the *same* checks the
operator's admission webhook enforces: one shared implementation, so a document
that passes `validate` is one the cluster will admit.

A path argument reads that file; `-` reads from stdin. Exit code is `0` when the
document is valid and `4` when it is invalid. With `-o json` the result is a
`{"kind", "valid", "errors"}` payload for scripting.

### `cfgd machineconfig` / `cfgd configpolicy` / `cfgd clusterconfigpolicy`

Author-side commands for the three cluster CRD kinds. Each currently exposes a single subcommand,
`validate`, documented together above under [`cfgd <kind> validate`](#cfgd-kind-validate); the
schema for each kind is in [spec/machineconfig.md](spec/machineconfig.md),
[spec/configpolicy.md](spec/configpolicy.md), and
[spec/clusterconfigpolicy.md](spec/clusterconfigpolicy.md).

```sh
cfgd machineconfig validate mc.yaml
cfgd configpolicy validate policy.yaml
cfgd clusterconfigpolicy validate -
```

### `cfgd skill`

Install a provider-native agent skill that teaches your coding agent (Claude
Code, Gemini, Copilot, Codex, Cursor) to author a high-quality cfgd resource.

```sh
cfgd skill install module                 # install for every detected agent (project scope)
cfgd skill install profile --global       # install under ~/ for cross-repo use
cfgd skill install source --provider claude-code --provider gemini
cfgd skill install module --force         # write even for an undetected agent / overwrite
cfgd skill list                           # alias: ls; -g for user scope
cfgd skill update --all                   # re-render every installed skill at the scope
cfgd skill remove module                  # alias: rm
```

| Flag | Meaning |
|---|---|
| `-g` / `--global` | install/list/remove under the user's home dirs instead of the project |
| `--provider <id>` | restrict to named providers, repeatable (`claude-code`, `gemini`, `copilot`, `codex`, `cursor`); default is every detected agent |
| `--force` | write even for an undetected agent, and overwrite an existing skill |
| `--all` | (on `update`) re-render every skill currently installed at the scope |

The six author kinds are `module`, `profile`, `source`, `machineconfig`,
`configpolicy`, `clusterconfigpolicy`. Install is continue-on-error: each
provider's outcome (`installed` / `skipped` / `failed`) is reported and the
command exits non-zero if any targeted provider failed. `copilot` and `cursor`
have no user-scope primitive, so `-g` reports them skipped rather than writing.
With `-o json`, each command emits a `{kind, scope, cfgdVersion, results[]}`
payload. See [Authoring Skills](skill.md) for the provider target matrix, the
quality bar, and when to use this instead of `cfgd generate`.

### `cfgd paths`

Print the resolved config, state, cache, and runtime directories, each with its
effective source (`flag`/`env`/`default`) and the files cfgd owns there.

```sh
cfgd paths                 # human-readable
cfgd paths -o json         # structured (config/state/cache/runtime objects)
cfgd --cache-dir /srv/c paths -o json   # source reflects the override → "flag"
```

See [Configuration → File locations](configuration.md#file-locations) for the
per-platform defaults and the override precedence.

### Package tokens

`--package` takes the same token on `profile create`, `profile update`, `module create` and
`module update`, in both directions (prefix with `-` to remove). The token mirrors the schema
path the value is written to:

```
--package <manager>[.<list>]:<name>    # brew:ripgrep, brew.taps:charmbracelet/tap
--package <name>                       # no colon: the platform's native manager
```

| Token | Written to |
|---|---|
| `ripgrep` | the native manager for this platform (`apt`, `brew`, `winget`, …) |
| `brew:ripgrep` | `spec.packages.brew.formulae` |
| `brew.taps:charmbracelet/tap` | `spec.packages.brew.taps` |
| `brew.casks:firefox` | `spec.packages.brew.casks` |
| `snap.classic:code` | `spec.packages.snap.classic` |
| `apt:libc6:amd64` | `spec.packages.apt` (only the first colon splits, so the architecture qualifier stays part of the name) |

A colon-carrying token whose prefix names no manager is an error, never a package name:

```
$ cfgd profile update base --package brew.tap:charmbracelet/tap
unknown package manager 'brew.tap' in '--package brew.tap:charmbracelet/tap'; known: apk, apt, apt.packages, brew, brew.casks, brew.formulae, brew.taps, cargo, cargo.packages, chocolatey, dnf, flatpak, flatpak.packages, go, nix, npm, npm.global, pacman, pipx, pkg, scoop, snap, snap.classic, snap.packages, winget, yum, zypper
```

Confirmation lines name what the entry is, then the schema path the value landed in
(`Added tap: charmbracelet/tap (brew.taps)`, `Added cask: firefox (brew.casks)`,
`Added package: ripgrep (brew)`), so the flag and the file agree about where to look.

## Profile Commands

### `cfgd profile list`

List available profiles. Marks the active one.

### `cfgd profile show`

Show the fully resolved profile (all inheritance layers merged).

### `cfgd profile switch <name>`

Switch the active profile in cfgd.yaml. Alias: `cfgd profile use <name>`.

### `cfgd profile create <name>`

Create a new profile. Interactive if no flags provided.

```sh
cfgd profile create work-linux \
  --inherit base \
  --module nvim --module tmux \
  --package apt:build-essential \
  --env EDITOR=vim \
  --alias vim=nvim \
  --file ~/.config/starship.toml \
  --secret secrets/api-key.enc:~/.config/app/key \
  --pre-apply scripts/setup.sh
```

| Flag | Description |
|---|---|
| `--inherit <name>` | Inherit from profile (repeatable) |
| `--module <name>` | Include module (repeatable) |
| `--package <manager[.list]:name>` | Add package (repeatable); see [Package tokens](#package-tokens) |
| `--env <key=value>` | Set env var (repeatable) |
| `--alias <name=command>` | Set shell alias (repeatable) |
| `--system <key=value>` | Set system setting (repeatable) |
| `--file <path>` | Manage file (repeatable) |
| `--private-files` | Mark files as private (gitignored) |
| `--secret <source:target>` | Add secret (repeatable) |
| `--pre-apply <script>` | Add pre-apply script (repeatable) |
| `--post-apply <script>` | Add post-apply script (repeatable) |
| `--pre-reconcile <script>` | Add pre-reconcile script (repeatable) |
| `--post-reconcile <script>` | Add post-reconcile script (repeatable) |
| `--on-change <script>` | Add on-change script (repeatable) |
| `--on-drift <script>` | Add on-drift script (repeatable) |

### `cfgd profile update [name]`

Modify an existing profile. When no name is given, defaults to the active profile. Prefix a value with `-` to remove it.

![authoring a profile from the CLI](../demo/cfgd-author.gif)
*Explain a field, add a package and an alias, preview, then converge: no editor needed.*

```sh
cfgd profile update --package brew:jq
cfgd profile update work --module new-tool --module -old-tool
cfgd profile update work --package brew:jq --package -brew:unused --alias vim=nvim --alias -old
```

| Flag | Description |
|---|---|
| `--inherit <name>` | Add/remove inherited profile (prefix with `-` to remove) |
| `--module <name>` | Add/remove module (prefix with `-` to remove) |
| `--package <manager[.list]:name>` | Add/remove package (prefix with `-` to remove); see [Package tokens](#package-tokens) |
| `--file <path>` | Add/remove file (prefix with `-` to remove by target) |
| `--env <KEY=VALUE>` | Add/remove env var (prefix with `-` to remove by key) |
| `--alias <name=cmd>` | Add/remove alias (prefix with `-` to remove by name) |
| `--system <key=val>` | Add/remove system setting (prefix with `-` to remove by key) |
| `--secret <src:tgt>` | Add/remove secret (prefix with `-` to remove by target) |
| `--pre-apply <script>` | Add/remove pre-apply script (prefix with `-` to remove) |
| `--post-apply <script>` | Add/remove post-apply script (prefix with `-` to remove) |
| `--pre-reconcile <script>` | Add/remove pre-reconcile script (prefix with `-` to remove) |
| `--post-reconcile <script>` | Add/remove post-reconcile script (prefix with `-` to remove) |
| `--on-change <script>` | Add/remove on-change script (prefix with `-` to remove) |
| `--on-drift <script>` | Add/remove on-drift script (prefix with `-` to remove) |
| `--private-files` | Mark all `--file` entries as private (local-only, excluded from git) |
| `--allow-unsigned` | Allow unsigned modules even when require-signatures is enabled |

### `cfgd profile edit <name>`

Open profile in `$EDITOR` with post-save validation.

### `cfgd profile delete <name>`

Delete a profile. Refuses if it's the active profile or inherited by others.

```sh
cfgd profile delete dev --yes               # skip confirmation
cfgd profile delete dev --ignore-not-found  # exit 0 if dev doesn't exist
```

`--ignore-not-found` makes removal of a missing profile a no-op that exits `0`
(kubectl-style idempotent delete) instead of the strict not-found error
(exit `6`). It only affects the not-found case: deleting the active profile
still fails (exit `1`).

When the profile's directory still holds payload files (e.g. `files/`), a
second confirmation gates removing the directory too; declining keeps it in
place. Both confirmations are gathered before anything is deleted, so aborting
at either prompt (Ctrl-C/EOF) leaves the profile fully intact. `--yes` skips
both confirmations.

### `cfgd profile migrate [name]`

Move a legacy flat profile manifest (`profiles/<name>.yaml`) into the canonical
bundle layout (`profiles/<name>/profile.yaml`). The bundle directory may already
exist holding `files/`; the manifest joins its payload. Uses `git mv` when the
config directory is a git work tree (preserving history), a plain rename
otherwise. If a manifest is tracked but `git mv` fails (e.g. index lock
contention), a warning is printed and the move falls back to a plain rename:
the migration succeeds but git history is not preserved for that file. Profile
references are by name, so no manifest content changes.

```sh
cfgd profile migrate work                   # migrate a single profile
cfgd profile migrate --all                  # migrate every legacy profile
cfgd profile migrate --all --dry-run        # print the move plan, change nothing
cfgd profile migrate work --yes             # skip confirmation
```

| Flag | Description |
|---|---|
| `--all` | Migrate every legacy profile (mutually exclusive with `name`) |
| `--dry-run` | Print the move plan without changing anything; exits non-zero if any planned profile would fail (matching a real run) |

Idempotent: already-canonical profiles report "already canonical" and are left
untouched. With `--all`, migration continues past per-profile failures and exits
non-zero if any profile failed (each is reported). An ambiguous profile (both
`profiles/work/profile.yaml` and `profiles/work.yaml` present) is refused as a
failure rather than migrated.

## Module Commands

### `cfgd module list`

List all available modules with their state: `Synced`, `Failed`, or
`NotApplied`. The `-o json` payload's `status` field carries the stored
token instead (`installed`, `error`, `pending`, `available`).

### `cfgd module show <name>`

Show module details: packages, files, dependencies, resolved managers. Env variable values are masked by default (shows `***` with last 3 chars).

```sh
cfgd module show my-tool                # env values masked
cfgd module show my-tool --show-values  # reveal full env values
```

The `Scripts` section lists every lifecycle hook the module declares, one row
per entry labelled with its hook, in the order the hooks run:

```
Scripts
  ◉ preApply: mkdir -p ~/.config/dev-tools
  ◉ postApply: echo 'post-apply hook ran'
  ◉ onDrift: notify-send 'dev-tools drifted'
```

`--show-values` renders each script's whole body instead of its condensed
first line.

### `cfgd module export <name>`

Export a module to another format.

```sh
cfgd module export my-tool --format devcontainer              # current directory
cfgd module export my-tool --format devcontainer --dir out/    # custom output dir
```

Generates `install.sh` and `devcontainer-feature.json` suitable for publishing as a [DevContainer Feature](https://containers.dev/implementors/features/) to GHCR or another OCI registry.

### `cfgd module create <name>`

Create a new local module.

```sh
cfgd module create my-tool \
  --depends node \
  --package neovim \
  --file ~/.config/tool/config.toml \
  --post-apply "tool --setup" \
  --set package.neovim.minVersion=0.9 \
  --set package.neovim.prefer=brew,snap,apt
```

| Flag | Description |
|---|---|
| `--description <text>` | Module description |
| `--depends <name>` | Dependency on another module (repeatable) |
| `--package <manager[.list]:name>` | Add package (repeatable); see [Package tokens](#package-tokens) |
| `--file <path>` | Import file (repeatable) |
| `--private-files` | Mark files as private |
| `--env <key=value>` | Set env var (repeatable) |
| `--alias <name=command>` | Set shell alias (repeatable) |
| `--post-apply <cmd>` | Post-apply script (repeatable) |
| `--set <key=value>` | Helm-style override (repeatable) |
| `--apply` | Apply the module immediately after creating it |

### `cfgd module update <name>`

Modify a local module. Prefix a value with `-` to remove it.

```sh
cfgd module update nvim --package fd --package -unused
cfgd module update nvim --depends node --env EDITOR=nvim --alias vim=nvim
```

| Flag | Description |
|---|---|
| `--package <manager[.list]:name>` | Add/remove package (prefix with `-` to remove); see [Package tokens](#package-tokens) |
| `--file <path>` | Add/remove file (prefix with `-` to remove by target) |
| `--env <KEY=VALUE>` | Add/remove env var (prefix with `-` to remove by key) |
| `--alias <name=cmd>` | Add/remove alias (prefix with `-` to remove by name) |
| `--depends <name>` | Add/remove dependency (prefix with `-` to remove) |
| `--post-apply <cmd>` | Add/remove post-apply script (prefix with `-` to remove) |
| `--private-files` | Mark all `--file` entries as private (local-only, excluded from git) |
| `--set <key=value>` | Helm-style override (repeatable) |
| `--description <text>` | Set description |

### `cfgd module edit <name>`

Open module.yaml in `$EDITOR`.

### `cfgd module delete <name>`

Delete a local module. Any files that were adopted (moved into the module and symlinked back) are automatically restored to their original locations before the module directory is removed.

```sh
cfgd module delete nvim                   # restores symlinked files, then deletes modules/nvim/
cfgd module delete nvim -y                 # skip confirmation
cfgd module delete nvim --purge            # remove deployed target files instead of restoring them
cfgd module delete nvim --ignore-not-found # exit 0 if nvim doesn't exist
```

| Flag | Description |
|---|---|
| `--purge` | Remove files deployed by this module to target locations instead of restoring symlinks |
| `--ignore-not-found` | Exit `0` with a no-op message instead of erroring (exit `6`) when the module doesn't exist; the in-use guard (referenced by a profile) still applies |

### `cfgd module upgrade <name>`

Upgrade a remote (locked) module to a new version.

```sh
cfgd module upgrade tmux                     # latest available
cfgd module upgrade tmux --ref tmux/v2.0     # specific version
cfgd module upgrade tmux --yes               # skip confirmation
cfgd module upgrade tmux --allow-unsigned    # allow unsigned modules
```

### `cfgd module search <query>`

Search configured registries for modules matching a query.

### `cfgd module registry`

Manage module registries.

```sh
cfgd module registry add cfgd-community/modules            # GitHub shorthand
cfgd module registry add https://github.com/cfgd-community/modules.git
cfgd module registry add https://github.com/myorg/modules.git --name myorg
cfgd module registry add https://gitlab.example.com/myorg/modules.git --name myorg
cfgd module registry list
cfgd module registry remove community
cfgd module registry remove community --ignore-not-found  # exit 0 if absent
cfgd module registry rename community cfgd-community
```

The URL may be any git URL, or the GitHub shorthand `owner/repo`. Only GitHub URLs
can supply a default registry name (the org), so pass `--name` when adding a
registry hosted anywhere else.

`module registry remove --ignore-not-found` exits `0` with a no-op message
instead of the strict not-found error (exit `6`) when the registry is absent.

### `cfgd module push <dir>`

Push a module directory (containing `module.yaml`) to an OCI registry as an artifact.

```sh
cfgd module push ./my-module --artifact ghcr.io/me/my-module:1.0.0
cfgd module push ./my-module --artifact ghcr.io/me/my-module:1.0.0 --sign --attest
```

| Flag | Description |
|---|---|
| `--artifact <ref>` | OCI artifact reference (required, e.g. `ghcr.io/myorg/mymodule:v1.0.0`) |
| `--platform <os/arch>` | Platform annotation (default: auto-detected from OS/arch) |
| `--apply` | Apply the module after pushing |
| `--sign` | Sign with cosign (keyless by default) |
| `--key <path>` | Signing key path |
| `--attest` | Attach SLSA provenance attestation |

### `cfgd module pull <ref>`

Pull a module artifact from an OCI registry into a local directory.

```sh
cfgd module pull ghcr.io/me/my-module:1.0.0 --dir modules/my-module
cfgd module pull ghcr.io/me/my-module:1.0.0 --dir modules/my-module --require-signature
```

| Flag | Description |
|---|---|
| `--dir <path>` | Directory to extract the module into (required) |
| `--require-signature` | Require a cosign signature on the artifact |
| `--verify-attest` | Verify the SLSA provenance attestation |
| `--key <path>` | Public key for signature verification |
| `--certificate-identity <id>` | Expected certificate identity for keyless verification |
| `--certificate-oidc-issuer <url>` | Expected OIDC issuer for keyless verification |

### `cfgd module build <dir>`

Build a module into an OCI-ready artifact using Docker or Podman.

```sh
cfgd module build ./my-module --artifact ghcr.io/me/my-module:1.0.0
cfgd module build ./my-module --target linux/amd64,linux/arm64
```

| Flag | Description |
|---|---|
| `--target <platforms>` | Target platform(s), comma-separated (e.g. `linux/amd64,linux/arm64`) |
| `--base-image <ref>` | Base container image (default: `ubuntu:22.04`) |
| `--artifact <ref>` | OCI artifact reference to tag the build with |
| `--sign` | Sign with cosign |
| `--key <path>` | Signing key path |

### `cfgd module keys`

Manage cosign signing keys for module artifacts.

```sh
cfgd module keys generate --dir keys/          # new key pair
cfgd module keys list                          # known signing keys
cfgd module keys rotate --dir keys/ --artifacts ghcr.io/me/my-module:1.0.0
```

`generate` writes a new cosign key pair (`-d`/`--dir`, default: current
directory). `rotate` generates a new pair in place of the `cosign.key` in
`--dir` and re-signs the artifacts named by `--artifacts` (repeatable).

## Source Commands

### `cfgd source add <url>`

Subscribe to a config source.

```sh
cfgd source add git@github.com:acme/dev-config.git \
  --priority 500 \
  --accept-recommended \
  --sync-interval 1h
```

| Flag | Description |
|---|---|
| `--name <name>` | Name for this source (default: inferred from URL) |
| `--branch <name>` | Git branch to subscribe to (default: the remote's default branch) |
| `--priority <n>` | Priority for conflict resolution (default: 500; local config is 1000) |
| `--accept-recommended` | Accept recommended items |
| `--opt-in <item>` | Opt in to specific items (repeatable) |
| `--sync-interval <dur>` | Sync interval (`30m`, `1h`, `6h`) |
| `--auto-apply` | Reconcile and apply immediately after a refresh that changed this source, regardless of `daemon.reconcile.driftPolicy`; items awaiting a decision stay withheld |
| `--pin-version <range>` | Pin to a semver version range (`~1.0`, `>=2.0`) |
| `--require-signed-commits` | Demand a valid signature on this source's HEAD commit. Enforced by the subscribing clone itself, so an unsigned HEAD refuses the subscription |
| `--allow-scripts` | Let this source's lifecycle scripts run even when its own `constraints.noScripts` would reject them |

The URL may be any git URL or the GitHub shorthand `owner/repo`, both equally
supported:

```sh
cfgd source add acme/dev-config                              # GitHub shorthand
cfgd source add https://github.com/acme/dev-config.git
cfgd source add https://gitlab.example.com/acme/dev-config.git
```

A source origin must be a remote. A local path (absolute, relative or
`file://`) is refused, because a source delivers files, packages and scripts
to this machine and its origin has to be something a subscriber can fetch, pin
and verify rather than a directory anything on the host can rewrite. An
existing local path is never silently expanded into a GitHub URL either: it is
reported as the local path it is. To try a source out before publishing it, see
[testing a source locally](sources.md#testing-a-source-locally).

### `cfgd source list`

List subscribed sources.

```
Sources
Name       Source                                   Priority  Status  Last Sync  Signed
───────────────────────────────────────────────────────────────────────────────────────
acme-corp  https://github.com/acme-corp/dev-config  500       Active  2h ago     yes
```

`Last Sync` is the age of the last successful fetch (`never` when the source has
not been fetched yet); `Signed` says whether that fetched commit carried a
verified signature. Both read from recorded state rather than from `cfgd.yaml`,
which is why they are on the default table: they are the columns that change
between one listing and the next. A column no listed source can fill is left off
the table rather than padded with `-` (`Commit` and `Signed` before the first
fetch, `Drift`, which only the daemon's own `cfgd daemon status` holds); the
`-o json` / `-o yaml` payload keeps every field, with the exact ISO 8601 instant
in `lastFetched`. A row nothing declares (the implicit `local` layer on
`cfgd daemon status`) reads `-` in `Source`, `Priority` and `Requires Signed`
rather than a default, and carries `null` there on the wire.
`--wide` adds a `Version` column carrying the source's self-reported
`metadata.version`.

### `cfgd source show <name>`

Show source details, provided profiles, policy breakdown, conflicts, and the
modules the source delivers (its manifest `provides.modules` allow-list). The
delivered modules appear under a `Modules` section in human output and as a
`modules` array in the structured (`-o json`/`-o yaml`) payload.

A `Policy` section shows what is actually enforced on the source, so an
operator can audit it without opening the manifest YAML: the manifest's
`policy.constraints` combined with this machine's own `subscription`
overrides:

```
Policy
  Require Signed Commits  true
  Scripts Allowed         false
  Secrets Read Allowed    false
  System Changes Allowed  false
  Allowed Target Paths    ~/.config/**, ~/.bashrc
```

`Require Signed Commits` is the OR of the manifest's `constraints.requireSignedCommits`
and this machine's `subscription.requireSignedCommits` (either side asking is enough).
`Scripts Allowed` is this machine's `subscription.allowScripts` OR the manifest not
constraining scripts at all (`constraints.noScripts: false`). The rest
(`Secrets Read Allowed`, `System Changes Allowed`, `Allowed Target Paths`) read straight
from the manifest's own constraints. The section (and its `policy` object under
`-o json`/`-o yaml`) is omitted when the manifest could not be loaded, since the
constraints it would combine with are unknown.

### `cfgd source remove <name>`

Remove a subscription. The source's cached clone (under
`<state-dir>/sources/<name>`) is deleted as part of removal, so a later
re-subscription clones fresh rather than reusing stale contents.

```sh
cfgd source remove acme-corp --keep-all          # keep resources as local
cfgd source remove acme-corp --remove-all        # remove everything
cfgd source remove acme-corp --yes --remove-all  # remove everything, no prompts
cfgd source remove acme-corp --ignore-not-found  # exit 0 if acme-corp isn't subscribed
```

`--ignore-not-found` exits `0` with a no-op message instead of the strict
not-found error (exit `6`) when no source by that name is subscribed.

Removing the source's records also drops the content hash cfgd recorded for
each file it deployed. Any file whose bytes no longer match that hash is listed
by path as a warning, and the removal asks for confirmation first (`--yes`
skips it). See [Source Removal](sources.md#source-removal).

Like every `source` verb that edits the composition (`add`, `replace`,
`override`, `priority`), a successful removal closes on the step that settles
it: ``→ Run `cfgd plan` to preview changes, then `cfgd apply` ``.

### `cfgd source update [name]`

Fetch latest from sources (all or specific). Exits non-zero
(`1`, `ExitCode::Error`) if any source fails to update, so CI can detect a
failed refresh from `$?` alone; the per-source failure is also printed.

The two subscriber-side trust knobs are settable here as `--flag`/`--no-flag`
pairs, each of which requires a named source:

| Flag | Description |
|---|---|
| `--require-signed-commits` | Start demanding a valid signature on this source's HEAD commit |
| `--no-require-signed-commits` | Stop demanding one. A source whose own manifest demands one is still verified |
| `--allow-scripts` | Start letting this source's lifecycle scripts run |
| `--no-allow-scripts` | Stop letting them run |

```sh
cfgd source update acme --require-signed-commits
cfgd source update acme --no-allow-scripts
```

An omitted flag leaves the stored value alone, so a plain `cfgd source update`
never resets a demand. The edit is written **after** the fetch: it records what
every future fetch must satisfy, so setting it does not retroactively fail the
update that set it. The next `cfgd sync` is where an unsigned HEAD is refused,
which is what a successful update that changed the demand points at:

```console
$ cfgd source update acme --require-signed-commits
Update source:acme
  ✓ Require Signed Commits — no → yes

✓ Updated 1 source

→ Run `cfgd sync` to fetch under the new policy
```

An update that changed the composition instead (new content fetched, or
`--allow-scripts` / `--no-allow-scripts`) closes on the plan/apply hint the rest
of the family uses. The verdict carries a count because `update` can run over
every source at once; the single-subject verbs (`add`, `remove`, `replace`,
`override`, `priority`) close bare (`✓ Subscribed`, `✓ Removed`).

### `cfgd source override <source> <action> <path> [value]`

Override or reject a source's recommendation.

```sh
cfgd source override acme-corp reject packages.brew.formulae kubectx
cfgd source override acme-corp set env.EDITOR "nvim"
```

### `cfgd source priority <name> [value]`

Set or view source priority.

### `cfgd source replace <old> <new-url>`

Replace one source with another. The new URL accepts the same forms as
`cfgd source add`: any git URL, or the GitHub shorthand `owner/repo`.

```sh
cfgd source replace acme newco/dev-config                    # GitHub shorthand
cfgd source replace acme https://gitlab.example.com/newco/dev-config.git
```

### `cfgd source create`

Create a new `cfgd-source.yaml` in the current directory.

### `cfgd source edit`

Open `cfgd-source.yaml` in `$EDITOR`.

## Secret Commands

### `cfgd secret`

Encrypt, decrypt, and edit SOPS-managed secret files in the config repository.

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt <file>          # encrypt values in place
cfgd secret decrypt <file>          # decrypt to stdout
cfgd secret edit <file>             # decrypt, edit, re-encrypt
```

No command-specific flags; each subcommand takes the file to operate on. See
[Secrets](secrets.md) for the backend matrix (SOPS/age, 1Password, Bitwarden, Vault), key
management, and how encrypted values are referenced from a profile.

## Daemon Commands

### `cfgd daemon`

Run the reconcile loop in the foreground, or manage it as a system service.

```sh
cfgd daemon                # run in foreground (default when no subcommand given)
cfgd daemon run            # run in foreground (explicit)
cfgd daemon install        # install as a system service (systemd / launchd / Windows Service)
cfgd daemon status         # check running state, PID, and socket path
cfgd daemon uninstall      # stop the daemon and remove the service
```

No command-specific flags; the daemon's behaviour is configured by the `daemon` block in
`cfgd.yaml`. See [Daemon](daemon.md) for the timer set, drift policy, live config reload
(`SIGHUP`), and which fields require a restart.

## Decision Commands

### `cfgd decide [action] [resource]`

Accept or reject pending source decisions.

```sh
cfgd decide                                # list pending decisions
cfgd decide accept packages.brew.k9s       # accept one item
cfgd decide reject packages.brew.stern     # reject one item
cfgd decide accept --source acme-corp      # accept all from source
cfgd decide accept --all                   # accept everything
```

Bare `cfgd decide` lists the decisions still awaiting you. Only rows whose source is
still in `spec.sources` are listed: a decision outliving its subscription can no longer
withhold anything, so there is nothing left to accept or reject. `cfgd status` reports
the same filtered set.

A listed row whose entry a higher-priority layer already owns is annotated
`(outranked by <owner>)` — accepting it records your answer, and the apply that
follows writes nothing. See
[Items a Higher Layer Already Wins](sources.md#items-a-higher-layer-already-wins).

Answering an item **records only that item**: an item `cfgd plan` classified but nothing
has recorded yet is minted and resolved in the same step, and no source hash is stamped,
so the daemon's notification for the source's *other* new items is preserved. If the
classification needed to see unrecorded items cannot be built (an unreadable config or
composition), a resolving `cfgd decide` refuses with the reason instead of reporting the
decision as not found; already-recorded rows still resolve, and the bare listing shows
them with a warning that the unrecorded ones could not be read. The refusal only applies
where unrecorded items could exist: with no config file, or a config with no
`spec.sources`, decide answers from the store alone and never runs the classification.

## Backup Commands

### `cfgd backup`

Run, inspect, restore, or roll back the declarative backups a profile declares in
`spec.backups[]`.

```sh
cfgd backup run                                       # run every backup declared in the active profile
cfgd backup run notes-db                              # run only the named backup
cfgd backup list                                      # inventory + snapshot count + last-run status + next scheduled run; alias: ls
cfgd backup list notes-db                             # only that unit's row
cfgd backup list notes-db --snapshots                 # its snapshots: name, kind, created, size
cfgd backup restore notes-db                          # newest snapshot, back over the source
cfgd backup restore notes-db --at 20260730T120000Z    # pick an older one
cfgd backup restore notes-db --to /tmp/inspect --yes  # somewhere else, no prompt
cfgd backup rollback                                  # what has a pre-restore copy beside it
cfgd backup rollback notes-db --yes                   # put that copy back over the source
cfgd --output json backup list
```

An unknown name given to `cfgd backup run`, `backup list`, `backup restore`, or
`backup rollback` is exit code `6`
(see [Exit Codes](#exit-codes)) and lists every valid name; an unknown `--at` snapshot is exit `6`
too and lists every available snapshot. A run that recorded a failure (a bad copy, or
`postBackup` erroring after a good one) also exits nonzero.

`backup restore` overlays the snapshot onto the target (names only in the target are left alone;
a target entry whose kind differs from the snapshot's (a symlink, or a directory where the
snapshot holds a file) is removed and replaced, never written through), leaves a safety copy of the current contents beside the source first (the same `<path>.cfgd-backup` sidecar cfgd leaves beside any file it displaces; not a snapshot of the unit), and requires confirmation
unless `--yes` (`CFGD_YES`) is given. `--to <path>` redirects the restore; a path outside the
backup's source also skips the safety copy, while a path at or inside the source still takes
one. The unit's `preBackup` / `postBackup` hooks wrap the whole restore exactly once and see
`CFGD_OPERATION=restore`. Where cfgd cannot prompt (piped stdin, CI, or `-o json`) a restore
without `--yes` is an error rather than a silent no-op. See [Restoring](backups.md#restoring).

`backup rollback <name>` puts the safety copy back over the source, undoing a restore of the
wrong snapshot. It runs through the same envelope: the unit's lock, the one `preBackup` /
`postBackup` hook list (seeing `CFGD_OPERATION=rollback`), and the same confirmation and `--yes`
rule, including the safety copy: the contents it displaces are copied aside as their own sidecar
first — symlinks inside a directory source included — so a rollback is itself reversible and a
second one returns the source to where it started. A failed safety copy refuses the rollback
before anything is written. The primary `<source>.cfgd-backup` sidecar (the first displacement,
never pruned) plus at most one stamped copy (the newest displacement) both survive; a new
displacement prunes only the older stamped sidecars cfgd itself wrote for that path, and nothing
else in the directory. With no name it lists what it could put back and changes nothing; a unit
with no copy beside its source is exit `6`, pointed at `cfgd backup list <name>` for its snapshots
rather than at the restore that would create one. See
[Rolling back a restore](backups.md#rolling-back-a-restore).

A unit that is already running elsewhere (the daemon's timer, another `cfgd apply`) is refused
rather than interleaved: `backup run` reports the holding process as a skip and exits `1`, while the
other units it was asked to run still run. See
[One run at a time](backups.md#run-semantics).

Structured output (`-o json`) payload for `backup run`: an array of
`{ name, status, clean, destinationPath?, error? }`, where `status` is `success`, `failed`, or
`skipped` (the unit was already running). A refused unit does not add a second document to stdout:
the payload is always one JSON value and the nonzero exit code carries the failure. For
`backup list`: an array of
`{ name, source, schedule?, retention, snapshots?, lastRunStatus?, lastRunAt?, lastRunClean?, nextRunAt? }`.
For `backup list <name> --snapshots`: an array of `{ name, created, sizeBytes }`, newest first,
where `name` is the snapshot's path relative to the backup's `destination`. A restore's safety
copy is a sidecar beside the source, so it appears in neither list and is never the unit's
`lastRunAt`. For `backup restore`:
a single `{ name, snapshot, restoredTo, restored, clean, sizeBytes, safetyCopy?, safetyCopyReused?, error? }`;
when the operator declines at the confirmation prompt,
`{ name, snapshot, restoredTo, restored: false, declined: true }`. The declined payload omits
`clean` deliberately: a decline exits `0`, and reporting `clean: false` beside a zero exit would
contradict whichever of the two a consumer trusted.
For `backup rollback <name>`: a single
`{ name, copy, restoredTo, restored, clean, sizeBytes, safetyCopy?, safetyCopyReused?, error? }`,
and on a decline
`{ name, copy, restoredTo, restored: false, declined: true }` — the same split, for the same
reason. For `backup rollback` with no name: an array of `{ name, copy, created, sizeBytes }`,
one per unit that has a copy beside its source, where `created` is the copy's modification time
(a sidecar carries no record of its own).
`nextRunAt` is the ISO 8601 UTC time the daemon's timer will next fire the unit, computed from the
same `schedule` + last `finished_at` seeding the daemon uses; it is omitted for a schedule-less
unit (the `Next Run` column renders `-`). See [Declarative Backups](backups.md#cli).

`backup run` always runs the units it names, schedule or not. A backup that declares a `schedule`
additionally runs on the [daemon's timer](backups.md#daemon-scheduling), and a schedule-less one
runs during `cfgd apply`.

## Compliance Commands

### `cfgd compliance`

Collect a compliance snapshot of the machine (every managed file, package, and system setting
scored against the effective desired state) and inspect the stored history.

```sh
cfgd compliance                          # collect and store a snapshot, print the summary
cfgd compliance export                   # write the newest snapshot out
cfgd compliance history                  # list stored snapshots, newest first
cfgd compliance history --since 30d      # only snapshots newer than a duration
cfgd compliance diff <base-id> <target-id>   # what changed between two snapshots
```

| Flag | Meaning |
|---|---|
| `--since <duration>` | (on `history`) only snapshots newer than `30d` / `12h` / `90m` |

`cfgd compliance` run by hand always stores its snapshot. The daemon stores one only when the
machine's content hash changed, so **history records changes, not ticks**; see
[spec.compliance](spec/config.md#speccompliance) before treating row arrival as a liveness signal.
Snapshot IDs for `diff` come from `history`.

## Image Commands

### `cfgd image pack <DIR> <ARTIFACT>`

Pack a directory into a standard OCI image and push it to a registry. The result is
mountable as a Kubernetes `volume.image` (KEP-4639) via containerd. No Dockerfile or
Docker daemon required.

```sh
cfgd image pack ./out registry.example.com/myapp:v1.4.0
cfgd image pack ./out registry.example.com/myapp:v1.4.0 --sign --attest
cfgd image pack ./out registry.example.com/myapp:v1.4.0 --platform linux/arm64
cfgd image pack ./out registry.example.com/myapp:v1.4.0 -o json
```

| Flag | Description |
|---|---|
| `--platform <os/arch>` | Target platform (default: host, e.g. `linux/amd64`) |
| `--entrypoint <arg>` | Image entrypoint, repeatable |
| `--cmd <arg>` | Default command arguments, repeatable |
| `--env KEY=VALUE` | Runtime environment variable, repeatable |
| `--working-dir <path>` | Working directory for the entrypoint |
| `--user <user>` | User/UID for the entrypoint |
| `--label k=v` | Image config label (`→ config.Labels`), repeatable |
| `--annotation k=v` | Manifest annotation, repeatable |
| `--sign` | Sign with cosign (keyless by default) |
| `--key <path>` | Signing key path |
| `--attest` | Attach SLSA provenance attestation |
| `--base <ref>` | Base image to layer the packed directory on top of (e.g. `ghcr.io/org/app:v1`) |
| `--lock [<file>]` | Record the resolved digest in an image lockfile for `kubectl cfgd deploy` (default file: `cfgd-images.lock`) |

Structured output (`-o json`) payload: `{ artifact, digest, platform, signed, attested }`.

See [image-pack.md](image-pack.md) for the full reference, worked example, and Pod spec.

## Other Commands

### `cfgd config show`

Show the current cfgd.yaml configuration. Alias: `cfgd config ls`.

### `cfgd config edit`

Open cfgd.yaml in `$EDITOR`.

### `cfgd config get <key>`

Get a config value by dotted key path. Outputs raw value to stdout (suitable for scripting).

```sh
cfgd config get profile                      # → work
cfgd config get theme.name                   # → dracula
cfgd config get theme                        # prints the theme block (name + overrides)
cfgd config get daemon.reconcile.interval    # → 5m
cfgd config get fileStrategy                 # → Symlink
cfgd config get aliases.add                  # → profile update --file
cfgd config get daemon                       # prints full daemon YAML block
```

### `cfgd config set <key> <value>`

Set a config value by dotted key path. Creates intermediate sections as needed.

```sh
cfgd config set profile personal
cfgd config set theme dracula
cfgd config set theme.name minimal
cfgd config set daemon.reconcile.interval 10m
cfgd config set daemon.enabled true
cfgd config set fileStrategy Copy
cfgd config set aliases.deploy "apply --yes"
```

### `cfgd config unset <key>`

Remove a config value (resets to default). Alias: `cfgd config rm`.

```sh
cfgd config unset theme                          # remove entire theme section
cfgd config unset daemon.reconcile.autoApply    # reset single field
cfgd config unset aliases.deploy                 # remove an alias
```

### `cfgd workflow generate`

Generate GitHub Actions workflows for config repo releases.

```sh
cfgd workflow generate --force   # overwrite existing (the global --yes / CFGD_YES=1 also overwrites)
```

Profiles whose YAML fails to parse are skipped with a warning naming the file and the parse error; the remaining valid profiles still generate.

Tags are immutable. A changed module is tagged `<name>/v<version>` from its [`metadata.version`](spec/module.md#metadataversion) (read through `cfgd module show`, never guessed) and the job fails if the module declares no version or if that tag already exists (bump `metadata.version`). A changed profile is tagged `profile/<name>/<UTC timestamp>` in `%Y%m%dT%H%M%SZ` form, so a second release on the same day gets its own tag. Nothing is force-pushed. The job installs the same cfgd version that generated the workflow, pinned in the job's `CFGD_VERSION` environment variable; re-run `cfgd workflow generate --force` after upgrading cfgd to move the pin.

The generated workflow's change detection covers both profile manifest forms, the flat file (`profiles/<name>.yaml`) and the bundle directory (`profiles/<name>/**`), so a push touching either layout tags a release. Names containing regex metacharacters (e.g. `web.app`) are matched literally, and matching is exact: a change to a sibling profile whose name extends another (`profiles/work.app.yaml`) does not flag `work`. Generation fails if two names would fold to the same job-output key (`web.app` and `web-app` both fold to `profile_web_app`); rename one so they stay distinct.

### `cfgd checkin`

Check in with the device gateway.

```sh
cfgd checkin --server-url https://cfgd.acme.com --api-key <key>
```

| Flag | Description |
|---|---|
| `--server-url <url>` | Device gateway URL |
| `--api-key <key>` | Device API key (issued at enrollment) |
| `--device-id <id>` | Device identifier to report as (default: derived from the enrollment credential) |

### `cfgd enroll`

Enroll with a device gateway using token or key-based verification.

```sh
cfgd enroll --server-url https://cfgd.acme.com --token <bootstrap-token>
cfgd enroll --server-url https://cfgd.acme.com --ssh-key ~/.ssh/id_ed25519
cfgd enroll --server-url https://cfgd.acme.com --gpg-key ABCD1234
```

| Flag | Description |
|---|---|
| `--server-url <url>` | Device gateway URL |
| `--token <token>` | Bootstrap token for token-based enrollment |
| `--ssh-key <path>` | SSH key for key-based enrollment |
| `--gpg-key <id>` | GPG key ID for key-based enrollment |
| `--username <name>` | Username to enroll as (default: current system user) |

#### Enrollment Methods

The server's enrollment method is configured by the administrator. cfgd auto-detects which method the server requires.

| Method | How it works | Best for |
|---|---|---|
| **Token** | Admin generates a short-lived bootstrap token, gives it to the user. User exchanges it for a permanent device credential. | Quick onboarding, automated provisioning |
| **SSH key** | Admin pre-registers the user's SSH public key. User proves possession via challenge-response signing. | Teams already using SSH keys for git access |
| **GPG key** | Admin pre-registers the user's GPG public key. User proves possession via challenge-response signing. | Teams with existing GPG infrastructure |

**Challenge-response flow (SSH/GPG):**

1. cfgd contacts the server and requests a challenge nonce
2. The server generates a random nonce with a 5-minute TTL
3. cfgd signs the nonce with your local key
4. cfgd sends the signature back to the server
5. The server verifies the signature against pre-registered public keys
6. On success, the server returns a permanent device API key

**Key auto-detection:** If neither `--ssh-key` nor `--gpg-key` is specified, cfgd checks the SSH agent first, then falls back to `~/.ssh/id_ed25519`, `~/.ssh/id_rsa`, and `~/.ssh/id_ecdsa` in order. The first available key is used.

### `cfgd alias`

Manage user-level CLI aliases stored in `cfgd.yaml` under `spec.aliases`: shorthands for cfgd
invocations you type often.

```sh
cfgd alias set pu "profile update --file"   # add or update; alias: add
cfgd alias show pu                          # print the command a single alias expands to
cfgd alias list                             # alias: ls
cfgd alias delete pu                        # alias: rm
```

No command-specific flags. `set` takes `<NAME> <COMMAND>`, where `COMMAND` is the argument string
the alias expands to. Aliases live in the config file, so they travel with the config repository.

### `cfgd man`

Emit a `roff(7)` man page for cfgd on stdout.

```sh
cfgd man > cfgd.1 && man ./cfgd.1
cfgd man > /usr/local/share/man/man1/cfgd.1
```

No command-specific flags.

### `cfgd help [command]`

Print the top-level synopsis, or the help for a named command. Identical to `--help`:
`cfgd help backup` and `cfgd backup --help` produce the same page.

```sh
cfgd help                  # top-level command list
cfgd help backup restore   # help for a nested subcommand
```

### `cfgd completion <shell>`

Generate shell completions.

```sh
# Add to your shell's rc file
source <(cfgd completion bash)  # .bashrc
source <(cfgd completion zsh)   # .zshrc
cfgd completion fish | source   # config.fish
```

## Exit Codes

Scripted consumers rely on distinct exit codes to decide follow-up actions without parsing stderr. The taxonomy is stable: breaking changes bump the CLI major version.

| Code | Meaning | Emitted by |
|---|---|---|
| `0` | Operation succeeded. | All commands on success. |
| `1` | Generic failure (network, IO, unclassified internal error). Also a `cfgd backup run` that recorded a failed or unclean snapshot (see [Run Semantics](backups.md#run-semantics)), a `cfgd backup restore` or `cfgd backup rollback` whose overlay, safety copy or hooks failed, and `cfgd diff --exit-code`, `cfgd status --exit-code`, or `cfgd verify --exit-code` when a system configurator's own check failed (drift is undetermined rather than absent, which outranks `5`). | Any command whose `Result` resolves to a non-config error, and `cfgd diff --exit-code` / `cfgd status --exit-code` / `cfgd verify --exit-code` on a failed configurator check. |
| `2` | An upgrade is available but not installed. | `cfgd upgrade --check` only. |
| `3` | No cfgd config file at the resolved path. | Any command when `--config` points to a missing file. |
| `4` | Config file exists but failed parse or validation. | Any command when `--config` is malformed or schema-invalid. |
| `5` | Drift detected between actual and desired state. | `cfgd diff --exit-code`, `cfgd status --exit-code`, `cfgd verify --exit-code`. |
| `6` | A named resource was not found. | Any command naming a missing resource: `cfgd module show/delete/edit/export <missing>`, `cfgd profile show/switch/delete/edit/update <missing>`, `cfgd source show/update/remove/priority/override <missing>`, `cfgd module registry remove/rename <missing>`, `cfgd backup run/list/restore/rollback <missing>`, `cfgd backup restore --at <missing-snapshot>`, `cfgd backup rollback <name>` with no copy beside its source, `cfgd init --apply-profile <missing>`, `cfgd init --apply-module <missing>`, `cfgd config get/set/unset <missing-key>`, `cfgd alias show/delete <missing>` (which dispatch into the same config-key lookup with an `aliases.` prefix), `cfgd rollback <missing-apply-id>`. The destructive verbs `module delete`, `module registry remove`, `source remove`, and `profile delete` accept `--ignore-not-found` to exit `0` instead when the target is absent. |
| `7` | An apply ran but at least one action failed (partial or total). Also a schedule-less `spec.backups[]` unit that failed or didn't complete cleanly during `cfgd apply` (see [Apply Integration](backups.md#cli)) — the unit is reported, apply continues, and the overall status downgrades to `partial`. | `cfgd apply`, `cfgd init --apply/--apply-profile/--apply-module`, and `cfgd module create --apply` when one or more actions fail. |
| `130` | `apply` was cooperatively aborted by `SIGINT` (Ctrl-C). | `cfgd apply` interrupted with Ctrl-C; the in-flight action finishes, the lock releases, the run is recorded as `Aborted`. |
| `143` | `apply` was cooperatively aborted by `SIGTERM`. | `cfgd apply` interrupted with `kill`; same cooperative-abort semantics as `130`. |

Codes `130` / `143` follow the POSIX `128 + signal` convention and are not cfgd-specific. See [Graceful Interruption](safety.md#graceful-interruption-sigint--sigterm) for the abort semantics. The `--exit-code` / `-e` flag on `diff`, `status`, and `verify` follows the `git diff --exit-code` convention: without the flag these commands always exit `0`; with the flag they exit `5` whenever drift is present, except that all three exit `1` instead when a system configurator check itself failed, since an unknown state outranks a known one for a script deciding whether to apply.

External-process passthrough (e.g. `kubectl exec` invoked by the `kubectl cfgd` plugin) forwards the inner tool's exit code unchanged; those codes are not part of the cfgd taxonomy.

### Error output

A failure always renders somewhere; where depends on the format, because a
selector format's success shape and an error doc's shape rarely agree:

- **Human (default):** a single `✗` line carrying the error message, to `stderr`, followed by any
  remediation hints (e.g. `Available modules: …`, or `run \`cfgd init\``). The same failure is
  never printed twice.
- **Full-dump structured (`-o json` / `yaml`):** exactly one error object, to `stdout`,
  always, even for an unclassified internal error, so a scripted consumer is never left with
  empty output on failure. The shape is stable:

  ```json
  { "error": "not_found", "name": "web-server", "available": ["base", "dev"] }
  ```

  `error` is a machine-readable kind (`not_found`, `registry_not_found`, `already_exists`,
  `parse_failed`, `key_not_found`, `target_not_writable`, …), `name` identifies the subject
  (module / source / profile / registry / key), and any
  command-specific fields follow. `name` is present only when the failure has a subject to
  report: an empty subject is omitted from the payload rather than serialized as `""`. A
  plain propagated error with no CLI handler attached still gets a real kind: any typed
  `CfgdError` in its chain names its own domain (`config`, `source`, `module`, …), and only a
  genuinely untyped failure falls back to `{ "error": "internal", "message": "<text>" }`.
  Remediation hints are human-only and never appear in the structured payload.
- **Selector structured (`-o name` / `jsonpath=` / `template=` / `template-file=`):** the same
  error message is *always* echoed to `stderr` first, before the selector is evaluated. A
  selector is written against the success shape (`.items[].foo`), and an error doc's shape
  (`error`/`message`/`name`) almost never satisfies one: without the `stderr` echo, a
  non-matching selector printed nothing to `stdout` and nothing anywhere else, leaving only the
  exit code to say a failure happened. If the selector *does* resolve against the error doc's
  fields (e.g. `-o jsonpath={.name}` against a `not_found` error, which does carry `name`), that
  projection additionally prints to `stdout`, so a selector format can render an error twice,
  once as the guaranteed `stderr` diagnostic and once as whatever the selector matched.

### Use in CI

```sh
# Fail the build if the machine has drifted from the committed profile.
cfgd verify --exit-code

# Run upgrade on a schedule but only page humans on real failures.
if ! cfgd upgrade --check; then
  case $? in
    2) echo "Update available: cfgd upgrade to install" ;;
    *) echo "Upgrade check failed" >&2; exit 1 ;;
  esac
fi
```
