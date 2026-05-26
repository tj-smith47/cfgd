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
