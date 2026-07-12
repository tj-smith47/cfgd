# CLI Reference

Complete command reference for `cfgd`. All commands respect [global flags](configuration.md#global-flags).

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

### `cfgd init`

Initialize a new cfgd configuration repository.

```sh
cfgd init                                          # interactive setup in current directory
cfgd init ~/dotfiles                               # scaffold in specific directory
cfgd init --from git@github.com:you/config.git     # clone and scaffold
cfgd init --from ~/existing/config                 # use local config directory
cfgd init --from <source> --branch dev                # specify branch
cfgd init --from <source> --apply-profile work-mac    # clone, activate profile, apply
cfgd init --from <source> --apply-module nvim         # clone, apply just one module
cfgd init --from <source> --apply --yes --install-daemon  # full one-liner bootstrap
```

| Flag | Description |
|---|---|
| `[path]` | Target directory (default: current directory) |
| `--from <url\|path>` | Config source: git URL to clone, or local path to existing config |
| `--branch <name>` | Git branch (default: master) |
| `--name <name>` | Config name in metadata (default: directory name) |
| `--apply` | Apply configuration after scaffolding |
| `--apply-profile <name>` | Activate and apply a specific profile (implies --apply, exits `6` if not found) |
| `--apply-module <name>` | Apply a specific module (repeatable, implies --apply, errors if not found) |
| `--yes`, `-y` | Skip confirmation prompts (used with --apply) |
| `--install-daemon` | Install daemon service after init |
| `--theme <name>` | Theme name (default, dracula, solarized-dark, solarized-light, minimal) |

See [bootstrap.md](bootstrap.md) for the full init flow.

### `cfgd apply`

Apply the configuration plan.

```sh
cfgd apply                          # apply with confirmation
cfgd apply --dry-run                # preview without applying
cfgd apply --yes                    # skip confirmation
cfgd apply --phase packages         # single phase
cfgd apply --module nvim            # single module + deps (no profile required)
cfgd apply --only packages.brew     # dot-notation filter
cfgd apply --skip system.sysctl     # skip specific items
cfgd apply --skip-scripts           # apply without running any hooks
```

| Flag | Description |
|---|---|
| `--dry-run` | Preview changes without applying (supports `-o json`) |
| `--phase <name>` | Apply only a specific phase |
| `--yes`, `-y` | Skip confirmation prompt |
| `--module <name>` | Apply only this module and its dependencies |
| `--skip <path>` | Skip items by dot-notation path (repeatable) |
| `--only <path>` | Apply only items matching dot-notation paths (repeatable) |
| `--skip-scripts` | Skip all script hooks (pre/post/onChange) |

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

```sh
$ cfgd plan
Phase: Modules
  - [dev-tools] brew install ripgrep, fd <- team   # delivered by source 'team'
  - [localmod] brew install jq                     # consumer-local, no tag
```

```jsonc
// cfgd plan -o json  →  phases[].actions[]
{ "type": "install", "description": "[dev-tools] brew install ripgrep, fd <- team", "origin": "team" }
{ "type": "install", "description": "[localmod] brew install jq" }   // no "origin" key
```

### `cfgd status`

Show configuration status, drift, and pending decisions.

```sh
cfgd status                                 # human-readable table
cfgd status -o json                         # full status as JSON
cfgd status -o jsonpath='{.drift}'          # extract drift events
cfgd status --module nvim                   # status for a single module (no profile required)
```

### `cfgd diff`

Show detailed file diffs with syntax highlighting.

### `cfgd verify`

Check that all managed resources match desired state.

```sh
cfgd verify -o json          # structured pass/fail results
cfgd verify --module nvim    # verify only a single module's resources (no profile required)
```

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
cfgd module registry add https://github.com/cfgd-community/modules.git
cfgd module registry add https://github.com/myorg/modules.git --name myorg
cfgd module registry list
cfgd module registry remove community
cfgd module registry remove community --ignore-not-found  # exit 0 if absent
cfgd module registry rename community cfgd-community
```

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

Replace one source with another.

### `cfgd source create`

Create a new `cfgd-source.yaml` in the current directory.

### `cfgd source edit`

Open `cfgd-source.yaml` in `$EDITOR`.

## Secret Commands

```sh
cfgd secret init                    # generate age key + .sops.yaml
cfgd secret encrypt <file>          # encrypt values in place
cfgd secret decrypt <file>          # decrypt to stdout
cfgd secret edit <file>             # decrypt, edit, re-encrypt
```

## Daemon Commands

```sh
cfgd daemon                # run in foreground (default)
cfgd daemon run            # run in foreground (explicit)
cfgd daemon install        # install as system service
cfgd daemon status         # check running state
cfgd daemon uninstall      # stop the daemon and remove the service
```

## Decision Commands

### `cfgd decide <action> [resource]`

Accept or reject pending source decisions.

```sh
cfgd decide accept packages.brew.k9s       # accept one item
cfgd decide reject packages.brew.stern     # reject one item
cfgd decide accept --source acme-corp      # accept all from source
cfgd decide accept --all                   # accept everything
```

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
| `1` | Generic failure (network, IO, unclassified internal error). | Any command whose `Result` resolves to a non-config error. |
| `2` | An upgrade is available but not installed. | `cfgd upgrade --check` only. |
| `3` | No cfgd config file at the resolved path. | Any command when `--config` points to a missing file. |
| `4` | Config file exists but failed parse or validation. | Any command when `--config` is malformed or schema-invalid. |
| `5` | Drift detected between actual and desired state. | `cfgd diff --exit-code`, `cfgd status --exit-code`, `cfgd verify --exit-code`. |
| `6` | A named resource was not found. | Any command naming a missing resource — e.g. `cfgd module show/delete/edit/export <missing>`, `cfgd profile show/switch/delete/edit/update <missing>`, `cfgd source show/update/remove/priority/override <missing>`, `cfgd module registry remove/rename <missing>`, `cfgd init --apply-profile <missing>`. The destructive verbs `module delete`, `module registry remove`, `source remove`, and `profile delete` accept `--ignore-not-found` to exit `0` instead when the target is absent. |
| `7` | `apply` ran but at least one action failed (partial or total). | `cfgd apply` when one or more actions fail. |
| `130` | `apply` was cooperatively aborted by `SIGINT` (Ctrl-C). | `cfgd apply` interrupted with Ctrl-C; the in-flight action finishes, the lock releases, the run is recorded as `Aborted`. |
| `143` | `apply` was cooperatively aborted by `SIGTERM`. | `cfgd apply` interrupted with `kill`; same cooperative-abort semantics as `130`. |

Codes `130` / `143` follow the POSIX `128 + signal` convention and are not cfgd-specific. See [Graceful Interruption](safety.md#graceful-interruption-sigint--sigterm) for the abort semantics. The `--exit-code` / `-e` flag on `diff`, `status`, and `verify` follows the `git diff --exit-code` convention: without the flag these commands always exit `0`; with the flag they exit `5` whenever drift is present.

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
