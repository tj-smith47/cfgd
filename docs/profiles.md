# Profiles

Profiles declare the desired state of a machine. They can inherit from other profiles to share common configuration.

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

  secrets:
    - source: secrets/api-keys.yaml
      target: ~/.config/api-keys.yaml
    - source: 1password://Work/GitHub/token
      target: ~/.config/gh/token
      template: "token: ${secret:value}"

  scripts:
    preReconcile:
      - scripts/pre-setup.sh
    postReconcile:
      - scripts/post-setup.sh
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
| `modules` | Union — all modules from all layers combined, deduplicated |

## Env Vars

Env vars are name/value pairs available in [Tera templates](templates.md) and exported to the shell environment. They're set in the profile's `env` section and resolved through the inheritance chain (later overrides earlier for the same name).

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

See [modules.md](modules.md) for the module spec.
