# Lifecycle Scripts

Lifecycle scripts run shell commands at defined points during apply and reconciliation. They are
declared in `spec.scripts` on both profiles and modules. For the full field reference, see the
[Module spec](spec/module.md#specscripts) and [Profile spec](spec/profile.md#specscripts).

## Shell Selection

The `shell` field controls which interpreter runs an inline command. Valid values: `bash`, `zsh`,
`sh`, `pwsh`, `cmd`, `auto`. Default is `auto` (`sh` on Unix, `cmd.exe` on Windows).

```yaml
scripts:
  postApply:
    # Simple form — default shell (sh on Unix, cmd.exe on Windows)
    - echo "done"

    # Explicit bash for bash-specific features
    - run: echo "BASH_VERSION=$BASH_VERSION"
      shell: bash

    # Explicit zsh
    - run: echo "ZSH_VERSION=$ZSH_VERSION"
      shell: zsh
```

`shell` only applies to inline commands (the `run:` string or simple string form). File scripts
(paths that resolve to an existing file) use their shebang and ignore `shell`.

## Env and Alias Availability

When `shell` is `bash` or `zsh`, the script automatically sources `~/.cfgd.env` before your
command runs. This file contains all resolved `spec.env` variables and `spec.aliases` declarations.
Alias expansion is enabled (`shopt -s expand_aliases` for bash, `setopt aliases` for zsh).

```yaml
spec:
  env:
    - name: EDITOR
      value: nvim
  aliases:
    - name: vim
      command: nvim

  scripts:
    postApply:
      # shell: bash — all env vars AND aliases from ~/.cfgd.env are available
      - run: vim --headless "+Lazy! sync" +qa
        shell: bash

      # Default (sh) — spec.env vars are injected directly into the environment,
      # but aliases are not available (POSIX sh has no alias expansion in -c mode)
      - echo $EDITOR
```

With the default shell (`sh`), `spec.env` variables are passed via direct environment injection.
Aliases require `bash` or `zsh` because they depend on `~/.cfgd.env` sourcing.

### Value expansion

A leading `~` (and a `~` following a `:`, for `PATH`-style lists) in a `spec.env` **value** expands
to your home directory. This is necessary because the managed env file quotes values, so the shell
never performs tilde expansion itself — a literal `~/.local/bin` would be a broken path.

```yaml
spec:
  env:
    - name: CLIFT_DIR
      value: ~/.local/share/clift     # → /home/you/.local/share/clift
    - name: PATH
      value: ~/bin:$PATH              # ~ expands now; $PATH expands when the file is sourced
```

`$VAR` / `${VAR}` references are left intact in the bash/zsh env file and expand when it is sourced
(so `$PATH` always references the live PATH). For scripts run under the default `sh`, where there is
no file to source, `$VAR` references are resolved at injection time against the process environment
plus earlier `spec.env` entries (fold-left, like a shell).

## Reserved Env Var Names

Env var names starting with `CFGD_` are reserved for cfgd internal use and rejected at config
parse time. `BASH_ENV` and `ZDOTDIR` are also reserved (cfgd uses these to control shell sourcing
behavior).

```yaml
spec:
  env:
    # All three are rejected at parse time with an error:
    - name: CFGD_FOO        # Error: CFGD_* prefix is reserved
      value: bar
    - name: BASH_ENV         # Error: BASH_ENV is reserved
      value: /some/path
    - name: ZDOTDIR          # Error: ZDOTDIR is reserved
      value: /some/path
```

## Working Directory

Every lifecycle script runs in **your home directory** by default — never in the config source
tree. This keeps a relative write (`touch .installed`, `git init`, `> build.log`) out of your
version-controlled cfgd config repo. Scripts reach their module's bundled assets and the config
root through the injected `$CFGD_MODULE_DIR` / `$CFGD_CONFIG_DIR` variables (see below), so the
source directory never needs to be the working directory.

Set `workdir` on a full-form script to run it somewhere else. A leading `~` expands to the home
directory, and `$VAR` / `${VAR}` expand against the script environment (including the injected
`CFGD_*` variables):

```yaml
scripts:
  postApply:
    # Default: runs in $HOME — `.cfgd-managed` lands in the deploy dir below
    - run: touch .cfgd-managed
      workdir: ~/.local/share/clift

    # Run inside the module's own checked-out directory
    - run: ./install.sh
      workdir: $CFGD_MODULE_DIR

    # Absolute path
    - run: make build
      workdir: /opt/build
```

A relative `workdir` is resolved against `$HOME`. A `workdir` whose directory does not exist is a
script error (it names the offending path), not a silent fallback.

## Injected Variables

cfgd injects these read-only variables into every lifecycle script's environment. They are reserved
(you cannot set them via `spec.env`) and are the supported way to reach paths from a script:

| Variable | Value |
|----------|-------|
| `CFGD_CONFIG_DIR` | Absolute path to the config root |
| `CFGD_PROFILE` | Active profile name |
| `CFGD_CONTEXT` | `apply` or `reconcile` |
| `CFGD_PHASE` | The phase being run (`preApply`, `postApply`, `onChange`, `onDrift`, …) |
| `CFGD_MODULE_NAME` | Module name (module scripts only) |
| `CFGD_MODULE_DIR` | Absolute path to the module's directory (module scripts only) |

## File Scripts vs Inline

| Aspect | Inline (`run:` string) | File (path to script) |
|--------|------------------------|----------------------|
| Interpreter | Selected by `shell` field | Selected by shebang (`#!/bin/bash`, etc.) |
| `~/.cfgd.env` sourcing | Automatic when `shell: bash` or `shell: zsh` | Manual: add `source ~/.cfgd.env` in script body |
| `spec.env` vars | Injected into environment | Injected into environment |
| Aliases | Available with `bash`/`zsh` via auto-sourcing | Available only if script sources `~/.cfgd.env` |

For file scripts that need aliases, source the env file explicitly:

```bash
#!/usr/bin/env bash
source ~/.cfgd.env
vim --headless "+Lazy! sync" +qa
```
