# CLI Reference

Complete command reference for `cfgd`. All commands respect [global flags](configuration.md#global-flags).

**Every top-level command has an entry here**, and each entry is the same depth: what the command
does in a sentence, a synopsis of its subcommands and arguments, the flags that are specific to it,
and a link to the topic document when one exists. Global flags are listed once, in
[Configuration](configuration.md#global-flags), and are never repeated per command. Semantics —
what a backup run does when a hook fails, how a source's constraints are enforced — live in the
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
| `--model <model-id>` | Override AI model (default: from config or `claude-sonnet-4-6`) |
| `--provider <name>` | Override AI provider (default: claude) |
| `--yes`, `-y` | Skip confirmation prompts |
| `--scan-only` | Only scan system, don't start AI conversation |

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

Serves the `core` group (nine reconcile commands) by default. `--group` / `--command` / `--tool` widen it, the matching `--hide-` flags narrow it, and `--all` serves the full 86. See [Serving the CLI itself](ai-generate.md#serving-the-cli-itself).

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
cfgd init --from <source> --apply-module nvim         # clone, apply just one module
cfgd init --from <source> --apply --yes --install-daemon  # full one-liner bootstrap
```

`--from` accepts any git URL, a local path, or the GitHub shorthand `owner/repo` —
all equally supported. Only a bare `owner/repo` is expanded, and an existing path
always wins over the shorthand: run inside a directory that holds `you/config`,
`--from you/config` means that directory. A first segment carrying a dot
(`gitlab.example.com/you/config`) is a URL for that host and is passed through
untouched; a dotless first segment cannot be told from a GitHub owner by the
value alone, so name a host like `gitserver` with a scheme
(`http://gitserver/config`). The same rule applies everywhere cfgd takes a
repository reference — `cfgd apply --from`, `cfgd plan --from`,
`cfgd source add`, `cfgd source replace` and `cfgd module registry add`.

| Flag | Description |
|---|---|
| `[path]` | Target directory (default: current directory) |
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--branch <name>` | Git branch (default: master) |
| `--name <name>` | Config name in metadata (default: directory name) |
| `--apply` | Apply configuration after scaffolding |
| `--apply-profile <name>` | Activate and apply a specific profile (implies --apply, exits `6` if not found) |
| `--apply-module <name>` | Apply a specific module (repeatable, implies --apply, errors if not found) |
| `--yes`, `-y` | Skip confirmation prompts (used with --apply) |
| `--install-daemon` | Install daemon service after init |
| `--theme <name>` | Theme name (default, dracula, solarized-dark, solarized-light, minimal) |

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
cfgd apply --module nvim            # single module + deps (no profile required)
cfgd apply --only packages.brew     # dot-notation filter (the brew manager)
cfgd apply --only packages.module:nvim  # a module's package work
cfgd apply --skip module:nvim       # one module, every phase
cfgd apply --skip cfgd:managers     # every package-manager bootstrap
cfgd apply --skip system.sysctl     # skip specific items
cfgd apply --skip-scripts           # apply without running any hooks
```

| Flag | Description |
|---|---|
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--dry-run` | Preview changes without applying (supports `-o json`) |
| `--phase <name>` | Apply only a specific phase |
| `--yes`, `-y` | Skip confirmation prompt |
| `--module <name>` | Apply only this module and its dependencies |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Apply only items matching dot-notation paths (repeatable) |
| `--skip-scripts` | Skip all script hooks (pre/post/onChange) |
| `--context <ctx>` | `apply` (default) or `reconcile` — selects which hooks run |
| `--shell <auto\|sh\|bash\|zsh\|pwsh\|cmd>` | Force every *inline* lifecycle script under this interpreter, overriding each entry's own `shell:`. File and shebang scripts are unaffected. For debugging a script that behaves differently under another shell |

`apply` reconciles exactly what `plan` previews, so a [source item awaiting a
decision](sources.md#automatic-apply-decisions) is not installed by `apply --yes` either.
`cfgd decide` is the only command that resolves one. The same holds for an item your
auto-apply policy declines outright (`newRecommended: Reject`): `plan`, `apply` and the
daemon read one policy, so a manual apply cannot install what the daemon skips. A `Notify`
item — the default — is withheld from the first run that sees it, before any row exists;
`apply` records the row so `cfgd decide` can answer it without waiting for a daemon tick,
while `plan` withholds it read-only.

### `cfgd plan`

Preview the reconciliation plan without applying. This is the canonical preview command — `apply --dry-run` is a convenience that delegates to the same logic.

```sh
cfgd plan                               # preview with default (apply) context
cfgd plan --context reconcile           # preview what the daemon would run
cfgd plan --module nvim                 # plan for a single module
cfgd plan --skip-scripts                # exclude all script hooks
cfgd plan -o json                       # structured plan output
```

| Flag | Description |
|---|---|
| `--from <url\|owner/repo\|path>` | Config source: git URL on any host, GitHub `owner/repo` shorthand, or local path to an existing config directory (an existing path wins over the shorthand) |
| `--phase <name>` | Show only a specific phase |
| `--module <name>` | Plan only this module and its dependencies |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Plan only items matching dot-notation paths (repeatable) |
| `--skip-scripts` | Exclude all script hooks from the plan |
| `--context <ctx>` | `apply` (default) or `reconcile` — selects which hooks to include |

A module delivered by a [ConfigSource](sources.md) is tagged with its origin
just like source-delivered files and packages: the human plan line ends with
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

⊙ 4 action(s) planned
```

`--phase modules` selects every module-owned action wherever it was planned, so
it stays the way to preview "just the modules".

Dot-notation paths give the owner its own segment, so a module named `brew` and
the `brew` package manager never collide:

| Pattern | Selects |
|---|---|
| `packages.brew` | the brew package manager's work |
| `packages.module:nvim` | the `nvim` module's package work |
| `module:nvim` | everything the `nvim` module declares, in every phase |
| `profile:work` | everything the `work` profile declares |
| `cfgd:managers` | every package-manager bootstrap |

`modules` and `modules.<name>` still work and print a deprecation naming their
replacement. Skipping a bootstrap leaves the installs that needed it in the
plan; cfgd warns and prints the `--skip packages.<manager>` flags that would
drop those too.

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

Actions moved one level down, and a phase's module identity is now the group's
owner rather than a `module`/`section` key on the phase:

| Before | Now |
|---|---|
| `jq '.phases[].actions[]'` | `jq '.phases[].groups[].actions[]'` |
| `jq '.phases[] \| select(.module=="nvim")'` | `jq '.phases[].groups[] \| select(.owner.name=="nvim")'` |

A [source](sources.md#automatic-apply-decisions) item still awaiting `cfgd decide` — or one
you rejected — is withheld from the plan: it is absent from the phases, from
`totalActions`, and from what `apply` executes (with `--yes` or with the
confirmation prompt). Both states are named rather than silently missing: the
human render lists them under **Pending Decisions (not included in this plan)**
and **Declined Decisions (not included in this plan)**, and the payload carries
the same rows as `pendingDecisions` and `rejectedDecisions`, each omitted when
empty — so a structured consumer can tell "in sync" from "waiting on you" from
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
decision store: `plan` is read-only, so the row is minted later — by `cfgd
decide` when you answer it, or by the `cfgd apply` / daemon tick that follows.
Every other field carries the same shape either way, and the item is withheld
identically; only a recorded row has a real (non-zero) `id`. The same
discriminator reaches the other two read surfaces: `cfgd status -o json`
(`pendingDecisions`) and the bare `cfgd decide -o json` listing (`decisions`)
fold in the same classified-but-unrecorded rows, `id: 0` included.

A `Notify`-tier package the machine **already satisfies** never lands in
`pendingDecisions` at all: the run's own package enumeration answers the
question and the item is [auto-accepted](sources.md#edge-cases) — previewed as
included by `plan`, recorded as a resolved row with resolution `auto-accepted`
by the writing paths. "Satisfies" is judged against the version the manager's
listing reports (an entry with no version spec is satisfied by any installed
version; `tool@v1.2.3` pins with caret semantics, i.e. `^1.2.3`). A version
conflict instead stays pending with the conflict annotated in the row's
`summary` (e.g. `… — installed 13.0, source wants ^14`), on the recorded row
and the `id: 0` shape alike — a manager whose listing reports no version reads
`installed (version unknown), source wants ^14`.

In the human render, an unrecorded item keeps the usual ``run `cfgd decide
accept/reject` `` instruction only where that command could actually record it.
On a config that cannot mint the row — a foreign `--config` without
`--state-dir` — the suffix says so instead (*not yet recorded; decide from the
machine's own config, or with `--state-dir`*); a recorded row resolves without
a mint, so its instruction holds on every config.

Only a source you are still subscribed to can withhold anything: a decision
whose source has been removed from `spec.sources` is inert, and a real `cfgd
apply` discards it — unless you pointed that run at a FOREIGN config (a
`--config`, `--config-dir`, or `CFGD_CONFIG` that resolves somewhere other
than the default config location) while leaving `--state-dir` at its default,
in which case the rows are left alone because they belong to another config's
picture of the machine. Ownership follows the resolved path, not the spelling:
`--config ~/.config/cfgd/cfgd.yaml` names the machine's own config — the same
`--config` every installed service unit bakes into its invocation — and still
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
```

`pendingDecisions` lists the same rows `cfgd decide` offers, including
classified-but-unrecorded items with `id: 0` (see [`cfgd plan`](#cfgd-plan)).
The dashboard degrades rather than failing on that classification: if it cannot
be built (a malformed package manifest, say), status still renders everything
else and prints a warning naming what it could not read. Under `-o json` the
warning line is suppressed, so the degradation is part of the payload instead —
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

`classificationDegradedCode` is the machine-stable half — a closed set a
consumer can branch on: `decisionStoreUnreadable` (fix the state directory /
database), `sourceUnreadable` (re-sync or inspect the source's cached
config), `manifestUnreadable` (fix the referenced Brewfile / `package.json` /
`Cargo.toml` / apt list), or `classificationFailed` (anything else). The
reason string is the human detail and carries no stability promise.

A source batch no decision row can name — packages under a
[dotted custom manager](sources.md#edge-cases) — is withheld fail-closed, and
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

The bare `cfgd decide -o json` listing carries the same fields — the
degradation pair and `warnings` alike — alongside its `decisions` array.

### `cfgd diff`

Show detailed file diffs with syntax highlighting.

```sh
cfgd diff                    # human drift report
cfgd diff --module nvim      # a single module's resources
cfgd diff --exit-code        # exit 5 on drift, for CI gating
cfgd diff -o json            # structured drift payload
```

The human render uses the plan's own axes — phase, then owner group — so a drifted
resource is named by the same coordinates the plan and apply trees would use to fix it:

```
Diff
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  work

Phase: Files
  profile:work
    ⊙ /home/you/.gitconfig (new file)
[user]
	name = You
  module:nvim
    ⊙ /home/you/.config/nvim/init.lua (new file)
-- init
    ⊙ /home/you/.config/nvim/lua/opts.lua (new file)
-- opts
  ⚠ File drift detected

Phase: Packages
  profile:work
    ⚠ brew: missing — extra-tool
    ⚠ nix: missing  — hello
  cfgd:managers
    ⚠ nix: not installed — can bootstrap via nix installer

Phase: System
  profile:work
    ⚠ sysctl.net.core.somaxconn — want 8192, have 4096

⚠ Drift detected
```

File bodies render at column 0 under the file they belong to, so a diff hunk stays
copy-pasteable.

The payload carries `files[]`, `packages[]`, `system[]`, and a `summary`. `files[]` lists only the managed files that do NOT match desired state, in the same shape `cfgd verify` reports a resource:

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

A file cfgd could not evaluate — an unparseable target, a filter that exited non-zero, a patch script a source is [barred from running](sources.md#noscripts) — appears with the reason as its `actual`, so the cause is visible without reading the terminal rendering.

A managed file whose `source` cannot be found is reported as drift here and by `cfgd verify` / `cfgd status`: the desired content could not be determined, which is never the same as convergence.

### `cfgd verify`

Check that all managed resources match desired state.

```sh
cfgd verify -o json          # structured pass/fail results
cfgd verify --module nvim    # verify only a single module's resources (no profile required)
```

Each entry in `results[]` carries `resourceType`, `resourceId`, `matches`, `expected`, and `actual`, alongside the top-level `passCount` / `failCount`.

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
as does a supported legacy-flat layout — warnings do not affect the exit code.

### `cfgd log`

Show apply history from the state store.

```sh
cfgd log                    # last 20 entries
cfgd log --limit 50         # last 50 entries
cfgd log -o json            # JSON apply history
cfgd log --show-output 42   # show captured script output for apply #42
```

### `cfgd rollback <apply-id>`

Restore the file backups cfgd took before a previous apply, undoing that apply's file writes.

```sh
cfgd log                          # find the apply ID
cfgd rollback 42                  # restore the files that apply overwrote
cfgd rollback 42 --yes            # skip the confirmation
cfgd rollback 42 -o json          # structured result
```

| Flag | Meaning |
|---|---|
| `--yes` / `-y` | skip the confirmation prompt (also `CFGD_YES=1`) |

Rollback covers **cfgd's own file writes** — the pre-overwrite backups stored inline in the state
database, not the declarative `spec.backups[]` snapshots that
[`cfgd backup restore`](#cfgd-backup) puts back. Packages installed and system settings changed by
that apply are not reverted. See [File Safety](safety.md#file-backups) for what is captured and for
how long.

### `cfgd sync`

Pull from all remotes, show changes, prompt for apply.

### `cfgd pull`

Pull remote changes (git pull only, no apply).

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
`<archive>.sha256` file — pinned to a canonical-repo workflow identity (the
`publish-crate.yml` legs that `release.yml` invokes do the signing) — then
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
registry), so `explain` always matches what cfgd actually accepts.

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
operator's admission webhook enforces — one shared implementation, so a document
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
| `--yes` / `-y` | skip the overwrite confirmation (also `CFGD_YES=1`) |
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
| `--package <mgr:pkg>` | Add package (repeatable) |
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

```sh
cfgd profile update --package brew:jq
cfgd profile update work --module new-tool --module -old-tool
cfgd profile update work --package brew:jq --package -brew:unused --alias vim=nvim --alias -old
```

| Flag | Description |
|---|---|
| `--inherit <name>` | Add/remove inherited profile (prefix with `-` to remove) |
| `--module <name>` | Add/remove module (prefix with `-` to remove) |
| `--package <mgr:pkg>` | Add/remove package (prefix with `-` to remove) |
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
(exit `6`). It only affects the not-found case — deleting the active profile
still fails (exit `1`).

When the profile's directory still holds payload files (e.g. `files/`), a
second confirmation gates removing the directory too; declining keeps it in
place. Both confirmations are gathered before anything is deleted, so aborting
at either prompt (Ctrl-C/EOF) leaves the profile fully intact. `--yes` skips
both confirmations.

### `cfgd profile migrate [name]`

Move a legacy flat profile manifest (`profiles/<name>.yaml`) into the canonical
bundle layout (`profiles/<name>/profile.yaml`). The bundle directory may already
exist holding `files/` — the manifest joins its payload. Uses `git mv` when the
config directory is a git work tree (preserving history), a plain rename
otherwise. If a manifest is tracked but `git mv` fails (e.g. index lock
contention), a warning is printed and the move falls back to a plain rename —
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
| `-y`, `--yes` | Skip the confirmation prompt (`CFGD_YES`) |

Idempotent: already-canonical profiles report "already canonical" and are left
untouched. With `--all`, migration continues past per-profile failures and exits
non-zero if any profile failed (each is reported). An ambiguous profile — both
`profiles/work/profile.yaml` and `profiles/work.yaml` present — is refused as a
failure rather than migrated.

## Module Commands

### `cfgd module list`

List all available modules with status (installed, pending, outdated, error).

### `cfgd module show <name>`

Show module details: packages, files, dependencies, resolved managers. Env variable values are masked by default (shows `***` with last 3 chars).

```sh
cfgd module show my-tool                # env values masked
cfgd module show my-tool --show-values  # reveal full env values
```

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
| `--package <name>` | Add package (repeatable) |
| `--file <path>` | Import file (repeatable) |
| `--private-files` | Mark files as private |
| `--env <key=value>` | Set env var (repeatable) |
| `--alias <name=command>` | Set shell alias (repeatable) |
| `--post-apply <cmd>` | Post-apply script (repeatable) |
| `--set <key=value>` | Helm-style override (repeatable) |

### `cfgd module update <name>`

Modify a local module. Prefix a value with `-` to remove it.

```sh
cfgd module update nvim --package fd --package -unused
cfgd module update nvim --depends node --env EDITOR=nvim --alias vim=nvim
```

| Flag | Description |
|---|---|
| `--package <name>` | Add/remove package (prefix with `-` to remove) |
| `--file <path>` | Add/remove file (prefix with `-` to remove by target) |
| `--env <KEY=VALUE>` | Add/remove env var (prefix with `-` to remove by key) |
| `--alias <name=cmd>` | Add/remove alias (prefix with `-` to remove by name) |
| `--depends <name>` | Add/remove dependency (prefix with `-` to remove) |
| `--post-apply <cmd>` | Add/remove post-apply script (prefix with `-` to remove) |
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
| `--yes`, `-y` | Skip confirmation prompt |
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

## Source Commands

### `cfgd source add <url>`

Subscribe to a config source.

```sh
cfgd source add git@github.com:acme/dev-config.git \
  --profile acme-backend \
  --priority 500 \
  --accept-recommended \
  --sync-interval 1h
```

The URL may be any git URL or the GitHub shorthand `owner/repo` — both equally
supported:

```sh
cfgd source add acme/dev-config                              # GitHub shorthand
cfgd source add https://github.com/acme/dev-config.git
cfgd source add https://gitlab.example.com/acme/dev-config.git
```

A source origin must be a remote. A local path — absolute, relative or
`file://` — is refused, because a source delivers files, packages and scripts
to this machine and its origin has to be something a subscriber can fetch, pin
and verify rather than a directory anything on the host can rewrite. An
existing local path is never silently expanded into a GitHub URL either: it is
reported as the local path it is. To try a source out before publishing it, see
[testing a source locally](sources.md#testing-a-source-locally).

### `cfgd source list`

List subscribed sources.

### `cfgd source show <name>`

Show source details, provided profiles, policy breakdown, conflicts, and the
modules the source delivers (its manifest `provides.modules` allow-list). The
delivered modules appear under a `Modules` section in human output and as a
`modules` array in the structured (`-o json`/`-o yaml`) payload.

### `cfgd source remove <name>`

Remove a subscription. The source's cached clone (under
`<state-dir>/sources/<name>`) is deleted as part of removal, so a later
re-subscription clones fresh rather than reusing stale contents.

```sh
cfgd source remove acme-corp --keep-all          # keep resources as local
cfgd source remove acme-corp --remove-all        # remove everything
cfgd source remove acme-corp --ignore-not-found  # exit 0 if acme-corp isn't subscribed
```

`--ignore-not-found` exits `0` with a no-op message instead of the strict
not-found error (exit `6`) when no source by that name is subscribed.

### `cfgd source update [name]`

Fetch latest from sources (all or specific). Exits non-zero
(`1`, `ExitCode::Error`) if any source fails to update, so CI can detect a
failed refresh from `$?` alone; the per-source failure is also printed.

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
`cfgd source add` — any git URL, or the GitHub shorthand `owner/repo`.

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

### `cfgd decide <action> [resource]`

Accept or reject pending source decisions.

```sh
cfgd decide accept packages.brew.k9s       # accept one item
cfgd decide reject packages.brew.stern     # reject one item
cfgd decide accept --source acme-corp      # accept all from source
cfgd decide accept --all                   # accept everything
```

Bare `cfgd decide` lists the decisions still awaiting you. Only rows whose source is
still in `spec.sources` are listed: a decision outliving its subscription can no longer
withhold anything, so there is nothing left to accept or reject. `cfgd status` reports
the same filtered set.

Answering an item **records only that item**: an item `cfgd plan` classified but nothing
has recorded yet is minted and resolved in the same step, and no source hash is stamped
— so the daemon's notification for the source's *other* new items is preserved. If the
classification needed to see unrecorded items cannot be built (an unreadable config or
composition), a resolving `cfgd decide` refuses with the reason instead of reporting the
decision as not found; already-recorded rows still resolve, and the bare listing shows
them with a warning that the unrecorded ones could not be read. The refusal only applies
where unrecorded items could exist: with no config file, or a config with no
`spec.sources`, decide answers from the store alone and never runs the classification.

## Backup Commands

### `cfgd backup`

Run, inspect, or restore the declarative backups a profile declares in `spec.backups[]`.

```sh
cfgd backup run                                       # run every backup declared in the active profile
cfgd backup run notes-db                              # run just the named backup
cfgd backup list                                      # inventory + last-run status + next scheduled run; alias: ls
cfgd backup list notes-db                             # just that unit's row
cfgd backup list notes-db --snapshots                 # its snapshots: name, created, size
cfgd backup restore notes-db                          # newest snapshot, back over the source
cfgd backup restore notes-db --at 20260730T120000Z    # pick an older one
cfgd backup restore notes-db --to /tmp/inspect --yes  # somewhere else, no prompt
cfgd --output json backup list
```

An unknown name given to `cfgd backup run`, `backup list`, or `backup restore` is exit code `6`
(see [Exit Codes](#exit-codes)) and lists every valid name; an unknown `--at` snapshot is exit `6`
too and lists every available snapshot. A run that recorded a failure — a bad copy, or
`postBackup` erroring after a good one — also exits nonzero.

`backup restore` overlays the snapshot onto the target (names only in the target are left alone;
a target entry whose kind differs from the snapshot's — a symlink, or a directory where the
snapshot holds a file — is removed and replaced, never written through), takes a safety snapshot of the current contents first, and requires confirmation
unless `--yes` (`CFGD_YES`) is given. `--to <path>` redirects the restore; a path outside the
backup's source also skips the safety snapshot, while a path at or inside the source still takes
one. The unit's `preBackup` / `postBackup` hooks wrap the whole restore exactly once and see
`CFGD_OPERATION=restore`. Where cfgd cannot prompt — piped stdin, CI, or `-o json` — a restore
without `--yes` is an error rather than a silent no-op. See [Restoring](backups.md#restoring).

A unit that is already running elsewhere (the daemon's timer, another `cfgd apply`) is refused
rather than interleaved: `backup run` reports the holding process as a skip and exits `1`, while the
other units it was asked to run still run. See
[One run at a time](backups.md#run-semantics).

Structured output (`-o json`) payload for `backup run`: an array of
`{ name, status, clean, destinationPath?, error? }`, where `status` is `success`, `failed`, or
`skipped` (the unit was already running). A refused unit does not add a second document to stdout —
the payload is always one JSON value and the nonzero exit code carries the failure. For
`backup list`: an array of
`{ name, source, schedule?, retention, lastRunStatus?, lastRunAt?, lastRunClean?, nextRunAt? }`.
For `backup list <name> --snapshots`: an array of `{ name, created, sizeBytes }`, newest first,
where `name` is the snapshot's path relative to the backup's `destination`. For `backup restore`:
a single `{ name, snapshot, restoredTo, restored, clean, sizeBytes, safetySnapshot?, error? }` —
or, when the operator declines at the confirmation prompt,
`{ name, snapshot, restoredTo, restored: false, declined: true }`. The declined payload omits
`clean` deliberately: a decline exits `0`, and reporting `clean: false` beside a zero exit would
contradict whichever of the two a consumer trusted.
`nextRunAt` is the ISO 8601 UTC time the daemon's timer will next fire the unit, computed from the
same `schedule` + last `finished_at` seeding the daemon uses; it is omitted for a schedule-less
unit (the `Next Run` column renders `-`). See [Declarative Backups](backups.md#cli).

`backup run` always runs the units it names, schedule or not. A backup that declares a `schedule`
additionally runs on the [daemon's timer](backups.md#daemon-scheduling), and a schedule-less one
runs during `cfgd apply`.

## Compliance Commands

### `cfgd compliance`

Collect a compliance snapshot of the machine — every managed file, package, and system setting
scored against the effective desired state — and inspect the stored history.

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
machine's content hash changed, so **history records changes, not ticks** — see
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

Structured output (`-o json`) payload: `{ artifact, digest, platform, signed, attested }`.

See [image-pack.md](image-pack.md) for the full reference, worked example, and Pod spec.

## Other Commands

### `cfgd config show`

Show the current cfgd.yaml configuration.

### `cfgd config edit`

Open cfgd.yaml in `$EDITOR`.

### `cfgd config get <key>`

Get a config value by dotted key path. Outputs raw value to stdout (suitable for scripting).

```sh
cfgd config get profile                      # → work
cfgd config get theme                        # → dracula
cfgd config get theme.name                   # → dracula
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

Remove a config value (resets to default).

```sh
cfgd config unset theme                          # remove entire theme section
cfgd config unset daemon.reconcile.autoApply    # reset single field
cfgd config unset aliases.deploy                 # remove an alias
```

### `cfgd workflow generate`

Generate GitHub Actions workflows for config repo releases.

```sh
cfgd workflow generate --force   # overwrite existing
```

Profiles whose YAML fails to parse are skipped with a warning naming the file and the parse error; the remaining valid profiles still generate.

Tags are immutable. A changed module is tagged `<name>/v<version>` from its [`metadata.version`](spec/module.md#metadataversion) — read through `cfgd module show`, never guessed — and the job fails if the module declares no version or if that tag already exists (bump `metadata.version`). A changed profile is tagged `profile/<name>/<UTC timestamp>` in `%Y%m%dT%H%M%SZ` form, so a second release on the same day gets its own tag. Nothing is force-pushed. The job installs the same cfgd version that generated the workflow, pinned in the job's `CFGD_VERSION` environment variable; re-run `cfgd workflow generate --force` after upgrading cfgd to move the pin.

The generated workflow's change detection covers both profile manifest forms — the flat file (`profiles/<name>.yaml`) and the bundle directory (`profiles/<name>/**`) — so a push touching either layout tags a release. Names containing regex metacharacters (e.g. `web.app`) are matched literally, and matching is exact — a change to a sibling profile whose name extends another (`profiles/work.app.yaml`) does not flag `work`. Generation fails if two names would fold to the same job-output key (`web.app` and `web-app` both fold to `profile_web_app`); rename one so they stay distinct.

### `cfgd checkin`

Check in with the device gateway.

```sh
cfgd checkin --server-url https://cfgd.acme.com --api-key <key>
```

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

Manage user-level CLI aliases stored in `cfgd.yaml` under `spec.aliases` — shorthands for cfgd
invocations you type often.

```sh
cfgd alias set pu "profile update --file"   # add or update; alias: add
cfgd alias show pu                          # print the command a single alias expands to
cfgd alias list                             # alias: ls
cfgd alias delete pu                        # alias: rm
```

No command-specific flags. `set` takes `<NAME> <COMMAND>`, where `COMMAND` is the argument string
the alias expands to. Aliases live in the config file, so they travel with the config repository.

### `cfgd state forget-prefix <manager>`

Forget the global-install prefix cfgd persisted for a package manager, so the next
install/uninstall/list derives it fresh.

```sh
cfgd state forget-prefix npm
cfgd state forget-prefix pipx
```

No command-specific flags. cfgd already revalidates a persisted prefix that became unwritable —
this is for the opposite case, where a *better* prefix became available (permissions fixed after
cfgd fell back to a user-local directory).

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

Scripted consumers rely on distinct exit codes to decide follow-up actions without parsing stderr. The taxonomy is stable — breaking changes bump the CLI major version.

| Code | Meaning | Emitted by |
|---|---|---|
| `0` | Operation succeeded. | All commands on success. |
| `1` | Generic failure (network, IO, unclassified internal error). Also a `cfgd backup run` that recorded a failed or unclean snapshot (see [Run Semantics](backups.md#run-semantics)), a `cfgd backup restore` whose overlay or hooks failed, and `cfgd diff --exit-code` when a system configurator's own check failed — drift is undetermined rather than absent, which outranks `5`. | Any command whose `Result` resolves to a non-config error, and `cfgd diff --exit-code` on a failed configurator check. |
| `2` | An upgrade is available but not installed. | `cfgd upgrade --check` only. |
| `3` | No cfgd config file at the resolved path. | Any command when `--config` points to a missing file. |
| `4` | Config file exists but failed parse or validation. | Any command when `--config` is malformed or schema-invalid. |
| `5` | Drift detected between actual and desired state. | `cfgd diff --exit-code`, `cfgd status --exit-code`, `cfgd verify --exit-code`. |
| `6` | A named resource was not found. | Any command naming a missing resource — e.g. `cfgd module show/delete/edit/export <missing>`, `cfgd profile show/switch/delete/edit/update <missing>`, `cfgd source show/update/remove/priority/override <missing>`, `cfgd module registry remove/rename <missing>`, `cfgd backup run/list/restore <missing>`, `cfgd backup restore --at <missing-snapshot>`, `cfgd init --apply-profile <missing>`. The destructive verbs `module delete`, `module registry remove`, `source remove`, and `profile delete` accept `--ignore-not-found` to exit `0` instead when the target is absent. |
| `7` | An apply ran but at least one action failed (partial or total). Also a schedule-less `spec.backups[]` unit that failed or didn't complete cleanly during `cfgd apply` (see [Apply Integration](backups.md#cli)) — the unit is reported, apply continues, and the overall status downgrades to `partial`. | `cfgd apply`, `cfgd init --apply/--apply-profile/--apply-module`, and `cfgd module add --apply` when one or more actions fail. |
| `130` | `apply` was cooperatively aborted by `SIGINT` (Ctrl-C). | `cfgd apply` interrupted with Ctrl-C; the in-flight action finishes, the lock releases, the run is recorded as `Aborted`. |
| `143` | `apply` was cooperatively aborted by `SIGTERM`. | `cfgd apply` interrupted with `kill`; same cooperative-abort semantics as `130`. |

Codes `130` / `143` follow the POSIX `128 + signal` convention and are not cfgd-specific. See [Graceful Interruption](safety.md#graceful-interruption-sigint--sigterm) for the abort semantics. The `--exit-code` / `-e` flag on `diff`, `status`, and `verify` follows the `git diff --exit-code` convention: without the flag these commands always exit `0`; with the flag they exit `5` whenever drift is present — except `cfgd diff --exit-code`, which exits `1` instead when a system configurator check itself failed, since an unknown state outranks a known one for a script deciding whether to apply.

External-process passthrough (e.g. `kubectl exec` invoked by the `kubectl cfgd` plugin) forwards the inner tool's exit code unchanged — those codes are not part of the cfgd taxonomy.

### Error output

Every failure renders exactly once, to `stderr` in human mode and to `stdout` in structured mode:

- **Human (default):** a single `✗` line carrying the error message, followed by any
  remediation hints (e.g. `Available modules: …`, or `run \`cfgd init\``). The same failure is
  never printed twice.
- **Structured (`-o json` / `yaml` / `jsonpath` / `template`):** exactly one error object,
  always — even for an unclassified internal error, so a scripted consumer is never left with
  empty output on failure. The shape is stable:

  ```json
  { "error": "not_found", "name": "web-server", "available": ["base", "dev"] }
  ```

  `error` is a machine-readable kind (`not_found`, `registry_not_found`, `already_exists`,
  `parse_failed`, `key_not_found`, `target_not_writable`, …), `name` identifies the subject
  (module / source / profile / registry / key), and any
  command-specific fields follow. An error that carries no typed metadata falls back to
  `{ "error": "error", "name": "", "message": "<text>" }`. Remediation hints are human-only and
  never appear in the structured payload.

### Use in CI

```sh
# Fail the build if the machine has drifted from the committed profile.
cfgd verify --exit-code

# Run upgrade on a schedule but only page humans on real failures.
if ! cfgd upgrade --check; then
  case $? in
    2) echo "Update available — cfgd upgrade to install" ;;
    *) echo "Upgrade check failed" >&2; exit 1 ;;
  esac
fi
```
