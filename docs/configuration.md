# Configuration

cfgd config files follow a structure inspired by the [Kubernetes Resource Model](https://github.com/kubernetes/design-proposals-archive/blob/main/architecture/resource-management.md): every document has `apiVersion`, `kind`, `metadata`, and `spec` fields. This gives a consistent shape across configs, profiles, modules, and sources. TOML is also supported (use `.toml` extension).

The only supported `apiVersion` is `cfgd.io/v1alpha1`. Any other value (e.g. a future `cfgd.io/v1alpha2`) is rejected at parse time with an error naming the supported version, rather than being silently loaded under the current schema.

For the complete field-by-field reference, see the [Config spec reference](spec/config.md).

## Editor Support

cfgd publishes JSON Schemas for each config document — `cfgd.yaml`, modules
(`modules/<name>/module.yaml`), profiles (`profiles/<name>/profile.yaml`), and
config sources (`cfgd-source.yaml`) — so editors with a YAML language server
(VS Code, Neovim, JetBrains, …) can offer completion and inline validation.

The schemas are self-hosted at `https://cfgd.io/schemas/` and registered with
[SchemaStore](https://www.schemastore.org/) on each release, so for the standard
file names above no setup is needed once your editor's YAML extension picks up
the SchemaStore catalog. To pin a schema explicitly (or for non-standard file
names), add a modeline to the top of the file:

```yaml
# yaml-language-server: $schema=https://cfgd.io/schemas/cfgd-config.schema.json
apiVersion: cfgd.io/v1alpha1
kind: Config
# ...
```

Swap the URL for `cfgd-module`, `cfgd-profile`, or `cfgd-source` as appropriate.

cfgd's scaffolders (`cfgd init`, `cfgd profile create`, `cfgd module create`, and
AI generate) emit this modeline as the first line of every manifest they write, so
generated files validate immediately even where the SchemaStore catalog does not
match — including legacy flat profiles (`profiles/<name>.yaml`), files reached
through a dot-directory, and hand-renamed manifests. The SchemaStore catalog
associates the canonical bundle path `profiles/<name>/profile.yaml`; the modeline
covers everything else.

CLI commands that rewrite a manifest in place (`cfgd config set`, `cfgd module
update`, `cfgd profile switch`, `cfgd profile update`, source mutations, …)
preserve the file's **leading** comment block — the modeline and any banner
comments above the first YAML key survive the rewrite. Comments elsewhere in
the document are not preserved.

## Root Config — `cfgd.yaml`

The entry point. Tells cfgd which profile to activate, where config is stored, and how the daemon behaves.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work

  origin:
    - type: Git
      url: git@github.com:me/machine-config.git
      branch: master

  daemon:
    enabled: true
    reconcile:
      interval: 5m
      onChange: true
    sync:
      autoPull: true
      autoPush: false
      interval: 5m
    notify:
      drift: true
      method: Desktop
      webhookUrl: https://...

  secrets:
    backend: sops
    sops:
      ageKey: ~/.config/cfgd/age-key.txt
    integrations:
      - name: 1password
      - name: bitwarden
      - name: vault

  update:
    policy: Prompt         # cfgd binary self-update behavior (default: Prompt)
    interval: 24h          # check cadence when policy != Manual (default: 24h)
    channel: stable        # release channel (default: cfgd's built-in channel)
    skills:
      policy: Inherit      # follows spec.update.policy unless overridden (default: Inherit)

  sources:
    - name: acme-corp
      origin:
        type: Git
        url: git@github.com:acme-corp/dev-config.git
        branch: master
      subscription:
        profile: acme-backend
        priority: 500
        acceptRecommended: true
        requireSignedCommits: true   # demand a signed HEAD from this source
```

## Fields

| Field | Required | Default | Description |
|---|---|---|---|
| `spec.profile` | yes | — | Name of the profile YAML file to activate (without `.yaml`) |
| `spec.origin.type` | no | — | `Git` or `Server` |
| `spec.origin.url` | no | — | Repository URL |
| `spec.origin.branch` | no | `master` | Git branch |
| `spec.origin.sshStrictHostKeyChecking` | no | `AcceptNew` | SSH host key policy: `AcceptNew` (accept first-seen), `Yes` (require known_hosts), `No` (insecure) |
| `spec.daemon.reconcile.interval` | no | `5m` | Drift check interval (e.g. `1m`, `5m`, `1h`) |
| `spec.daemon.reconcile.onChange` | no | `false` | Reconcile immediately on file change |
| `spec.daemon.reconcile.patches` | no | `[]` | Per-module/profile reconcile overrides (see [daemon.md](daemon.md#reconcile-patches)) |
| `spec.daemon.sync.autoPull` | no | `false` | Auto-pull from remote |
| `spec.daemon.sync.autoPush` | no | `false` | Auto-commit and push local changes |
| `spec.daemon.notify.method` | no | `Desktop` | `Desktop`, `Stdout`, or `Webhook` |
| `spec.update.policy` | no | `Prompt` | cfgd binary self-update behavior: `Auto`, `Prompt`, `Notify`, or `Manual` (see [Update behavior](#update-behavior-specupdate)) |
| `spec.update.interval` | no | `24h` | Update-check cadence when `policy != Manual` (e.g. `30m`, `24h`, `7d`) |
| `spec.update.channel` | no | — | Release channel to track (e.g. `stable`, `prerelease`); unset uses cfgd's built-in default channel |
| `spec.update.skills.policy` | no | `Inherit` | Authored-skill refresh policy: `Inherit` (follow `spec.update.policy`), `Auto`, `Prompt`, `Notify`, or `Manual` |
| `spec.secrets.backend` | no | `sops` | `sops` or `age` (see [secrets.md](secrets.md) for when to use which) |
| `spec.theme` | no | `default` | Theme name (string) or object with `name` + `overrides` |
| `spec.fileStrategy` | no | `Symlink` | `Symlink`, `Copy`, `Template`, or `Hardlink` (Windows: `Symlink` requires Developer Mode or elevation) |
| `spec.aliases.<name>` | no | — | CLI command aliases (e.g. `add: "profile update --file"`) |
| `spec.compliance` | no | — | Continuous compliance snapshot settings. Reports the effective desired state (profile + modules), and file checks are content-aware (see [spec/config.md](spec/config.md#speccompliance)) |
| `spec.sources[].subscription.requireSignedCommits` | no | `false` | Demand a valid GPG or SSH signature on that source's HEAD commit. ORed with the source manifest's `constraints.requireSignedCommits`, so it only adds strictness (see [sources.md](sources.md#security-model)) |

All fields can be read and written programmatically via `cfgd config get <key>` and `cfgd config set <key> <value>`. See the [CLI reference](cli-reference.md) for details.

Enum-valued fields (e.g. `spec.fileStrategy`, `spec.daemon.driftPolicy`, `spec.daemon.notify.method`, the profile-level `spec.envScope`, `spec.compliance.export.format`) are parsed case-insensitively — `Symlink`, `symlink`, and `SYMLINK` are all accepted. The documented PascalCase form is canonical and is what cfgd writes back.

## Update behavior (`spec.update`)

cfgd can check for its own updates (it doesn't by default — `cfgd upgrade` is
otherwise purely manual), and separately decide whether installed [authoring
skills](skill.md) are re-rendered when cfgd moves. Both are governed by
`spec.update`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  profile: work
  update:
    policy: Prompt         # cfgd binary self-update behavior (default: Prompt)
    interval: 24h          # check cadence when policy != Manual (default: 24h)
    channel: stable        # release channel (default: cfgd's built-in channel)
    skills:
      policy: Inherit      # follows spec.update.policy unless overridden (default: Inherit)
```

`spec.update.policy` is the one posture knob; by default it governs both the
binary and skill refresh. Override `spec.update.skills.policy` only to decouple
skill refresh from the binary. "update" is the umbrella verb for keeping things
current; "upgrade" is the specific binary-replacement action (`cfgd upgrade`),
which `policy: Auto`/`Prompt` drives.

### Update policies

The binary policy (`spec.update.policy`) is an `UpdatePolicy`:

| Policy | Meaning |
|---|---|
| `Auto` | on a detected newer version, apply it unattended |
| `Prompt` | check, then ask before applying (interactive CLI); non-interactive falls back to `Notify` |
| `Notify` | check and surface/record availability; never apply, never prompt |
| `Manual` | cfgd does nothing automatically — no check, no notice; you drive it |

The skill policy (`spec.update.skills.policy`) is a `SkillUpdatePolicy` — the
same four values **plus** `Inherit`, which is its default:

| Skill policy | Meaning |
|---|---|
| `Inherit` *(default)* | use the binary `spec.update.policy` value |
| `Auto` / `Prompt` / `Notify` / `Manual` | as above, but for skill refresh only |

### Suppressing the automatic check

Three environment variables silence the *automatic* update check (CLI startup
and the daemon sync loop) regardless of `spec.update.policy`, checked in this
order:

| Variable | Convention |
|---|---|
| `CFGD_NO_UPDATE_CHECK` | cfgd's own, most specific |
| `NO_UPDATE_NOTIFIER` | shared with npm's `update-notifier` |
| `DO_NOT_TRACK` | [consoledonottrack.com](https://consoledonottrack.com) |

A variable counts as "set" when it is present and, after lowercasing and
trimming, is not one of `""`, `"0"`, or `"false"` — so `DO_NOT_TRACK=0` means
"do track", not "opt out". The same rule applies to all three; there is no
per-variable special case.

```sh
DO_NOT_TRACK=1 cfgd status   # no "Update available" line, ever
DO_NOT_TRACK=0 cfgd status   # NOT an opt-out — checks run as normal
```

**Explicit `cfgd upgrade` (and `cfgd upgrade --check`) is never suppressed** —
the opt-out silences the automatic check only; a user who asks for an update
check always gets one. `cfgd doctor` reports which variable, if any, is
currently suppressing the automatic check.

### At most one update surface, ever

Skill staleness is a *consequence* of a binary version change (a skill is stale
only when the running cfgd is newer than its stamp), so the two surfaces are
naturally serialized — binary first, skills after. Three rules dedup the only
collision (skills left stale from a past skipped refresh *and* a newer binary
now available), so you'll never see two update prompts:

1. **Binary outranks skills.** While a binary update is pending/available, the
   skill surface is suppressed — only the binary surface shows. (Refreshing
   skills against a binary you're about to replace is wasted work.)
2. **Ride-along.** When a binary upgrade actually happens (`Auto`, an accepted
   `Prompt`, or a manual `cfgd upgrade`), the user-scope skill refresh is part of
   **that same action and output block** — never a second prompt.
3. **One consolidated skill surface.** When skills are surfaced standalone
   (binary current, skills stale), a single notice covers both user- and
   project-scope staleness — never one notice per scope.

### Scope governs auto vs manual (the git-safety invariant)

> **cfgd never auto-rewrites tracked project files.** Ride-along and
> `Auto`/`Inherit→Auto` refresh touch **user-scope (home) skills only**.
> **Project-scope skills are always manual** — regardless of policy — because
> they are committed, and a surprise diff is unacceptable. The consolidated
> surface (rule 3) tells you project skills are stale so you can run
> `cfgd skill update` and commit deliberately.

| Effective skills policy | User-scope on version change | Project-scope |
|---|---|---|
| `Auto` (incl. `Inherit→Auto`) | re-render (ride-along if same action) | notice only, never written |
| `Prompt` / `Inherit→Prompt` | refresh rides along with the accepted binary upgrade; no separate prompt | notice only |
| `Notify` / `Inherit→Notify` | a single stale notice (rule 3); no write | notice only |
| `Manual` / `Inherit→Manual` | nothing (silent); you run `cfgd skill update` | nothing |

In daemon context, `Notify` records a structured event rather than prompting.

## Repository Layout

```
my-config/
├── cfgd.yaml              # root config
├── profiles/              # each profile is a bundle: <name>/profile.yaml + payload
│   ├── base/
│   │   └── profile.yaml   # base profile — shared across machines
│   ├── work/
│   │   ├── profile.yaml   # inherits base, adds work config
│   │   └── files/         # profile-owned file payload (created by --file)
│   └── personal/
│       └── profile.yaml
├── modules/               # reusable config modules
│   ├── nvim/
│   │   ├── module.yaml
│   │   └── files/
│   └── tmux/
│       ├── module.yaml
│       └── files/
├── files/                 # source files for profiles
│   ├── shell/
│   │   ├── .zshrc
│   │   └── .zshrc.tera
│   ├── git/
│   │   └── .gitconfig
│   └── ssh/
│       └── config
├── secrets/               # SOPS-encrypted files
│   └── api-keys.yaml
└── scripts/               # lifecycle hook scripts
    ├── pre-setup.sh
    └── post-setup.sh
```

Each `modules/<name>/module.yaml` may declare its own release version under
`metadata.version` (strict semver, optional) — the value `cfgd workflow generate`'s
release job tags and `cfgd module show <name> -o jsonpath='{.metadata.version}'` reports:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
  version: 1.4.0

spec:
  packages:
    - name: neovim
```

`1.4.0`, `2.0.0-rc.1`, and `1.0.0+build.5` are accepted; `0.10`, `v1.2.3`, and `latest`
are rejected at parse time. See the [Module spec reference](spec/module.md#metadataversion).

Each profile is a self-contained bundle: a fixed-name `profiles/<name>/profile.yaml`
manifest alongside its own `files/` payload directory (mirroring the
`modules/<name>/module.yaml` shape). The legacy flat form `profiles/<name>.yaml`
remains fully supported — both forms load, and existing flat profiles keep working
untouched. Run `cfgd profile migrate <name>` (or `--all`) to move a flat profile
into the canonical bundle form. Having both `profiles/work/profile.yaml` and
`profiles/work.yaml` on disk is a hard error (ambiguous); migrate or delete one.

Profile files support five deployment strategies:

- **Symlink** (default) — creates a symbolic link from target to source. Changes to the source are immediately reflected.
- **Copy** — copies the source file to the target path. The target is independent of the source after apply.
- **Template** — renders the file through [Tera](templates.md) before copying. Auto-detected for `.tera` extension.
- **Hardlink** — creates a hard link. Both paths share the same inode.
- **Patch** — merges structured keys/values into an existing target, or pipes it through a script, leaving everything else untouched. Requires a `patch:` block; `source` is not required.

```yaml
files:
  managed:
    - source: shell/.zshrc
      target: ~/.zshrc
      # strategy defaults to Symlink
    - source: git/.gitconfig
      target: ~/.gitconfig
      strategy: Copy
    - source: shell/.zshrc.tera   # .tera triggers template rendering
      target: ~/.zshrc
    - target: ~/.gitconfig
      strategy: Patch
      patch:
        format: Ini             # Ini | Json | Yaml | Toml; inferred from the target's extension when omitted
        ensure:                 # deep-merged into the target; mutually exclusive with `script`
          user:
            name: "Example User"
    - target: /etc/hosts
      strategy: Patch
      patch:
        script: scripts/ensure-hosts-entry.sh   # receives current content on stdin, writes new content to stdout
```

Files can be marked `private: true` to exclude them from git (added to `.gitignore`).

### Choosing a file strategy

`Symlink` and `Copy` answer one question differently: who owns the bytes at the
target after apply.

| | `Symlink` (default) | `Copy` |
|---|---|---|
| Content edits in the repo | Live immediately (no apply needed) | Reach the target on the next apply or reconcile |
| `cfgd apply` after a content edit | Nothing to do (the link is intact; content already flowed) | Shows the file write |
| Hand edits at the target | Land in your repo (the target is the repo file) | Detected as drift and repaired back to the repo content |
| App rewrites its own config | Rewrites your repo file through the link | Rewrite is drift; reconcile restores it |
| Works across filesystems | Yes | Yes |
| Windows | Requires Developer Mode or elevation | Works everywhere |

Pick `Symlink` when the repo should stay the single source of truth and you want
edits (yours or the app's) captured there instantly: editor configs you iterate
on, dotfiles you tweak in place. Pick `Copy` when the target must not follow the
repo between applies, or when you want cfgd to police the target's content:
files an app rewrites at runtime, machines where a broken checkout must not
break the deployed config, Windows hosts without Developer Mode.

Under `Symlink`, editing the target through the link is not drift. The source
file in your repo owns the bytes, so the edit is already in the repo and there is
nothing for an apply to repair. cfgd refreshes what it recorded on the next apply,
without reporting an action. A `files.managed` file entry refreshes its own record.
A module-declared file refreshes the module's, which cfgd keeps as one record for
all of that module's files.

The remaining strategies are variants of `Copy`: `Template` renders through Tera
first (per-machine values baked in, so the output cannot be a link), `Hardlink`
shares the inode (same filesystem only; instant like a symlink, but severed
silently when any tool saves by rename, which most editors do), and `Patch` is for files cfgd shares
with other writers rather than owns.

The daemon watches both shapes: a change to a file target or anywhere under a
directory target triggers an immediate reconcile check, and interval ticks catch
the rest.

> **One writer per rc file.** `spec.env` maintains its own loader line inside
> shell rc files: `~/.bashrc` / `~/.zshrc` get
> `[ -f ~/.cfgd.env ] && . ~/.cfgd.env` injected so declared vars and
> bootstrapped `PATH` entries reach new shells. A `files.managed` entry whose
> `target` is the same rc file puts a second writer on that path:
>
> ```yaml
> env:
>   - name: EDITOR
>     value: nvim          # injects its loader line into ~/.zshrc
> files:
>   managed:
>     - source: shell/.zshrc
>       target: ~/.zshrc   # re-deploys ~/.zshrc from the repo copy
> ```
>
> Each writer undoes the other across runs (the file entry deploys an rc
> without the injected line, the env writer adds it back), so reconcile keeps
> finding drift on a target that never converges. Keep the rc file under one
> writer: put the loader line in the rc source you deploy, or leave the rc
> file out of `files.managed` and let `spec.env` own it.

### Partial-file edits (`strategy: Patch`)

`Patch` is the strategy for files cfgd must *share* rather than own — a
distro-shipped config, a file another tool also writes, a target that already has
hand-written content worth keeping. cfgd owns only the keys the spec names;
everything else in the target survives byte-for-byte where the format allows it.

A missing target is treated as empty content: `ensure` writes a minimal document,
`script` receives empty stdin.

`Patch` has no source file, so the source-file options are rejected rather than
silently ignored: `encryption` and `private` are both validation errors on a
`Patch` entry, and `source` itself is optional (and unused).

#### What survives besides the unnamed keys

The target keeps its identity, not just its content — the other strategies
replace the path, `Patch` rewrites the file in place:

| Property of the target | After a `Patch` apply |
|---|---|
| Permission bits | unchanged — a `0644 /etc/hosts` stays `0644`. A new target created by `Patch` gets the default `0600` until you declare `permissions:` |
| Symlink | followed, not replaced. `~/.gitconfig -> ~/dotfiles/gitconfig` keeps the link and the merge lands in `~/dotfiles/gitconfig`, so a dotfiles repo stays the source of truth. A dangling link is written at the link path itself, matching how a missing target is treated as empty content |
| Content the spec does not name | byte-for-byte, where the format allows it |

Declaring `permissions:` on a `Patch` entry still applies — it is the way to
*change* the mode deliberately, on top of a merge that otherwise leaves it alone.

Following the link has one consequence worth stating: the bytes are written at
the link's *destination*. When the entry comes from a source constrained by
[`allowedTargetPaths`](sources.md#allowedtargetpaths), the allow-list is matched
against the declared `target`, so a target you have symlinked out of an allowed
directory receives the merge at the real path — outside the allow-list. Point
`target` at the real file if you need the constraint to bind where the bytes land.

#### `ensure` — structured merge

`ensure` is deep-merged into the target. Nested mappings merge recursively; a
scalar, list, or type change replaces the value at that key. Values are literal —
`Patch` never renders Tera templates, so `{{ … }}` lands in the file verbatim.
Re-applying the same `ensure` is a no-op.

```yaml
files:
  managed:
    - target: ~/.config/app/settings.json
      strategy: Patch
      patch:
        ensure:
          editor:
            tabSize: 4        # other editor.* keys are left alone
          telemetry: false
```

The format decides how much of the target's original text survives:

| Format | Engine | Comments | Key order | Notes |
|---|---|---|---|---|
| `Ini` | line-preserving editor | preserved | preserved | Two levels: section → key → value |
| `Toml` | `toml_edit` | preserved | preserved | Nested tables, arrays, inline tables |
| `Json` | `serde_json` | n/a (JSON has none) | preserved | Reindented as 2-space pretty JSON |
| `Yaml` | `serde_yaml` | **lost** | preserved | The document is reflowed |

A JSON target that repeats an object key keeps the last occurrence, matching how
`serde_json` and every browser parse it — a duplicate key is tolerated, not an
error, because the target belongs to the user, not to cfgd.

A trailing comment behaves differently in the two comment-preserving formats,
because their editors work at different levels. TOML keeps a comment attached to
the value it trails, so an updated key keeps a comment that may now be stale:

```toml
jobs = 4 # tuned for the build box     →     jobs = 8 # tuned for the build box
```

INI replaces everything after `=`, so a trailing comment is dropped along with
the old value (INI dialects disagree about whether `;`/`#` even starts a comment
there, so cfgd never tries to keep part of a value). Comments on their *own*
line are untouched in both.

> **YAML comment caveat.** The YAML engine parses the target and re-serializes
> it, so comments, blank lines, and anchors are lost — the data and its key
> order survive, nothing else. When a YAML target's comments matter, use
> `script` mode and edit the text with a comment-preserving tool (`yq`, `sed`,
> a Python script) instead.

`format` is inferred from the target's extension when omitted:

| Extension | Format |
|---|---|
| `.ini` | `Ini` |
| `.json` | `Json` |
| `.yaml`, `.yml` | `Yaml` |
| `.toml` | `Toml` |

Any other extension (including no extension at all, such as `/etc/hosts` or
`~/.gitconfig`) requires an explicit `format`, or cfgd fails with a typed error
rather than guessing:

```yaml
    - target: ~/.gitconfig
      strategy: Patch
      patch:
        format: Ini             # required: `.gitconfig` has no format-bearing extension
        ensure:
          user:
            email: ada@example.com
```

INI specifics, which follow from editing lines rather than reparsing the file:

- A mapping under `ensure` is a `[section]`; a scalar is a key in the file's
  global area, above the first section header.
- Values must be scalars — INI has no list or nested-mapping syntax, and cfgd
  errors rather than inventing one.
- An updated key keeps its original spacing around `=`; a new key adopts the
  neighbouring keys' style (`key = value` vs `key=value`). CRLF files stay CRLF.
- Anything after `=` is replaced, including a trailing `; comment` — INI dialects
  disagree on whether that starts a comment, so cfgd never keeps part of a value.
- A duplicated key is rewritten at every occurrence, and a repeated `[section]`
  header is edited in every block, so the ensured value wins regardless of which
  duplicate the consuming parser honours (`git config` and `systemd` take the
  last). A key that is missing everywhere is added to the last block.
- Section names, key names, and values must survive being written and read back:
  a name containing `=`, `[`, `]`, or a line break, a name padded with
  whitespace, and a multi-line value are all rejected with a typed error. INI
  has no escape syntax, so writing one would make the merge unable to find its
  own key again and re-append it on every reconcile.

#### `script` — pipe the file through a command

The target's current content goes in on stdin; whatever the script writes to
stdout becomes the new content. A non-zero exit aborts with the script's stderr
attached — nothing is written. This is the escape hatch for formats cfgd has no
engine for, and for edits that must preserve YAML comments.

```yaml
    - target: /etc/hosts
      strategy: Patch
      patch:
        script: scripts/ensure-hosts-entry.sh
```

```sh
#!/bin/sh
# scripts/ensure-hosts-entry.sh
# Read stdin ONCE into a variable: a second `cat` would see EOF, the guard
# would always fail, and the entry would be appended on every reconcile.
content=$(cat)
printf '%s\n' "$content"
printf '%s\n' "$content" | grep -q '10.0.0.5 build.internal' \
  || echo '10.0.0.5 build.internal'
```

Like a lifecycle `run:`, `script:` is a path relative to the module (or config)
directory when one resolves, and an inline command otherwise — so a one-liner
works without a script file:

```yaml
      patch:
        script: "yq -y '.server.port = 9090'"
```

Scripts run with the same `CFGD_*` environment lifecycle hooks receive
(`CFGD_PHASE=patch`, plus `CFGD_MODULE_NAME` / `CFGD_MODULE_DIR` and the
module's `env` for a module-owned file), in the user's home directory, under the
standard script timeout.

**The filter must be a pure stdin → stdout transform.** cfgd decides whether a
`Patch` file has converged by *running* it, so every read-only command executes
it too: `cfgd plan`, `cfgd diff`, `cfgd verify`, `cfgd status --scan`,
`cfgd apply --dry-run`, and a compliance snapshot. A filter that installs
packages, writes files, or takes a lock will do so on a command the user expects
to change nothing, and a slow one makes every one of those commands slow. Write
it to be idempotent for the same reason: cfgd runs it on every reconcile.

#### When a `Patch` file fails

A target that cannot be parsed for its declared format, and a `script` that exits
non-zero, both fail with a typed error and write nothing — the target is left
byte-for-byte as it was. The merge always runs before the existing target is
touched, so a failure never leaves a half-written file.

What the failure *does* depends on what the command is for:

| Command class | Commands | A failure means |
|---|---|---|
| Builds an action list | `cfgd plan`, `cfgd apply`, `cfgd apply --dry-run` | the command aborts with the error — the same shape as a missing or unreadable `source` on the other strategies. An action list that quietly dropped a file cfgd could not evaluate would misstate what apply is about to do |
| Reports state | `cfgd diff`, `cfgd verify`, `cfgd status --scan`, `cfgd compliance` | that one file is reported as drifted (a `Warning` row in a compliance snapshot) with the error as its detail, and every other file, package and system result is still reported. One broken filter never blinds the whole report |

Where the evaluation happens depends on who declares the file, because the merge
is computed from the target's *current* bytes:

| Declared in | Evaluated at |
|---|---|
| A profile's `files.managed` | plan time — the failure is visible before any action runs |
| A module's `spec.files` | deploy time — the module's file-deployment action fails; the rest of the plan follows the usual apply semantics |

### Snapshot backups (`spec.backups[]`)

Declarative file/directory snapshots, including the `schedule` grammar (interval
or cron) and the hook, retention and destination semantics, live in
[Declarative Backups](backups.md); the field table is in the
[Profile spec](spec/profile.md#specbackups).

## File locations

cfgd stores four kinds of data, each resolved independently. Every root can be
relocated explicitly (see [Overriding a directory root](#overriding-a-directory-root)
below), and `cfgd paths` prints the resolved values on any host.

| Data | Default location |
|---|---|
| **Config** (`cfgd.yaml`, `profiles/`, `files/`, `modules.lock`) | `$XDG_CONFIG_HOME/cfgd` if set, else the platform default below |
| **State** (`state.db`, history, drift, apply journal, `apply.lock`, compliance exports, device credential, backups) | platform-native state dir — Linux `$XDG_STATE_HOME/cfgd` or `~/.local/state/cfgd`, macOS `~/Library/Application Support/cfgd/state`, Windows `%LOCALAPPDATA%\cfgd\state` |
| **Cache** (source cache, module cache) | platform-native cache dir — Linux `$XDG_CACHE_HOME/cfgd` or `~/.cache/cfgd`, macOS `~/Library/Caches/cfgd`, Windows `%LOCALAPPDATA%\cfgd`. Sources live under `<cache>/sources`, modules under `<cache>/modules`. |
| **Runtime** (daemon socket, pid files) | Linux `$XDG_RUNTIME_DIR/cfgd` (else `~/.cache/cfgd/runtime`), macOS `~/Library/Application Support/cfgd/runtime`, Windows `%LOCALAPPDATA%\cfgd` |

The **config** platform default per OS (used only when `XDG_CONFIG_HOME` is
unset):

| Platform | Config default | Notes |
|---|---|---|
| Linux | `~/.config/cfgd` | the XDG config base |
| macOS | `~/Library/Application Support/cfgd` | the native macOS location — shares one root with state and runtime (see migration below) |
| Windows | `%APPDATA%\cfgd` | the roaming app-data base |

`XDG_CONFIG_HOME` is honored on **every** platform (including macOS and Windows)
when it is set to a non-empty, absolute path; an empty or relative value is
ignored per the XDG Base Directory spec. Setting `XDG_CONFIG_HOME` relocates the
config dir on any platform — and is the supported way to keep config under
`~/.config` on macOS.

### System scope

Pass `--scope system` (or `CFGD_SCOPE=system`) to switch all four roots to their
machine-wide FHS / `/Library` equivalents:

| Root | Linux system | macOS system |
|---|---|---|
| Config | `/etc/cfgd` | `/Library/Application Support/cfgd` |
| State | `/var/lib/cfgd` | `/Library/Application Support/cfgd/state` |
| Cache | `/var/cache/cfgd` | `/Library/Caches/cfgd` |
| Runtime | `/run/cfgd` | `/Library/Application Support/cfgd/runtime` |

Windows is always system-scope; `--scope system` is a no-op there.

```console
$ cfgd --scope system paths
cfgd directories (scope: system)

Config
  dir    /etc/cfgd
  source default

State
  dir    /var/lib/cfgd
  source default

Cache
  dir    /var/cache/cfgd
  source default

Runtime
  dir    /run/cfgd
  source default
```

### Overriding a directory root

Each root has a dedicated flag and environment variable. The resolution
precedence for every root is:

```text
--<role>-dir flag  >  CFGD_<ROLE>_DIR env  >  $*_DIRECTORY (systemd, system scope)  >  scope default  >  platform default
```

The `$*_DIRECTORY` tier applies only under system scope on Linux: when cfgd runs
as a systemd system service, systemd injects `$CONFIGURATION_DIRECTORY`,
`$STATE_DIRECTORY`, `$CACHE_DIRECTORY`, and `$RUNTIME_DIRECTORY`; cfgd reads the
first `:`-separated entry from each and prefers it over the FHS defaults. This
means any systemd override (e.g. `StateDirectory=/srv/cfgd-state`) is honored
without any extra cfgd configuration.

The XDG base per role (`XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`,
`XDG_RUNTIME_DIR`) applies under user scope only.

| Root | Flag | Env var |
|---|---|---|
| Config | `--config-dir <dir>` (or `--config <file>`, which wins) | `CFGD_CONFIG_DIR` (or `CFGD_CONFIG`) |
| State | `--state-dir <dir>` | `CFGD_STATE_DIR` |
| Cache | `--cache-dir <dir>` | `CFGD_CACHE_DIR` |
| Runtime | `--runtime-dir <dir>` | `CFGD_RUNTIME_DIR` |

The roots are independent — overriding one does not move the others. `--config`
names the config *file* (or a directory cfgd searches for `cfgd.yaml`/`cfgd.toml`)
and takes precedence over `--config-dir`. `--cache-dir` relocates **both** the
source and module caches (they share one root). `--runtime-dir` relocates the
daemon socket and lock files, and is honored by both `cfgd daemon` and
`cfgd daemon status` so they always agree on the socket path.

### `cfgd paths`

`cfgd paths` reports the four resolved roots, the effective source of each
(`flag`, `env`, or `default`), and the files cfgd owns in each — so you never
have to guess where a host is reading or writing:

```console
$ cfgd paths
cfgd directories

Config
  dir     /home/you/.config/cfgd
  source  default
  file    /home/you/.config/cfgd/cfgd.yaml

State
  dir       /home/you/.local/state/cfgd
  source    default
  db        /home/you/.local/state/cfgd/state.db
  applyLock /home/you/.local/state/cfgd/apply.lock

Cache
  dir     /home/you/.cache/cfgd
  source  default
  sources /home/you/.cache/cfgd/sources
  modules /home/you/.cache/cfgd/modules

Runtime
  dir     /run/user/1000/cfgd
  source  default
  socket  /run/user/1000/cfgd/cfgd.sock
```

`cfgd paths -o json` (or `-o yaml`) emits the same data as a structured object
for scripts; the `source` field reflects any override in effect:

```console
$ cfgd --cache-dir /srv/cfgd-cache paths -o json
{
  "cache": {
    "dir": "/srv/cfgd-cache",
    "modules": "/srv/cfgd-cache/modules",
    "source": "flag",
    "sources": "/srv/cfgd-cache/sources"
  },
  ...
}
```

### macOS: legacy `~/.config/cfgd` migration

Earlier builds stored macOS config at `~/.config/cfgd`. A config dir already
there is **always preferred and read in place**, so upgrading never strands it.
On the first interactive run after the default changed, cfgd prompts once:

```text
Your cfgd config is at ~/.config/cfgd, but the native macOS location is now
~/Library/Application Support/cfgd. How would you like to proceed?
> Move it to ~/Library/Application Support/cfgd
  Keep it at ~/.config (set XDG_CONFIG_HOME in your shell config)
```

- **Move** relocates the directory to the native location (symlinked entries are
  preserved; cfgd refuses if the destination already exists).
- **Keep** sets `XDG_CONFIG_HOME` for the current session and persists it so all
  future shells resolve there. The export is written to the file your shell
  sources for **every** invocation (not just interactive ones): `~/.zshenv` for
  zsh, `~/.profile` for bash, `~/.config/fish/conf.d/cfgd-xdg.fish` for fish. A
  symlinked rc (e.g. into a dotfiles repo) is followed and edited in place, and
  an existing `XDG_CONFIG_HOME` assignment is left untouched. Unrecognized shells
  get printed instructions instead of a guessed file.

The prompt is suppressed when `XDG_CONFIG_HOME` or `--config`/`CFGD_CONFIG`
already pins the location, after you've chosen **Keep** once, for `cfgd daemon`,
and in non-interactive sessions (`--yes`/`CFGD_YES`, no TTY, or structured `-o`
output) — there cfgd silently keeps reading the legacy dir in place. Only the
config dir is affected; **state** and **runtime** data stay under
`~/Library/Application Support/cfgd`. That split is intentional: managed-file
symlink targets are declared explicitly in each file entry, so they don't depend
on where the config dir resides.

### Silent state & cache migration

Earlier builds kept the state DB and the source cache together in one data dir
(`~/.local/share/cfgd` on Linux, `~/Library/Application Support/cfgd` on macOS,
`%LOCALAPPDATA%\cfgd` on Windows). cfgd now resolves **state** and **cache** to
their own roots (the table above). On the first run after upgrading, cfgd
relocates that data to the new defaults automatically — **no prompt**. Unlike the
config dir, state and cache are app-managed (not hand-authored, not git-tracked),
so there is nothing to ask: the state DB (with its WAL sidecars and the device
credential), the queued server config, and the `sources/` cache move to their
new homes, while the module cache — already in the cache root — stays put.

The migration is safe by construction:

- **Per-artifact, never whole-dir.** Only cfgd's own files move; anything else in
  the legacy directory (including a co-located config dir on macOS) is left
  untouched.
- **Crash-safe state DB.** The SQLite WAL is folded into the DB before the file
  is moved; if that step can't run (a locked or degraded DB) the WAL/SHM sidecars
  are carried across so no committed data is lost. An existing state DB at the new
  location is authoritative and never overwritten.
- **Idempotent.** Re-running is a no-op once everything is in place.
- **Override-aware.** The migration runs **only** when both the state and cache
  roots are at their defaults. If you pass `--state-dir`/`--cache-dir` or set
  `CFGD_STATE_DIR`/`CFGD_CACHE_DIR`, cfgd assumes you are driving (e.g. a
  throwaway location) and never moves data into an overridden root.

Run `cfgd paths` afterward to confirm the new locations.

## Linux

On Linux, cfgd supports desktop environment-specific system configurators in addition to the cross-platform features:

| Feature | Linux behavior |
|---|---|
| Desktop configurators | `gsettings` (GNOME/GTK), `kdeConfig` (KDE Plasma), `xfconf` (XFCE) — each active only when its CLI tool is installed |
| System configurators | `systemdUnits`, `environment`; plus node-level configurators (`sysctl`, `kernelModules`, `containerd`, `kubelet`, `apparmor`, `seccomp`, `certificates`) |
| `spec.env` reach | `envScope: All` (default) writes `~/.config/environment.d/cfgd.conf` (read by `systemd --user` + Wayland GUI sessions) and refreshes the live session via `systemctl --user set-environment` |
| Bootstrapped `PATH` | An apply that bootstraps Homebrew records `/home/linuxbrew/.linuxbrew/{bin,sbin}` and exports them from `~/.cfgd.env`, sourced by `~/.bashrc`/`~/.zshrc` — no `brew shellenv` line to add by hand. A brew you installed yourself is left untouched. A prefix cfgd created for a manager you installed (npm's `$HOME/.npm-global`) is exported all the same |
| Daemon service | Registered as a systemd user service; starts at login |

## Windows

On Windows, cfgd supports the same configuration structure with these platform-specific behaviors:

| Feature | Windows behavior |
|---|---|
| Package managers | `winget`, `chocolatey`, `scoop` (in addition to cross-platform managers like `cargo`, `npm`, `pipx`) |
| System configurators | `windowsRegistry`, `windowsServices`; `shell` targets Windows Terminal; `environment` writes to `HKCU\Environment` via `setx` |
| `spec.env` reach | Writes `~/.cfgd-env.ps1` dot-sourced from the PowerShell profiles (and Git Bash rc when present); `envScope: All` (default) also persists vars to `HKCU\Environment` via `setx` |
| Reload reminder | After an apply changes your env, cfgd names the file your current shell can read — `. ~/.cfgd-env.ps1` under PowerShell, `source ~/.cfgd.env` under Git Bash / MSYS2 (detected via `MSYSTEM`, falling back to `SHELL`) |
| File strategy | `Symlink` requires Developer Mode or an elevated prompt; `Copy` is a safe default |
| Daemon service | Registered as a Windows Service via `sc.exe`; starts at boot; logs to `%LOCALAPPDATA%\cfgd\daemon.log` |
| Config directory | `%APPDATA%\cfgd` (equivalent to `~/.config/cfgd` on Unix) |

## Aliases

Define command aliases in `cfgd.yaml`. `cfgd init` scaffolds default aliases — edit or remove them as needed.

```yaml
spec:
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
    up: "apply --yes"
    s: "status"
    pkg: "profile update --package"
```

Default aliases (scaffolded by `cfgd init`):
- `add <path>` → `profile update --file <path>`
- `remove -<path>` → `profile update --file -<path>` (prefix with `-` to remove)

These are not hardcoded — they live in your cfgd.yaml and can be changed or removed.

## AI Configuration

Configure the AI provider for `cfgd generate`:

```yaml
spec:
  ai:
    provider: claude              # AI provider (default: claude)
    model: claude-sonnet-4-6      # Model ID (default: claude-sonnet-4-6)
    apiKeyEnv: ANTHROPIC_API_KEY # Env var containing API key (default: ANTHROPIC_API_KEY)
```

API keys are never stored in config files. The `apiKeyEnv` field names the environment variable to read. CLI flags `--model` and `--provider` override config values.

## Global Flags

These flags work with any subcommand:

| Flag | Short | Env Var | Description |
|---|---|---|---|
| `--config <path>` | | `CFGD_CONFIG` | Path to `cfgd.yaml` (or a directory — cfgd infers `cfgd.yaml`, then `cfgd.toml`, inside it) |
| `--config-dir <dir>` | | `CFGD_CONFIG_DIR` | Override the config directory (`--config` wins over it) |
| `--state-dir <dir>` | | `CFGD_STATE_DIR` | Override the state directory (`state.db`, history, `apply.lock`) |
| `--cache-dir <dir>` | | `CFGD_CACHE_DIR` | Override the cache directory (source, module, and update-check caches) |
| `--runtime-dir <dir>` | | `CFGD_RUNTIME_DIR` | Override the runtime directory (daemon socket, locks) |
| `--profile <name>` | | `CFGD_PROFILE` | Override the active profile |
| `--verbose` | `-v` | `CFGD_VERBOSE` | Show debug output (`-vv` = trace) |
| `--quiet` | `-q` | `CFGD_QUIET` | Suppress all non-error output |
| `--color <auto\|always\|never>` | | `CFGD_COLOR` | When to colorize terminal output. `auto` (default) follows the terminal, `NO_COLOR` and `TERM=dumb`; `always` colorizes even when stderr is not a terminal, for a pager that renders escapes (`less -R`) or a captured transcript; `never` disables it. Colour is never emitted under `-o json`/`yaml`/`name`/`jsonpath`/`template` whatever this says — an escape inside a payload string is corrupt data |
| `--no-color` | | `NO_COLOR` | Disable colored terminal output (alias for `--color never`) |
| `--output <format>` | `-o` | | Output format: `table` (default), `wide`, `json`, `yaml`, `name`, `jsonpath=EXPR`, `template=TMPL`, `template-file=PATH` |
| `--list-envelope` | | `CFGD_LIST_ENVELOPE` | Under `-o json`/`-o yaml`, wrap a top-level array in a KRM `List` envelope (`{apiVersion, kind: List, items}`) |
| `--scope <user\|system>` | | `CFGD_SCOPE` | Installation scope: `user` (default) or `system`. `system` switches all four directory roots to system/FHS defaults (`/etc/cfgd`, `/var/lib/cfgd`, …). See [System scope](configuration.md#system-scope). |
| | | `CFGD_NO_UPDATE_CHECK` | Silence the automatic update check (see [Suppressing the automatic check](#suppressing-the-automatic-check)) |
| | | `NO_UPDATE_NOTIFIER` | Same, via npm's `update-notifier` convention |
| | | `DO_NOT_TRACK` | Same, via the [consoledonottrack.com](https://consoledonottrack.com) convention |

Boolean env vars accept shell-truthy spellings, not just `true`/`false`. The
accept-set matches `CFGD_YES`: `1`/`y`/`yes`/`t`/`true`/`on` (case-insensitive)
enable, `0`/`n`/`no`/`f`/`false`/`off` disable. The three update-check opt-out
variables above are the exception — they follow the npm/consoledonottrack
rule instead (anything except `""`/`"0"`/`"false"` opts out); see
[Suppressing the automatic check](#suppressing-the-automatic-check).

```sh
CFGD_QUIET=1   cfgd profile list -o name   # same as -q
CFGD_VERBOSE=on cfgd plan                  # same as -v; bare integers still work (CFGD_VERBOSE=2 = trace)
```

#### Structured output shapes (`jsonpath` / `template`)

List commands emit a **bare top-level array**, not a kubectl-style `{"items": [...]}`
envelope. Index into it directly — `[0]`, not `.items[0]`:

```sh
cfgd profile list -o json                       # [ { "name": "base", ... }, ... ]
cfgd profile list -o 'jsonpath={[0].name}'      # base
cfgd profile list -o 'jsonpath={[*].name}'      # one name per line
cfgd profile list -o 'jsonpath={.items[0]}'     # empty — no `items` key on a bare array
```

##### KRM `List` envelope (`--list-envelope`)

If you'd rather consume list output as a Kubernetes-style `List` object, pass
the global `--list-envelope` flag (or set `CFGD_LIST_ENVELOPE=1`). It wraps the
top-level array under an `apiVersion: cfgd.io/v1alpha1`, `kind: List`, and an
`items` array carrying the original elements. The default (flag absent) stays a
bare array — this is purely opt-in:

```sh
cfgd source list -o json
# [ { "name": "base", ... }, ... ]

cfgd source list -o json --list-envelope
# {
#   "apiVersion": "cfgd.io/v1alpha1",
#   "items": [ { "name": "base", ... }, ... ],
#   "kind": "List"
# }

cfgd source list -o yaml --list-envelope
# apiVersion: cfgd.io/v1alpha1
# items:
# - name: base
#   ...
# kind: List
```

(Object keys serialize alphabetically — `apiVersion`, `items`, `kind` — as with
every cfgd JSON/YAML payload; key order is not semantically meaningful.)

The envelope shifts the path of every element: a bare-array `[0].name` becomes
`.items[0].name` under the envelope. It applies **only** to `-o json` and
`-o yaml`. The projecting formats (`-o name`, `-o jsonpath=…`, `-o template=…`,
`-o template-file=…`) ignore it and keep operating on the bare data, so your
existing jsonpath/template expressions are never reshaped:

```sh
cfgd source list -o 'jsonpath={[0].name}' --list-envelope   # still indexes the bare array
```

Single-object commands (e.g. `cfgd status`) expose their fields directly, so
`jsonpath={.field}` works against them:

```sh
cfgd status -o 'jsonpath={.drift}'              # extract drift events
```

A malformed `jsonpath` or `template` expression is rejected at parse time with a
usage error (exit `2`); a template that fails to render against the data, or a
`template-file` that cannot be read, writes the error to `stderr` and exits non-zero
(exit `1`) — the structured data channel on `stdout` is never polluted with an error
message, and a failure never reports exit `0`.

The standalone `--jsonpath EXPR` flag is **deprecated** in favor of
`-o jsonpath=EXPR`. It still works but prints a deprecation notice to `stderr`
(the `stdout` data channel stays pure), so scripts piping `stdout` are unaffected:

```sh
cfgd profile list --jsonpath '{[0].name}'   # stdout: base; stderr: deprecation notice
cfgd profile list -o 'jsonpath={[0].name}'  # canonical — no notice
```
