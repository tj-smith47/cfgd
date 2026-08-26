# Profiles

Profiles declare the desired state of a machine. They can inherit from other profiles to share common configuration.

For the complete field-by-field reference, see the [Profile spec reference](spec/profile.md).

## Layout

Profiles live under `profiles/` in your config dir. The canonical layout is a **bundle**: a
directory per profile holding `profile.yaml` next to a `files/` payload:

```
profiles/
├── base/
│   └── profile.yaml
└── workstation/
    ├── profile.yaml
    └── files/
        └── gitconfig
```

The legacy **flat** form (`profiles/<name>.yaml` or `.yml`) is still read, so existing
configs keep working. Run [`cfgd profile migrate`](cli-reference.md#cfgd-profile-migrate-name)
to move a flat profile into its bundle. If more than one form exists for one name, cfgd
fails closed rather than guess which wins: the error names every coexisting path; delete
or migrate all but one. The blast radius is scoped to the ambiguous profile itself: direct
operations on it (apply, switch, show, delete) fail, while unrelated operations (creating
or deleting other profiles, listing, workflow generation) warn about it and continue.

## Profile YAML

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - base
    - macos

  modules: [nvim, tmux, git, zsh]

  env:
    - name: EDITOR
      value: "code --wait"
    - name: GIT_AUTHOR_NAME
      value: "Jane Doe"
    - name: GIT_AUTHOR_EMAIL
      value: jane@work.com
    - name: color_theme
      value: gruvbox

  aliases:
    - name: vim
      command: nvim
    - name: ll
      command: ls -la
    - name: k
      command: kubectl

  packages:
    brew:
      taps:
        - homebrew/cask-fonts
      formulae:
        - git
        - ripgrep
        - fd
        - jq
        - kubectl
        - helm
      casks:
        - 1password
        - wezterm
        - visual-studio-code
    apt:
      packages:
        - build-essential
        - curl
    cargo:
      - bat
      - eza
      - cargo-watch
    npm:
      global:
        - typescript
        - prettier
    pipx:
      - httpie
      - ruff
    dnf:
      - gcc
      - make
    winget:
      - Microsoft.VisualStudioCode
      - Git.Git
    chocolatey:
      - nodejs
      - python
    scoop:
      - ripgrep
      - fd

  files:
    managed:
      - source: shell/.zshrc
        target: ~/.zshrc
      - source: git/.gitconfig.tera
        target: ~/.gitconfig
      - source: ssh/config
        target: ~/.ssh/config
    permissions:
      "~/.ssh/config": "600"
      "~/.ssh": "700"

  system:
    shell: /bin/zsh
    macosDefaults:
      NSGlobalDomain:
        AppleShowAllExtensions: true
      com.apple.dock:
        autohide: true
        tilesize: 48
    launchAgents:
      - name: com.example.myservice
        program: /usr/local/bin/myservice
        args: ["--config", "/etc/myservice.conf"]
        runAtLoad: true
    systemdUnits:
      - name: myservice.service
        unitFile: systemd/myservice.service
        enabled: true
    windowsRegistry:
      HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize:
        AppsUseLightTheme: 0
    windowsServices:
      - name: MyService
        startType: auto
        state: running

  secrets:
    - source: secrets/api-keys.yaml
      target: ~/.config/api-keys.yaml
    - source: 1password://Work/GitHub/token
      target: ~/.config/gh/token
      template: "token: ${secret:value}"

  scripts:
    preApply:
      - scripts/check-vpn.sh
    postApply:
      - scripts/reload-shell.sh
    preReconcile:
      - scripts/pre-setup.sh
    postReconcile:
      - scripts/post-setup.sh
    onDrift:
      - scripts/notify-slack.sh
    onChange:
      - scripts/rebuild-cache.sh
```

## Inheritance

Profiles can inherit from other profiles using `inherits`. cfgd processes the `inherits` list left-to-right, fully resolving each parent (and its parents) before moving to the next. The active profile is applied last, so it always wins on conflicts.

Given `work` inherits `[base, macos]` and `base` inherits `[core]`:

```
core → base → macos → work
 ↑      ↑      ↑       ↑
 │      │      │       └── active profile (wins on conflict)
 │      │      └── second parent
 │      └── first parent
 └── grandparent (resolved because base inherits it)
```

### Merge Rules

| Resource | Merge Strategy |
|---|---|
| `env` | Override — later profile replaces earlier for same name |
| `aliases` | Override — later profile replaces earlier for same name |
| `packages` | Union — all packages from all layers combined, deduplicated |
| `files.managed` | Overlay — later profile's file wins for same target path |
| `files.permissions` | Override — later profile replaces earlier for same path |
| `system` | Deep merge — later profile overrides at the leaf key level |
| `secrets` | Append — deduplicated by target path, later wins on conflict |
| `scripts` | Append — all scripts from all layers run in resolution order |
| `backups` | Append: deduplicated by `name`, later layer overrides |
| `modules` | Union — all modules from all layers combined, deduplicated |

## Env Vars

Env vars are name/value pairs available in [Tera templates](templates.md) and exported for the current user. They're set in the profile's `env` section and resolved through the inheritance chain (later overrides earlier for the same name).

cfgd writes a managed `~/.cfgd.env` and wires it into the user's shells and session managers according to **`spec.envScope`** (default `All`):

| `envScope` | Reaches |
|---|---|
| `All` *(default)* | Interactive + login shells, `systemd --user` / Wayland GUI (`~/.config/environment.d`), macOS GUI (LaunchAgent), and an immediate live-session refresh (`launchctl setenv` / `systemctl --user set-environment` / `setx`). No re-login needed. |
| `Login` | Interactive + login shells (`~/.zshenv`, `~/.profile`, and an existing `~/.bash_profile`). |
| `Interactive` | Interactive shells only (`~/.bashrc`/`~/.zshrc`, fish `conf.d`) — the historical behavior. |

`spec.env` is **per-user**. For system-wide (all-users, privileged) variables, use [`spec.system.environment`](system-configurators.md). See the [profile spec](spec/profile.md#specenvscope) for the full target list and the dotfile-safety rules.

The same file also carries every `PATH` entry **cfgd created for a package manager**, so a
profile with no `env` at all still gets a `~/.cfgd.env` and its source lines when cfgd installs
Homebrew, and `brew`'s binaries are reachable from the next shell without you editing a dotfile.
The test is who made the directory, not who installed the manager: a manager that was already on
the machine keeps its own locations untouched, while a prefix cfgd had to create for it (npm's
`$HOME/.npm-global`, when npm's own prefix is not writable) is exported like any other. cfgd
prints a re-source reminder, under the `cfgd:env` group of the closing **Caveats** section, after
any apply that touched either.

### Example: make `EDITOR` reach everywhere

```yaml
# profiles/envdemo/profile.yaml
spec:
  env:
    - name: EDITOR
      value: nvim
  # envScope omitted → defaults to All
```

```console
$ cfgd apply --yes
Apply
  Config   /home/you/.config/cfgd/cfgd.yaml
  Profile  envdemo
  Phases   Prerequisites
  Actions  6 planned

Phase: Prerequisites
  cfgd:env
    ✓ write /home/you/.cfgd.env                       — 1 var
    ✓ inject source line into /home/you/.bashrc
    ✓ inject source line into /home/you/.zshenv
    ✓ inject source line into /home/you/.profile
    ✓ write /home/you/.config/environment.d/cfgd.conf — 1 var
  cfgd:session
    ✓ publish 1 var to the session manager

✓ Apply complete — 6 actions succeeded (0.3s)

Caveats
  cfgd:env
    ⚠ run `source ~/.cfgd.env`, or open a new shell

# Now every entry point sees it, no re-login:
$ ssh localhost 'echo $EDITOR'            # non-interactive ssh command
nvim
$ bash -lc 'echo $EDITOR'                 # login shell
nvim
$ systemctl --user show-environment | grep EDITOR
EDITOR=nvim                                # systemd --user units + Wayland GUI
```

Each write states what went into that file, and only that file: `~/.cfgd.env` carries env
vars and aliases, while `environment.d` and the macOS LaunchAgent carry env vars alone.

A host with no session manager to publish to (a container, a Linux box without a systemd
user manager) still lists the `cfgd:session` row, with the reason in place of a result, and
prices it outside the run's count at both ends: the header's `Actions` row never promised
it, and the closing line names it only in its parenthetical:

```console
  cfgd:session
    ∅ publish 1 var to the session manager — no session manager

✓ Apply complete — 5 actions succeeded (1 not attempted — no session manager) (0.3s)
```

`-o json` carries the same split as `succeeded` / `skipped` / `notAttempted`; the stored
apply summary `cfgd log` reads back does too.

Every generated line names its owner, so a file holding entries from a profile chain,
several modules and a bootstrapped package manager says where each came from:

```bash
# managed by cfgd — do not edit
export PATH="/home/linuxbrew/.linuxbrew/bin:$PATH" # manager:brew
export PAGER="less" # profile:base
export EDITOR="nvim" # module:nvim
alias v="nvim" # module:nvim
alias catn="cat -n" # profile:base
```

The owner is the layer whose value survived the merge, so an entry a child profile
overrides names the child, not the base it came from. A subscribed source's entries name
the source (`# source:acme`), and the bootstrapped `PATH` line names every manager whose
directories it carries (`# manager:brew,cargo`).

A file carries exactly **one** `PATH` line, whoever produced it. Declaring `PATH` in
`spec.env` does not add a second: the declaration and the bootstrapped directories fold
into one assignment whose comment names both producers, with cfgd's directories spliced
in where the declaration reaches for the ambient `PATH`.

```bash
# module:nvim declares PATH: "$HOME/.cargo/bin:$PATH"; brew and npm were bootstrapped
export PATH="$HOME/.cargo/bin:/opt/brewroot/bin:$HOME/.npm-global/bin:$PATH" # manager:brew,npm module:nvim
```

`environment.d` and the macOS LaunchAgent have no trailing-comment grammar, so their lines
carry no owner.

The two owner groups separate what is durable from what is not: `cfgd:env` writes the files
a future shell reads, `cfgd:session` pushes the same values into the session manager you are
already logged into. A host with no live user session reports that group's action as
unchanged and carries the reason as a warning under it; the files are still correct.

To opt out of the broader surfaces, narrow the scope: `envScope: Interactive` restores the
classic "interactive shells only" behavior, writing only `~/.cfgd.env` + the `~/.bashrc`/`~/.zshrc`
source line.

## Shell Aliases

Shell aliases are name/command pairs written to `~/.cfgd.env` alongside env exports. They follow the same merge rules as env vars: later profile overrides earlier for the same name, and module aliases win over profile aliases on conflict.

```yaml
spec:
  aliases:
    - name: vim
      command: nvim
    - name: ll
      command: ls -la
```

For bash/zsh, aliases are written as `alias name="command"`. For fish, they're written as `abbr -a name command` to `~/.config/fish/conf.d/cfgd-env.fish`.

## CLI Commands

```sh
cfgd profile list                  # list available profiles
cfgd profile show                  # show resolved profile (all layers merged)
cfgd profile switch work           # switch active profile
cfgd profile create dev            # create a new profile (interactive or with flags)
cfgd profile update dev --package brew:ripgrep  # modify a profile
cfgd profile edit dev              # open in $EDITOR with validation
cfgd profile delete dev            # delete (refuses if active or inherited)
```

### Creating Profiles via CLI

```sh
cfgd profile create work-linux \
  --inherit base \
  --module nvim --module tmux \
  --package apt:build-essential \
  --env EDITOR=vim \
  --alias vim=nvim \
  --file ~/.config/starship.toml
```

### Updating Profiles via CLI

![authoring a profile from the CLI](../demo/cfgd-author.gif)
*Explain a field, add a package and an alias, preview, then converge: no editor needed.*

```sh
cfgd profile update work \
  --module git \
  --module -old-tool \
  --package brew:jq \
  --package -brew:unused \
  --env GIT_AUTHOR_NAME="Jane Doe" \
  --alias k=kubectl \
  --file ~/.bashrc \
  --file --private-files ~/.config/secret.conf
```

Prefix a value with `-` to remove it (e.g. `--module -old-tool` removes `old-tool`).

When no profile name is given, `profile update` defaults to the active profile from cfgd.yaml, so `cfgd profile update --file ~/.zshrc` is equivalent to `cfgd add ~/.zshrc`.

## The `modules` Field

Profiles declare which modules to use via the `modules` list. Module packages and profile-level packages coexist. If the same package appears in both, the module's version constraint and preference are authoritative.

Use modules for portable, shareable tool setups (nvim, tmux, a complete dev environment). Use profile-level packages for machine-specific one-off installs that don't need to be shared or cross-platform.

```yaml
spec:
  modules: [nvim, tmux, git, zsh]
  packages:
    brew:
      formulae: [extra-tool]  # profile-level, alongside module packages
```

See [modules.md](modules.md) for usage and the [Module spec reference](spec/module.md) for field details.
