# Multi-Source Config Management

cfgd supports subscribing to multiple config sources — team baselines, security policies, org-wide standards — alongside your personal config. Sources are composed with policy tiers that control what you can and can't override.

This is different from [module registries](modules.md#module-registries), which are simple collections of reusable modules. Sources provide complete profiles with **policy enforcement** — a team can require certain packages, lock certain files, and recommend others, with cfgd enforcing those policies on every reconcile.

## Conceptual Model

| Concept | Description |
|---|---|
| **ConfigSource** | Team publishes a config source: profiles, modules, packages, files, with a policy manifest |
| **ConfigSubscription** | Developer subscribes to a source in their `cfgd.yaml` |
| **Composition** | Merge engine combines all sources with priority and policy enforcement |

## ConfigSource Manifest

Published by the team as `cfgd-source.yaml` at the root of their config repo:

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
            - name: EDITOR
              value: "nvim"
        reject:
          packages:
            brew:
              formulae: [kubectx]
      sync:
        interval: "1h"
        autoApply: false
        pinVersion: "~2"
```

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
| **Recommended** | Applied by default, but subscriber can reject specific items. | Team suggests k9s, but you prefer a different k8s dashboard |
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
   - **Recommended + not rejected**: source value as default, local override wins
   - **Recommended + rejected**: skip entirely
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
cfgd source create --name my-team               # create a cfgd-source.yaml
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

When `true` (the default), the source cannot include `preReconcile` or `postReconcile` scripts. If a source manifest declares scripts while `noScripts: true`, cfgd rejects those scripts at composition time. Subscribers can relax this by setting `allowScripts: true` in their subscription — the scripts are then shown in `cfgd plan` before execution.

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
Sources:
  acme-base     (priority 400)  — sets EDITOR="nano"      (recommended)
  acme-backend  (priority 500)  — sets EDITOR="code"      (recommended)
  local config  (priority 1000) — sets EDITOR="nvim"

Resolution for EDITOR:
  acme-base loses to acme-backend (500 > 400)
  acme-backend loses to local (1000 > 500, and recommended allows override)
  Result: EDITOR="nvim"

But if acme-backend had EDITOR as "locked":
  Locked always wins regardless of priority
  Result: EDITOR="code" (local override rejected)
```

## Version Pinning

The `pinVersion` field in your subscription restricts which source versions cfgd will accept. It uses semver range syntax:

| Syntax | Meaning | Accepts | Rejects |
|---|---|---|---|
| `~2` | Compatible with 2.x | 2.0.0, 2.1.0, 2.9.9 | 3.0.0 |
| `^1.5` | Compatible with 1.5+ | 1.5.0, 1.6.0, 1.99.0 | 2.0.0 |
| `>=1.0.0` | At least 1.0.0 | 1.0.0, 2.0.0, 99.0.0 | 0.9.0 |
| `~2.1` | Compatible with 2.1.x | 2.1.0, 2.1.5 | 2.2.0 |

When a source update pushes a version outside your pinned range, cfgd rejects the update with an error and keeps the previous version. The rejection appears in `cfgd status` and daemon notifications. To accept the new version, update your `pinVersion` range.

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

Bump `metadata.version` in `cfgd-source.yaml` when making changes. Subscribers with `pinVersion` ranges will only receive updates within their pinned range.

## Security Model

| Threat | Mitigation |
|---|---|
| Arbitrary code execution | `noScripts: true` by default; scripts require explicit subscriber approval and are shown in plan |
| Secret exfiltration | Sources cannot access your SOPS/age keys or encrypted files |
| Arbitrary path writes | Sources must declare `allowedTargetPaths`; enforced at composition level |
| Template data leak | Source templates can only access source-provided env vars, not your personal env vars |
| MITM | Git SSH/HTTPS transport security; optional signature verification |
| Version pinning bypass | `pinVersion` enforced — source v3.0.0 rejected if pinned to `~2` |
| Privilege escalation | Sources cannot set `shell:` or install launchAgents/systemdUnits without `allowSystemChanges: true` |
| Recursive trust | A ConfigSource cannot itself subscribe to other ConfigSources |

Every new capability requested by a source update requires interactive confirmation. The daemon never auto-applies permission-expanding changes.
