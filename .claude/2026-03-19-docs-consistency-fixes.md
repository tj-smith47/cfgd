# Documentation Consistency Fixes

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all inconsistencies across docs/ and README.md — wrong CLI syntax, missing phases, wrong flag names, casing errors, cross-doc contradictions, structural gaps.

**Architecture:** Pure documentation edits. No code changes. Each task targets one file (or one logical group of related fixes). Tasks are ordered so cross-references stay consistent — files referenced by many others are fixed first.

**Tech Stack:** Markdown files only.

---

## Task 1: Fix `docs/reconciliation.md` — missing Env phase and incomplete configurator list

**Files:**
- Modify: `docs/reconciliation.md:8-14`

- [ ] **Step 1: Add the missing `Env` phase and complete the system configurator list**

Replace lines 8-14 with:

```markdown
1. **Modules** — resolve module dependencies, install module packages, deploy module files
2. **System** — shell, macOS defaults, launch agents, systemd units, environment, sysctl, kernel-modules, containerd, kubelet, apparmor, seccomp, certificates
3. **Packages** — install/uninstall across all package managers (profile-level packages)
4. **Files** — copy, template, set permissions (profile-level files)
5. **Env** — write env vars and shell aliases to `~/.cfgd.env`, inject shell rc source lines
6. **Secrets** — decrypt SOPS files, resolve external provider references
7. **Scripts** — run pre/post-reconcile scripts
```

- [ ] **Step 2: Verify no broken cross-references**

Grep for "6 phases" or "six phases" across docs/ to confirm no other doc hardcodes the old count.

- [ ] **Step 3: Commit**

```bash
git add docs/reconciliation.md
git commit -m "docs: add missing Env phase and complete system configurator list in reconciliation.md"
```

---

## Task 2: Fix `docs/safety.md` — snake_case YAML key and prose reference

**Files:**
- Modify: `docs/safety.md:70,76`

- [ ] **Step 1: Fix prose reference on line 70**

Replace: `` `drift_policy` `` with `` `drift-policy` ``

- [ ] **Step 2: Fix YAML key on line 76**

Replace:
```yaml
      drift_policy: NotifyOnly  # Auto | NotifyOnly | Prompt
```
With:
```yaml
      drift-policy: NotifyOnly  # Auto | NotifyOnly | Prompt
```

- [ ] **Step 3: Commit**

```bash
git add docs/safety.md
git commit -m "docs: fix drift_policy to kebab-case drift-policy in safety.md"
```

---

## Task 3: Fix `docs/cli-reference.md` — wrong `--server` flag name

**Files:**
- Modify: `docs/cli-reference.md:479`

- [ ] **Step 1: Fix the flag table for `cfgd enroll`**

Replace:
```markdown
| `--server <url>` | Device gateway URL |
```
With:
```markdown
| `--server-url <url>` | Device gateway URL |
```

- [ ] **Step 2: Commit**

```bash
git add docs/cli-reference.md
git commit -m "docs: fix --server to --server-url in enroll flag table"
```

---

## Task 4: Fix `docs/operator.md` — conflating `init` and `enroll`

**Files:**
- Modify: `docs/operator.md:145-146`

- [ ] **Step 1: Fix the bootstrap token enrollment example (line 145)**

Replace:
```markdown
2. User runs `cfgd init --server <url> --token <token>`
```
With:
```markdown
2. User runs `cfgd enroll --server-url <url> --token <token>`
```

- [ ] **Step 2: Fix the second `cfgd init --server` occurrence (line 156)**

Replace:
```markdown
cfgd init --server https://cfgd.acme.com --token <bootstrap-token>
```
With:
```markdown
cfgd enroll --server-url https://cfgd.acme.com --token <bootstrap-token>
```

- [ ] **Step 3: Commit**

```bash
git add docs/operator.md
git commit -m "docs: fix init to enroll in operator.md enrollment example"
```

---

## Task 5: Fix `docs/modules.md` — document actual CLI for remote modules

**Files:**
- Modify: `docs/modules.md:411-470`

- [ ] **Step 1: Fix the CLI commands section to match reality**

The `cfgd module add` and related commands shown in modules.md (lines 434-437) describe adding from registries/git URLs, but these are not exposed as standalone CLI commands. The remote module functionality is accessed through profile module references in YAML. Update the "Adding Modules" subsection (lines 429-440) to:

```markdown
### Adding Modules

Add a local module to your profile, or reference remote modules in your profile YAML:

```sh
cfgd module create nvim                    # create a new local module
cfgd profile update --active --module nvim        # add local module to active profile
```

For registry or git-hosted modules, reference them in your profile YAML:

```yaml
spec:
  modules:
    - nvim                                        # local module (from modules/ dir)
    - community/tmux                              # from "community" registry
```

When cfgd encounters a registry reference during apply, it clones or fetches the registry repo, checks out the appropriate tag, copies the module, and creates a lockfile entry.
```

- [ ] **Step 2: Fix security section `module add` reference (line 491)**

Replace:
```markdown
  cfgd module add community/experimental-tool --allow-unsigned
```
With:
```markdown
  cfgd module upgrade community/experimental-tool --allow-unsigned
```

- [ ] **Step 3: Commit**

