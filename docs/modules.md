# Modules

Modules are self-contained, portable configuration packages. A module bundles everything needed for one tool — packages (cross-platform), config files (local or git-sourced), and lifecycle scripts — into a single deployable unit.

## Why Modules

Without modules, profiles declare packages by manager: `brew: [neovim]`, `apt: [neovim]`. This means no portability (a profile for macOS doesn't work on Ubuntu), no granularity (you can't apply "just my nvim setup"), and no dependency tracking (nvim needs Node.js for LSP but that's implicit).

### Modules vs. Profile Packages

Use **modules** when the config is self-contained and shareable — a tool with its own config files, dependencies, and setup scripts. Use **profile packages** for machine-specific package lists that don't need to be portable or shared.

Rule of thumb: if you'd share it with a coworker or use it across machines with different OSes, it's a module. If it's "install these five tools on my work laptop," it's a profile package list.

## Module Spec

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
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

### Package Entry Fields

| Field | Required | Type | Description |
|---|---|---|---|
| `name` | yes | string | Canonical package name |
| `minVersion` | no | string | Minimum acceptable version (semver) |
| `prefer` | no | list | Ordered list of managers to try. `"script"` uses the `script` field as a custom installer. If omitted, uses platform's native manager. |
| `aliases` | no | map | Per-manager name overrides when the package name differs |
| `script` | no | string | Inline shell script or path. Used when `prefer` includes `"script"` |
| `platforms` | no | list | Platform filter — skip on non-matching platforms. Values: OS (`linux`, `macos`), distro (`ubuntu`, `fedora`, `arch`), or arch (`x86_64`, `aarch64`) |

### File Entry Fields

| Field | Required | Type | Description |
|---|---|---|---|
| `source` | yes | string | Local path (relative to module dir), or git URL |
| `target` | yes | string | Absolute target path on the machine |

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

### Aliases

Modules can declare shell aliases. These are merged with profile aliases using the same conflict rules as env vars — module wins on conflict by name.

```yaml
spec:
  aliases:
    - name: vim
      command: nvim
    - name: vimdiff
      command: nvim -d
```

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
2. **Determine candidate managers.** If `prefer` is specified, walk that list in order. If `prefer` is omitted, use the platform's native manager (e.g., `apt` on Ubuntu, `brew` on macOS).
3. **For each candidate manager:**
   - If the candidate is `"script"` — the `script` field must be present (error if missing). Scripts are always considered "available," and version checks are skipped (the script manages its own versioning). See [Script Execution](#script-execution) below.
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

Modules declare `depends: [node, python]`. cfgd builds a dependency graph and figures out the install order — dependencies are installed before the modules that need them. Circular dependencies are detected and reported as errors. If two modules share a dependency (A→C, B→C), it's resolved and installed once.

Processing order: leaf dependencies first (node, python), then dependents (nvim).

## Post-Apply Scripts

Scripts listed under `scripts.postApply` run after all of the module's packages are installed and files are deployed. They execute sequentially in the order listed. If a script fails, subsequent scripts in that module are skipped and the failure is reported in the plan output.

Use `postApply` scripts for tasks that depend on the packages and files being in place — plugin installations, cache rebuilds, index updates:

```yaml
scripts:
  postApply:
    - nvim --headless "+Lazy! sync" +qa
    - nvim --headless -c "MasonInstallAll" -c "qa"
```

Each script runs with `/bin/sh -e` in the module directory.

## Profile Integration

Profiles declare which modules to use via the `modules` field. Module packages and profile-level packages coexist. If the same package appears in both, the module's version constraint and preference take priority (a module is more specific than a profile package list).

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work-mac
spec:
  modules: [nvim, tmux, git, zsh]

  # Existing fields still work — modules don't replace them
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

Git sources are cached in `~/.local/share/cfgd/module-cache/` and updated on `cfgd apply` or daemon sync.

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
      module.yaml     # just packages, no files
```

## Module Registries

Registries are git repos that host multiple reusable modules. Think of them as community or organization module collections — you browse and install from them instead of writing everything yourself.

This is different from [config sources](sources.md), which provide full profiles with policy enforcement. Registries are simpler: just a directory of modules, no policy tiers.

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

### Registry Tag Convention

Registries use per-module git tags in the format `<module>/<version>` — for example, `tmux/v1.0.0`, `nvim/v2.3.1`. This allows a single git repo to host multiple modules with independent version histories. When you install a module at a specific version, cfgd checks out the tag matching that module name.

### Module Source Configuration

Configure module registries in your `cfgd.yaml`:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-workstation
spec:
  module-sources:
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

`cfgd status` includes a per-module health section:

```
Modules:
  ✓ nvim       3 packages, 12 files, healthy
  ✓ tmux       1 package, 1 file, healthy
  ⚠ git        1 package, outdated (git source has new commits)
```

Each module is tracked independently. cfgd stores a hash of the resolved package list and deployed file tree. When the daemon runs its reconciliation loop, it checks:

- **Package drift:** are all resolved packages still installed at the expected versions?
- **File drift:** do deployed files still match the source content?
- **Git source drift:** for modules with git file sources, have new commits appeared upstream since the last apply?

A module's status is one of: `installed` (healthy), `outdated` (upstream has changed), or `error` (a package is missing or a file has diverged).

## Plan Output Format

`cfgd plan` shows module actions grouped by module, with dependencies, resolved managers, and file deployments:

```
Modules:
  nvim (depends: node, python)
    ✓ node — resolved: apt install nodejs (18.19.0)
    ✓ python — resolved: apt install python3 (3.10.12), pipx install pynvim
    + neovim — snap install nvim (0.10.2, prefer: [brew, snap, apt], minVersion: 0.9)
    + ripgrep — apt install ripgrep (14.1.0)
    + fd — apt install fd-find (8.7.0, alias: fd→fd-find)
    + neovim — npm install -g neovim (companion)
    + pynvim — pipx install pynvim (companion)
    → deploy: ~/.config/nvim/ (from module files, 12 files)
    → postApply: nvim --headless "+Lazy! sync" +qa
    → postApply: nvim --headless -c "MasonInstallAll" -c "qa"

Packages: (profile-level)
  = apt: 3 packages up to date
  + brew install extra-tool

Files: (profile-level)
  = 5 files up to date
```

Module actions appear before profile-level packages and files. Dependencies are shown with `✓` (already satisfied) or `+` (will be installed). The `→` prefix marks file deployments and postApply scripts.

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

Use `cfgd module upgrade` to move to a newer version.

## CLI Commands

```sh
cfgd module list                    # list modules and their status
cfgd module show nvim               # show details: packages, files, deps, resolved managers
cfgd module show nvim --show-values # reveal full env variable values (masked by default)
cfgd module create my-tool          # create a new local module
cfgd module update nvim --package ripgrep  # modify a module
cfgd module edit nvim               # open in $EDITOR
cfgd module delete nvim             # restore adopted files, delete module
cfgd module delete nvim --purge    # remove deployed target files, delete module
```

### File Adoption

When you create a module with `--file`, cfgd **adopts** the file: it copies it into the module directory (`modules/<name>/files/`) and replaces the original with a symlink pointing back to the repo copy. This means the file is now version-controlled in your cfgd repo while still accessible at its original location.

`cfgd module delete` reverses this — any target that is still a symlink pointing into the module directory is restored to a regular file before the module is removed. Use `--purge` to instead remove all deployed target files entirely (skipping restoration).

### Adding Modules

Add a local module to your profile, or reference remote modules in your profile YAML:

```sh
cfgd module create nvim                       # create a new local module
cfgd profile update --module nvim              # add local module to active profile
```

For registry or git-hosted modules, reference them in your profile YAML:

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
cfgd module upgrade tmux                     # latest available
cfgd module upgrade tmux --ref tmux/v2.0     # specific version
```

### Searching

Search registries for modules matching a query:

```sh
cfgd module search tmux
```

### Apply/Plan by Module

```sh
cfgd apply --module nvim            # apply only nvim and its dependencies
cfgd apply --dry-run --module nvim  # preview module changes
```

### Bootstrap a Single Module

```sh
cfgd init --from git@github.com:jane/dotfiles.git --module nvim
```

Clones the repo, finds the module, resolves deps, detects platform, and applies just that module.

## Security

### Signature Verification

Remote modules can be signed with GPG or SSH keys. cfgd verifies signatures when present and supports three trust modes:

- **Verify if present (default).** If a module has a signature, cfgd verifies it. If verification fails, the module is rejected. If no signature is present, the module is accepted with a warning.
- **Require signatures.** All remote modules must have valid signatures. Unsigned modules are rejected. Enable this in `cfgd.yaml`:
  ```yaml
  spec:
    module-sources:
      - name: community
        url: https://github.com/cfgd-community/modules.git
        requireSignatures: true
  ```
- **Skip verification.** Use `--allow-unsigned` on the CLI to bypass signature checks for a single operation. This is intended for development and testing, not production use.
  ```sh
  cfgd module upgrade community/experimental-tool --allow-unsigned
  ```

### Lockfile Integrity

The lockfile (`modules.lock`) stores a sha256 hash of each module's directory contents. On every apply, cfgd recomputes the hash and compares it to the locked value. A mismatch means the module content has changed since it was locked — cfgd will refuse to apply and report the discrepancy. Run `cfgd module upgrade` to re-lock at the new content.
