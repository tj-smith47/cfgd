# Multi-Source Config Management

cfgd supports subscribing to multiple config sources — team baselines, security policies, org-wide standards — alongside your personal config. Sources are composed with policy tiers that control what you can and can't override.

This is different from [module registries](modules.md#module-registries), which are simple collections of reusable modules. Sources provide complete profiles with **policy enforcement** — a team can require certain packages, lock certain files, and recommend others, with cfgd enforcing those policies on every reconcile. For the source subscription field reference, see the [Config spec reference](spec/config.md#specsources).

## Conceptual Model

| Concept | Description |
|---|---|
| **ConfigSource** | Team publishes a config source: profiles, modules, packages, files, with a policy manifest |
| **ConfigSubscription** | Developer subscribes to a source in their `cfgd.yaml` |
| **Composition** | Merge engine combines all sources with priority and policy enforcement |

## ConfigSource Manifest

Published by the team as `cfgd-source.yaml` at the root of their config repo. A source must provide at least one **profile** or at least one **module** in `spec.provides` — a manifest with neither is rejected as invalid.

A source delivers only:
- **Profiles** — complete profile specs (`spec.provides.profiles` / `spec.provides.platformProfiles`)
- **Policy tiers** — required, recommended, optional, locked items and constraints (`spec.policy`)
- **Module bodies** — module implementations listed in `spec.provides.modules` (a "module library" source)

Consumer-local top-level config sections (`theme`, `ai`, `daemon`, `fileStrategy`, `compliance`) are **never** source-delivered. They are always local-only and ignored if present in a source's profile.

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-corp-dev
  version: "2.1.0"
  description: "ACME Corp developer environment baseline"
spec:
  provides:
    profiles:
      - acme-base
      - acme-backend
      - acme-frontend
    platformProfiles:
      macos: acme-base
      debian: acme-backend
      ubuntu: acme-backend
      fedora: acme-frontend
      linux: acme-base
    modules: [corp-vpn, corp-certs, approved-editor]

  policy:
    required:
      packages:
        brew:
          formulae: [git-secrets, pre-commit, aws-cli]
      files:
        - source: "linting/.eslintrc.json"
          target: "~/.eslintrc.json"
      modules: [corp-vpn, corp-certs]
    recommended:
      packages:
        brew:
          formulae: [k9s, stern, kubectx]
      modules: [approved-editor]
    optional:
      profiles: [acme-sre]
    locked:
      files:
        - target: "~/.config/company/security-policy.yaml"

    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths:
        - "~/.config/acme/"
        - "~/.config/company/"
```

## Subscribing

In your `cfgd.yaml`:

```yaml
spec:
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
        overrides:
          env:
            EDITOR: nvim
          packages:
            npm:
              global: [prettier]
        reject:
          packages:
            brew:
              formulae: [kubectx]
      sync:
        interval: "1h"
        autoApply: false
        pinVersion: "~2"
        required: false      # best-effort: a load failure warns and skips this source
```

## Adopting a team base profile

The **active profile is always local**. A source-delivered profile is a remote
building block that a local profile *pulls in* via `subscription.profile` — it
can never be `spec.profile` directly. This keeps composition strict: a machine's
active configuration lives in its own repo, and source profiles layer underneath
it by priority.

So to adopt a team's `acme-backend` profile, subscribe a local profile to it
rather than naming it as the active profile:

```yaml
spec:
  profile: workstation          # local — your machine's active profile
  sources:
    - name: acme-corp
      origin:
        type: Git
        url: git@github.com:acme-corp/dev-config.git
      subscription:
        profile: acme-backend   # the team profile, pulled in under `workstation`
        priority: 500
```

If you point `spec.profile` (or `--profile`) at a name that only a subscribed
source provides, cfgd doesn't just say "profile not found" — it names the source
that offers it and prints the exact `subscription` snippet to wire it in, then
reminds you to set `spec.profile` to a local profile. (A plain typo that no
source provides still gets the bare not-found.)

## Platform-Aware Profile Auto-Selection

Cross-platform sources (e.g., a team config with separate macOS/Ubuntu/Fedora profiles) can declare a `platformProfiles` map in their manifest. When a subscriber runs `cfgd source add` without `--profile`, cfgd detects the local platform and selects the matching profile automatically.

```yaml
spec:
  provides:
    profiles: [linux-debian, linux-fedora, macos-arm]
    platformProfiles:
      debian: linux-debian
      fedora: linux-fedora
      macos: macos-arm
      linux: linux-debian
```

Keys are platform identifiers — either a Linux distro ID (from `/etc/os-release`, e.g., `debian`, `ubuntu`, `fedora`, `arch`) or an OS name (`macos`, `linux`, `windows`). Values are profile names that must appear in `profiles` or `profileDetails`.

Matching order:
1. **Exact distro match** — if the machine is Debian, look for a `debian` key
2. **OS fallback** — if no distro key matches, look for a `linux` / `macos` key
3. **No match** — fall through to single-profile auto-select or interactive prompt

When auto-selection succeeds, cfgd prints the selected profile and platform. You can always override with `--profile`:

```sh
cfgd source add git@github.com:acme-corp/dev-config.git --profile linux-fedora
```

## Policy Tiers

Sources use four tiers to control what subscribers can and can't change. The key difference between **locked** and **required** is granularity: locked items can't be touched at all (not even adding alongside), while required items must be present but you can add your own on top.

| Tier | What it means | Example |
|---|---|---|
| **Locked** | Subscriber cannot override, modify, or remove. The source has absolute control. | A security policy file that must be byte-for-byte what the team published |
| **Required** | Must be present, but subscriber can add alongside. | `git-secrets` must be installed, but you can also install your own tools |
| **Recommended** | Applied only when the subscriber sets `acceptRecommended: true`; individual items can still be rejected. | Team suggests k9s, but you prefer a different k8s dashboard |
| **Optional** | Subscriber must explicitly opt in. | An SRE-specific profile most developers don't need |

Local config is always priority 1000. Team sources default to 500. Higher priority wins on conflict.

## Composition Algorithm

When you subscribe to multiple sources, cfgd merges them with your local config. Here's a concrete example:

```
Your machine subscribes to:
  acme-base     (priority 400)  — requires git-secrets
  acme-backend  (priority 500)  — recommends env EDITOR="code"
  local config  (priority 1000) — sets env EDITOR="nvim"

Result:
  git-secrets   — installed (required by acme-base, can't override)
  EDITOR="nvim" — your local env override wins (1000 > 500)
```

The full algorithm for each resource:
1. Collect all declarations from all sources + local
2. If only one source: use it
3. If multiple sources:
   - **Locked**: source wins unconditionally
   - **Required**: packages union; files/env/system — source wins
   - **Recommended + `acceptRecommended: true` + not rejected**: source value as default, local override wins
   - **Recommended + `acceptRecommended: false` (default)**: skip entirely unless individually accepted
   - **Recommended + rejected**: skip entirely
   - **Subscriber `overrides`**: applied just above the source's own recommended/standard items (so they beat what the source recommends) but below its required/locked tiers. Overrides ride one step above the source's own items, so they share the source's rank against local config (priority 1000): below local at the default source priority (500), but above local only if you deliberately raise the source to priority ≥ 1000 (the same "higher priority wins" rule). Because an override rides at its own source's rank, it refines only that source — a *higher-priority sibling source* still wins over it; to override across sources, raise this source's priority or set the value in your local config. Scalar fields (env, aliases, system, files) replace the source's value by name; list fields (packages, modules) are added (union), not replaced.
   - **Multiple non-local sources conflict**: higher priority wins; equal priority — alphabetical source name

## CLI Commands

Connect to a team's config source — cfgd fetches the manifest, shows available profiles and policy breakdown, and walks you through subscribing:

```sh
cfgd source add git@github.com:acme-corp/dev-config.git
```

Manage existing subscriptions:

```sh
cfgd source list                                        # list subscribed sources
cfgd source show acme-corp                              # details, policies, conflicts
cfgd source remove acme-corp                            # unsubscribe
cfgd source update                                      # fetch latest from all sources
```

Override or reject a source's recommendation (e.g., "I don't want kubectx, and I prefer nvim over VS Code"):

```sh
cfgd source override acme-corp reject packages.brew.formulae kubectx
cfgd source override acme-corp set env.EDITOR "nvim"
```

Change how conflicts resolve — higher priority means this source's items win over lower-priority sources:

```sh
cfgd source priority acme-corp 800
```

Switch teams or replace a source entirely:

```sh
cfgd source replace acme-corp git@github.com:newco/dev-config.git
```

Publish your own source:

```sh
cfgd source create my-team                       # create a cfgd-source.yaml
cfgd source edit                                # open cfgd-source.yaml in $EDITOR
```

## Automatic Apply Decisions

When the daemon detects new items from a source update, behavior depends on the daemon policy:

```yaml
daemon:
  reconcile:
    autoApply: true
    policy:
      newRecommended: Notify    # Notify | Accept | Reject
      newOptional: Ignore       # Notify | Ignore
      lockedConflict: Notify    # Notify | Accept
```

- `Notify`: record a pending decision, send notification, don't apply
- `Accept`: automatically apply without prompting
- `Reject`/`Ignore`: skip silently

Resolve pending decisions with `cfgd decide`:

```sh
cfgd decide accept packages.brew.k9s
cfgd decide reject packages.brew.stern
cfgd decide accept --source acme-corp     # accept all from source
cfgd decide accept --all                  # accept everything
```

### How New Items Are Detected

The daemon tracks a hash of each source's merged config. When a source update changes the hash, cfgd diffs the previous merge result against the new one. Any resource present in the new result but absent in the old (or moved to a different policy tier) is treated as a "new item" that needs a decision.

Pending decisions have three states:

| State | Meaning |
|---|---|
| **Pending** | New item detected, awaiting user action |
| **Accepted** | User approved; item included in next reconcile |
| **Rejected** | User declined; item excluded from reconciliation |

Notifications fire once per new pending decision, not on every reconcile cycle. If you don't act on a decision, you won't be reminded again until the source publishes another update that changes that item.

### Edge Cases

- **Source removed while decisions pending** — pending decisions for that source are automatically rejected (source gone = items gone).
- **User manually installs a pending package** — on the next reconcile, cfgd detects the package is already present and auto-accepts the decision (desired state already matches actual state).
- **Policies only apply when `autoApply` is enabled** — when `autoApply: false`, `cfgd plan` shows everything and you decide interactively. Policies are for unattended daemon reconciles.
- **Rejection doesn't persist across source versions** — if you reject an item and the source later updates it (new version, changed description), a fresh pending decision is created. This prevents stale rejections from silently blocking items the team considers important.

## Source Constraints

Sources declare `constraints` in their manifest to limit what they can do on your machine. cfgd enforces these at composition time — before anything is applied.

### `allowedTargetPaths`

Restricts where a source can write files. Any file target outside the declared paths is rejected during composition with an error:

```yaml
constraints:
  allowedTargetPaths:
    - "~/.config/acme/"
    - "~/.config/company/"
    - "~/.eslintrc*"
```

If the source tries to deploy a file to `~/.bashrc` (not in the allowed list), cfgd rejects that file and reports the violation in `cfgd plan`. The rest of the source's items still apply normally.

### `noScripts`

When `true` (the default), the source cannot deliver lifecycle scripts. This covers both **profile-layer** scripts (`preApply`/`postApply`/`preReconcile`/`postReconcile`/`onChange`/`onDrift` on the source's profiles and policy tiers) and **module-body** scripts (the same hooks, plus `prefer: [script]` package installs, on any module the source delivers via `provides.modules`). If a source declares any of these while `noScripts: true`, cfgd rejects them as a fatal error — at composition time for profile-layer scripts and at module-load time for module bodies.

Subscribers can relax this by setting `allowScripts: true` in their subscription:

```yaml
spec:
  sources:
    - name: acme
      subscription:
        profile: acme-backend
        allowScripts: true   # opt in to this source's scripts
```

With `allowScripts: true`, the source's scripts are permitted and `cfgd plan` surfaces a note that they will run, so the execution is visible before any apply.

### `allowSystemChanges`

By default, sources cannot install launch agents, systemd units, or modify shell configuration. A source that attempts to set `shell:` config or deploy a LaunchAgent without `allowSystemChanges: true` in its constraints is rejected. The subscriber must explicitly opt in.

### `noSecretsRead`

When `true`, the source cannot reference or access the subscriber's SOPS/age keys, encrypted files, or secret provider credentials.

## Template Sandboxing

Source templates run in a restricted variable context. A source template can access:

- **Source-provided variables** — env vars declared in the source's own profile
- **System facts** — `__os`, `__arch`, `__hostname`, `__distro` (detected at reconcile time)

Personal env vars from your local profile are **not** available to source templates. This prevents a team source from reading or exfiltrating your personal configuration.

Example of what a source template sees:

```yaml
# Source provides these variables:
env:
  - name: COMPANY_PROXY
    value: "proxy.acme.com:8080"

# Source template (~/.config/acme/proxy.conf):
proxy_host = {{ COMPANY_PROXY }}
platform = {{ __os }}
arch = {{ __arch }}

# These are NOT available in source templates:
# EDITOR, GITHUB_TOKEN, or any variable from your local profile
```

## Composition Priority Details

When multiple sources (and your local config) declare the same resource, priority determines which value wins. Each source has a numeric priority:

| Source Type | Default Priority |
|---|---|
| Local config | 1000 |
| Team sources | 500 |

Higher priority wins. When two sources have equal priority, the source whose name comes first alphabetically wins (deterministic tiebreaker). **Locked items always win regardless of priority** — a locked file at priority 400 overrides a local file at priority 1000.

Here's a concrete three-source conflict:

```
Sources (subscriber has acceptRecommended: true):
  acme-base     (priority 400)  — sets EDITOR="nano"      (recommended)
  acme-backend  (priority 500)  — sets EDITOR="code"      (recommended)
  local config  (priority 1000) — sets EDITOR="nvim"

Resolution for EDITOR:
  acme-base loses to acme-backend (500 > 400)
  acme-backend loses to local (1000 > 500; recommended items can be overridden by local)
  Result: EDITOR="nvim"

Without acceptRecommended: true:
  Both recommended EDITOR values are skipped entirely.
  Result: EDITOR="nvim" (local only)

But if acme-backend had EDITOR as "locked":
  Locked always wins regardless of priority
  Result: EDITOR="code" (local override rejected)
```

## Version Pinning

A source subscribed **without** `pinVersion` is **floating**: it tracks the remote's default-branch HEAD and is not reproducible — any `cfgd source update` may advance it to a different commit. Pin the source to get a reproducible ref.

The `pinVersion` field pins a source to a concrete **git ref** — a tag selected from the source repository's tags, or an exact commit SHA. cfgd resolves the pin against the remote's git tags (via `git ls-remote --tags`), **not** the source's self-reported `metadata.version`. This is more secure: a source cannot bypass your pin by editing the version string in its own `cfgd-source.yaml`. A checked-out pin is always a detached HEAD on the resolved ref.

A `pinVersion` value is interpreted in this order:

1. **Semver range** — list the source's git tags, strip a leading `v`, filter by the range, and check out the **highest** matching tag. It never checks out a tag outside the range. When no tag matches, behaviour depends on whether a previously-resolved checkout exists and whether the source is `required` — see [When a pin stops matching](#when-a-pin-stops-matching) below.
2. **Commit SHA** (7–40 hex characters) — check out that exact commit. A SHA is an immutable pin: it always resolves to the same commit.
3. **Exact tag name** (e.g. `release-2024`) — check out that tag verbatim.

Semver range syntax for case 1:

| Syntax | Meaning | Selects from tags `v1.0.0, v2.0.0, v2.1.0` | Rejects |
|---|---|---|---|
| `~2` | Highest 2.x | `v2.1.0` | 3.x |
| `^1.5` | Highest 1.5+ within major 1 | `v1.x` ≥ 1.5 | 2.0.0 |
| `>=1.0.0` | Highest at least 1.0.0 | `v2.1.0` | 0.9.0 |
| `~2.1` | Highest 2.1.x | `v2.1.x` | 2.2.0 |
| `2.0.0` | Exactly 2.0.0 (a bare full version pins exactly, not caret) | `v2.0.0` | v2.1.0 |

`--branch` and `--pin-version` are mutually exclusive on `cfgd source add` — a pin selects its own ref, so a branch would be meaningless.

```sh
cfgd source add https://github.com/acme/config.git --pin-version "~2"
cfgd source add https://github.com/acme/config.git --pin-version "v2.1.0"
cfgd source add https://github.com/acme/config.git --pin-version "9f3c1ab2c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9"
```

On `cfgd source update`, a semver-range pin is **re-resolved** so a newly-published higher matching tag is picked up; a tag or commit-SHA pin is immutable and stays put. To move the pin, change your `pinVersion`.

For commit-SHA pins, cfgd first tries a shallow fetch of the commit; if the server refuses (no `uploadpack.allowReachableSHA1InWant`), it deepens the fetch and prints a note so the depth relaxation is never silent.

### When a pin stops matching

When a source advances but **no tag matches your range** (or an exact tag/SHA pin no longer resolves), the outcome depends on `sync.required` and whether a prior load already cached a checkout:

| Situation | Behaviour |
|---|---|
| Cache exists + `required: false` (default) | cfgd **keeps the previously-resolved checkout** and warns. The source still composes (its policy tiers, profiles, and module bodies stay in effect) at the last-known-good ref. Change your `pinVersion` to move forward. |
| Cache exists + `required: true` | **Fatal** — a required source whose pin can't resolve aborts apply/plan rather than silently composing a stale ref. |
| No prior checkout (first-ever load) | Resolution **errors**. For a non-required source the error is warned and the source is skipped; for a `required` source it is fatal. |

This keep-previous fallback applies only to the pin-not-found case. A network/`ls-remote` failure, a corrupt cached manifest, or a failed signature is always an error.

## Required (fail-closed) sources

By default a source is **best-effort**: if it can't be fetched (network error, bad manifest, signature failure, or an unresolvable first-time pin), cfgd warns and composes without it, and apply/plan still succeed. That is wrong for a security or team baseline that **must** always be present.

Set `sync.required: true` to make the source **fail-closed**: if the source is unavailable for **any** reason — a failed fetch, a bad/unsigned cached manifest, an unresolvable pin, or simply never having been synced — its absence is fatal. The check lives at the composition chokepoint that every command flows through, so it is enforced uniformly across the refresh path *and* the offline read/daemon paths:

| Surface | Behaviour when a `required` source is unavailable |
|---|---|
| `cfgd apply` / `cfgd plan` (refresh) | **Aborts**, naming the source (exit code `4`, config-invalid). |
| `cfgd diff` / `status` / `verify` / `compliance` / `checkin` (offline read) | **Errors** instead of composing without it — a never-synced or cache-missed required source is never silently absent. |
| daemon reconcile tick | **Skips the tick** and raises an alert. The pruning reconcile never runs against a desired set that is missing the required source, so its packages/modules are never uninstalled as phantom drift. Run `cfgd sync` then `cfgd status` to recover. |

```yaml
spec:
  subscriptions:
    - origin:
        url: https://github.com/acme/security-baseline.git
      sync:
        pinVersion: "~2"
        required: true       # baseline must load, or every path fails closed
```

`required` is independent of the policy **required** *tier* (which marks individual items the subscriber must keep): `sync.required` governs whether the whole source must load at all.

## Source-Delivered Module Bodies

A source can act as a **module library**: it delivers module implementations (bodies) via `spec.provides.modules`. The list is the delivery allow-list — only modules named there are made available to subscribers.

A subscribed profile may reference a module from the source the same way it references a local module. When cfgd resolves a module name, it checks:

1. **Local modules** — modules in `<config-dir>/modules/` always win.
2. **Source modules by priority** — if the module exists in multiple subscribed sources, the higher-priority source wins. Equal priority is tie-broken by source name (alphabetical).

Referencing a module that is neither consumer-local nor listed in any subscribed source's `provides.modules` is a **fatal error** (`ModuleError::NotFound`), naming the source that could have offered it if its allow-list included it.

`cfgd plan` and `cfgd source show` display the originating source for each source-delivered module:

```
nvim        unchanged   <- acme-corp
corp-vpn    install     <- acme-corp
```

### Module-library-only sources

A source that delivers only modules (no profiles) is valid — `spec.provides.profiles` may be empty as long as `spec.provides.modules` is non-empty. This lets teams publish reusable module collections without a full profile.

```yaml
spec:
  provides:
    modules: [corp-vpn, corp-certs, approved-editor]
  # No profiles field required for a module-library source
```

## Source Removal

When you remove a source with `cfgd source remove`, cfgd needs to know what to do with the packages, files, and settings that source provided.

By default, removal is interactive — cfgd lists each resource from the source and asks whether to keep or remove it. Use flags to skip the prompt:

```sh
cfgd source remove acme-corp                # interactive: review each resource
cfgd source remove acme-corp --keep-all     # keep everything as locally managed
cfgd source remove acme-corp --remove-all   # uninstall/delete everything from the source
```

Resources you keep become part of your local config (priority 1000) with no source policy enforcement. They behave exactly like resources you added yourself.

## Publishing a ConfigSource

To publish a config source for your team:

1. Create a git repository with your team's profiles, files, and modules.

2. Add `cfgd-source.yaml` at the repository root (or use `cfgd source create` to scaffold one):

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: my-team-dev
  version: "1.0.0"
  description: "My team's developer environment"
spec:
  provides:
    profiles:
      - base
      - backend
    platformProfiles:
      macos: base
      debian: backend
      linux: base
  policy:
    required:
      packages:
        brew:
          formulae: [git-secrets, pre-commit]
    recommended:
      packages:
        brew:
          formulae: [k9s, stern]
    constraints:
      noScripts: true
      allowedTargetPaths:
        - "~/.config/my-team/"
```

3. Organize your repository:

```
my-team-config/
├── cfgd-source.yaml          # source manifest (required)
├── profiles/
│   ├── base.yaml             # referenced in spec.provides.profiles
│   └── backend.yaml
├── files/
│   └── linting/.eslintrc.json
└── modules/
    └── corp-vpn/
        └── module.yaml
```

4. Test locally before publishing:

```sh
# In another directory, subscribe to the local path
cfgd source add /path/to/my-team-config
cfgd plan    # verify the composed result
```

5. Push to a git remote. Team members subscribe with:

```sh
cfgd source add git@github.com:my-team/dev-config.git
```

Cut a git **tag** (e.g. `v2.1.0`) when releasing a new version of the source. Subscribers with semver-range `pinVersion` values resolve against your tags and will only check out tags within their pinned range. (`metadata.version` in `cfgd-source.yaml` is informational; pinning is enforced against signed git refs, not that field.)

## Security Model

| Threat | Mitigation |
|---|---|
| Arbitrary code execution | `noScripts: true` by default; scripts require explicit subscriber approval and are shown in plan |
| Secret exfiltration | Sources cannot access your SOPS/age keys or encrypted files |
| Arbitrary path writes | Sources must declare `allowedTargetPaths`; enforced at composition level |
| Template data leak | Source templates can only access source-provided env vars, not your personal env vars |
| MITM | Git SSH/HTTPS transport security; optional signature verification |
| Version pinning bypass | `pinVersion` resolved against git tags/refs, not the source's self-reported `metadata.version` — a source cannot edit its manifest to escape the pin, and a tag outside `~2` is never checked out |
| Privilege escalation | Sources cannot set `shell:` or install launchAgents/systemdUnits without `allowSystemChanges: true` |
| Recursive trust | A ConfigSource cannot itself subscribe to other ConfigSources |

Every new capability requested by a source update requires interactive confirmation. The daemon never auto-applies permission-expanding changes.