```bash
git add docs/modules.md
git commit -m "docs: fix module add CLI to match actual implementation"
```

---

## Task 6: Fix `README.md` — wrong module add syntax, missing safety doc

**Files:**
- Modify: `README.md:48-58,122-138`

- [ ] **Step 1: Fix the Shareable Modules section**

Replace:
```markdown
```sh
# Install a complete neovim setup — binary, plugins, config, env vars, post-install
cfgd module add --url https://github.com/jane/nvim-module

# Or create your own reusable module
cfgd module create my-dev-env
```
```

With:
```markdown
```sh
# Create your own reusable module
cfgd module create my-dev-env

# Or reference a remote module in your profile
cfgd profile update --active --module community/nvim
```
```

- [ ] **Step 2: Add safety.md to the documentation table**

Add after the Bootstrap row:
```markdown
| [Safety](docs/safety.md) | Atomic writes, backups, rollback, apply locking, path safety |
```

- [ ] **Step 3: Fix "15 native managers" wording**

Replace:
```markdown
- [15 package managers](docs/packages.md) — brew, apt, cargo, npm, pipx, dnf, pacman, snap, flatpak, nix, apk, zypper, yum, pkg, go (plus custom script-based managers)
```
With:
```markdown
- [15 package managers](docs/packages.md) — brew, apt, dnf, yum, pacman, apk, zypper, pkg, cargo, npm, pipx, snap, flatpak, nix, go (plus custom script-based managers)
```

And in the comparison table, change "15 native managers" to "15 managers":
```markdown
| **Packages** | 15 managers | None | Nix only | Any (via tasks) |
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: fix module CLI syntax, add safety doc to table, fix manager count wording"
```

---

## Task 7: Fix `docs/bootstrap.md` — wrong module add syntax

**Files:**
- Modify: `docs/bootstrap.md:160-166`

- [ ] **Step 1: Fix the module add example**

Replace:
```markdown
## Adding Modules

Modules can be added after init with `cfgd module add` for remote modules or `cfgd module create` for local ones:

```sh
cfgd init --from git@github.com:you/machine-config.git
cfgd module add --url https://github.com/jane/nvim-module
cfgd apply
```
```

With:
```markdown
## Adding Modules

Modules can be added after init with `cfgd module create` for local ones, or referenced in profile YAML for remote modules:

```sh
cfgd init --from git@github.com:you/machine-config.git
cfgd module create my-tool
cfgd apply
```
```

- [ ] **Step 2: Commit**

```bash
git add docs/bootstrap.md
git commit -m "docs: fix module add syntax in bootstrap.md"
```

---

## Task 8: Add Helm chart paths to `CLAUDE.md` module map

**Files:**
- Modify: `CLAUDE.md` (module map section)

The module map omits the Helm chart directories entirely. Add them.

- [ ] **Step 1: Add operator chart under cfgd-operator section**

After the `gateway/` block in the cfgd-operator tree, add:
```
    └── chart/cfgd-operator/ # Operator Helm chart
```

- [ ] **Step 2: Add DaemonSet chart as a top-level entry**

After the `crates/` tree, add:
```
charts/
└── cfgd/                   # DaemonSet agent Helm chart
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: add Helm chart paths to CLAUDE.md module map"
```

---

## Task 9: Fix `--name` flag to positional arg in `docs/cli-reference.md` and `docs/modules.md`

**Files:**
- Modify: `docs/cli-reference.md:223`
- Modify: `docs/modules.md:416`

`name` is a positional argument in `ModuleCreateArgs`, not a `--name` flag.

- [ ] **Step 1: Fix cli-reference.md**

Replace `cfgd module create --name my-tool` with `cfgd module create my-tool` (line 223).

- [ ] **Step 2: Fix modules.md**

Replace `cfgd module create --name my-tool` with `cfgd module create my-tool` (line 416).

- [ ] **Step 3: Commit**

```bash
git add docs/cli-reference.md docs/modules.md
git commit -m "docs: fix module create --name to positional arg"
```

---

## Task 10: Structural consistency pass — add CLI cross-references to docs missing them

**Files:**
- Modify: `docs/packages.md` (add CLI cross-reference)
- Modify: `docs/system-configurators.md` (add CLI cross-reference)
- Modify: `docs/templates.md` (add CLI cross-reference)

- [ ] **Step 1: Add cross-reference to packages.md**

Add at the end of `docs/packages.md`:
```markdown

See the [CLI reference](cli-reference.md) for `cfgd profile update --package` and `cfgd module update --package` commands.
```

- [ ] **Step 2: Add cross-reference to system-configurators.md**

Add at the end of `docs/system-configurators.md`:
```markdown

See the [CLI reference](cli-reference.md) for `cfgd profile update --system` and `cfgd profile create --system` commands.
```

- [ ] **Step 3: Add cross-reference to templates.md**

Add at the end of `docs/templates.md`:
```markdown

See the [CLI reference](cli-reference.md) for `cfgd profile update --file` and `cfgd module update --file` commands.
```

- [ ] **Step 4: Commit**

```bash
git add docs/packages.md docs/system-configurators.md docs/templates.md
git commit -m "docs: add CLI reference cross-links to packages, system-configurators, templates"
```
