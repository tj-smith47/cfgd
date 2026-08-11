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
| `CFGD_PHASE` | The phase being run (`preApply`, `postApply`, `preReconcile`, `postReconcile`, `onChange`, `onDrift`) |
| `CFGD_MODULE_NAME` | Module name (module scripts only) |
| `CFGD_MODULE_DIR` | Absolute path to the module's directory (module scripts only) |

The same variables reach a [`strategy: Patch` filter script](configuration.md#script--pipe-the-file-through-a-command),
which runs with `CFGD_PHASE=patch`.

## PATH for a Manager cfgd Just Installed

When cfgd bootstraps a package manager (Homebrew, an npm global prefix), that manager's
`bin` directory is not on the PATH of the shell that launched `cfgd` — it did not exist
when the shell started. cfgd prepends the recorded directories to every lifecycle
script's PATH, so a `postApply` step can call a binary the same apply just installed:

```yaml
spec:
  packages:
  - name: neovim
    prefer: [brew]           # brew installs to /home/linuxbrew/.linuxbrew/bin
  scripts:
    postApply:
    - run: nvim --headless "+Lazy! restore" +qa!   # resolves without any export
```

The directories land *ahead* of the inherited PATH, matching what the generated
`~/.cfgd.env` writes for the login shell that follows, so a script and the shell resolve
a command the same way. A module's own `spec.env` PATH is layered on top of that merged
value, so `PATH: $HOME/.local/bin:$PATH` keeps the bootstrapped entries rather than
dropping them.

Only a manager **cfgd itself bootstrapped** contributes here. A Homebrew the user
installed is already on their PATH and is recorded nowhere.

## Interactive Scripts

Set `interactive: true` on a script entry that needs to prompt the user — for
example, pausing until a manual step is done. The script runs **attached to
the terminal** (inherited stdin/stdout/stderr, no spinner, no output capture)
and is **not** subject to the idle timeout, because an interactive step is
attended by definition.

```yaml
scripts:
  postApply:
    - run: |
        echo "Install Azure VPN from Self Service, then press Enter"
        read
      interactive: true
```

An interactive script requires a TTY. When stdin is **not** a terminal — CI,
piped input, or any run by `cfgd daemon` (the daemon never has a TTY) — the
script is **skipped with a warning** rather than hanging on instant EOF, and
reports `changed=false`. Interactive steps therefore run only during an
attended `cfgd apply`, never under unattended reconcile.

**Process group.** The child shares cfgd's own process group instead of
getting a new detached one, so the terminal's foreground group still
includes it: a Ctrl-C typed at the terminal reaches the script directly, and
a raw-mode TUI or a `sudo` password prompt behaves normally. Every
non-interactive script still gets its own detached process group
(`process_group(0)`) — `interactive: true` is the one opt-out, because
sharing a group is only safe when a human is attending the terminal and
expects Ctrl-C to reach the step they're watching.

**Timeout.** By default an interactive script has **no timeout at all** —
force-killing a step that's mid-raw-mode or waiting on a password prompt
would be worse than an unbounded wait, and there's no safe generic
idle-timeout heuristic for an interactive program. Set `timeout:` on the
entry when a step does need a ceiling; once it elapses cfgd terminates the
script (SIGTERM, then SIGKILL after a grace period), by direct-PID kill
rather than a process-group kill, since the child shares cfgd's group and is
no longer a group leader of its own.

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

### How `run:` picks File vs Inline

cfgd tests `run:` against the filesystem to decide which column above applies —
there is no separate `file:` field:

1. The **whole string** is tried first. If it names a file (relative to the
   config directory, or absolute), that file runs directly: no shell, args
   splitting, or interpretation — the shebang alone selects the interpreter.

   ```yaml
   scripts:
     postApply:
       - run: scripts/deploy.sh   # exact match → direct exec, no shell
   ```

2. Otherwise, if the string contains whitespace, only the **leading token**
   (everything up to the first space) is tried the same way. A match there is
   still the Inline column — the shell runs it — but the leading token is
   resolved to its absolute path and substituted back in before the shell
   sees it, so the resolution doesn't depend on the shell's own `cwd`. Every
   byte after the leading token is untouched and reaches the shell verbatim:
   metacharacters, quoting, and `&&` chains all behave exactly as if you'd
   typed the resolved path yourself.

   ```yaml
   scripts:
     postApply:
       - run: scripts/deploy.sh --env prod && echo done
       # → '/abs/path/to/scripts/deploy.sh' --env prod && echo done
   ```

3. A leading token that doesn't resolve to a real file (an ordinary command
   name, or a reference like `$CFGD_CONFIG_DIR/deploy.sh` that only becomes a
   real path once the shell expands it) is left completely untouched — cfgd
   never guesses, it only substitutes a resolution it can prove.

File resolution in both steps is always relative to the **config
directory**, and is absolute regardless of how `--config` was spelled on the
command line (a relative `--config ./cfgd.yaml` resolves identically to an
absolute one) — never relative to the process's invocation directory or to
`$HOME`, which is the script's own default [working directory](#working-directory).
