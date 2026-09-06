# Tera Templates

Files with a `.tera` extension are rendered through the [Tera](https://keats.github.io/tera/) template engine before being placed at their target. The `.tera` extension is stripped from the target filename. A source without the extension can be rendered too, by declaring `strategy: Template` on the file entry.

## Template Context

Available as top-level variables in all templates:

| Variable | Source | Description |
|---|---|---|
| All profile `env` vars | Profile spec | Name/value pairs from the resolved profile's `env` section |
| `__os` | System | Operating system (`linux`, `macos`, `freebsd`, `windows`) |
| `__arch` | System | Architecture (`x86_64`, `aarch64`) |
| `__hostname` | System | Machine hostname |
| `__distro` | System | Linux distribution / pseudo-distro (`ubuntu`, `debian`, `fedora`, `rhel`, `centos`, `arch`, `manjaro`, `alpine`, `opensuse`, `macos`, `freebsd`, `windows`, `unknown`) |

## Custom Functions

| Function | Description | Example |
|---|---|---|
| `os()` | Returns the OS name | `{% if os() == "macos" %}` |
| `hostname()` | Returns the hostname | `{{ hostname() }}` |
| `arch()` | Returns the architecture | `{{ arch() }}` |
| `env(name="VAR")` | Reads an environment variable; an unset variable yields an empty string | `{{ env(name="HOME") }}` |

## Secret References

`${secret:ref}` placeholders in the rendered content are resolved at apply time, where `ref` is an [external provider reference](secrets.md#external-providers) or a path to a SOPS-encrypted file relative to the config dir. This works in any file deployed with `Copy` or `Template`; the resolved value is written to the target, never to the repo.

```ini
# git/.gitconfig.tera
[github]
    token = ${secret:1password://Work/GitHub/token}
```

## Example: `.gitconfig.tera`

```ini
[user]
    name = {{ GIT_AUTHOR_NAME }}
    email = {{ GIT_AUTHOR_EMAIL }}

[core]
    editor = {{ EDITOR }}

{% if __os == "macos" %}
[credential]
    helper = osxkeychain
{% endif %}
```

## Example: `.zshrc.tera`

```zsh
export EDITOR="{{ EDITOR }}"
export PATH="$HOME/.local/bin:$PATH"

{% if __os == "linux" %}
alias open="xdg-open"
{% endif %}

{% if __arch == "aarch64" %}
eval "$(/opt/homebrew/bin/brew shellenv)"
{% else %}
eval "$(/usr/local/bin/brew shellenv)"
{% endif %}
```

## Usage in Profiles

Template files are auto-detected by the `.tera` extension. No configuration needed beyond declaring the file:

```yaml
files:
  managed:
    - source: git/.gitconfig.tera    # rendered through Tera
      target: ~/.gitconfig           # .tera stripped from target
    - source: shell/.zshrc           # plain copy, no templating
      target: ~/.zshrc
```

## Usage in Modules

Module files work the same way. Templates in module file sources are rendered with the same context (profile env vars + system facts).

## Failure Behavior

A template that fails to parse or render aborts the plan or apply with a typed error naming the file and the underlying Tera cause (including the offending variable or syntax location). Nothing is written to the target.

## Env Var Sandboxing with Sources

Templates delivered by a [config source](sources.md) run sandboxed: they see only that source's provided env vars plus the system facts, never your local env vars. This prevents data exfiltration through templates. Concretely:

- `env()` is blocked; calling it fails the render with a sandbox-restriction error.
- Referencing a variable outside the source's own set fails with a sandbox-violation error naming the source and the variable.
- Tera `include` / `extends` resolve only among templates from the same origin, so a source template cannot inherit from a local one (or the reverse).

See the [CLI reference](cli-reference.md) for `cfgd profile update --file` and `cfgd module update --file` commands.
