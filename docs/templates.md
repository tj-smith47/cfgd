# Tera Templates

Files with a `.tera` extension are rendered through the [Tera](https://keats.github.io/tera/) template engine before being placed at their target. The `.tera` extension is stripped from the target filename.

## Template Context

Available as top-level variables in all templates:

| Variable | Source | Description |
|---|---|---|
| All profile `env` vars | Profile spec | Name/value pairs from the resolved profile's `env` section |
| `__os` | System | Operating system (`linux`, `macos`, `freebsd`) |
| `__arch` | System | Architecture (`x86_64`, `aarch64`) |
| `__hostname` | System | Machine hostname |

## Custom Functions

| Function | Description | Example |
|---|---|---|
| `os()` | Returns the OS name | `{% if os() == "macos" %}` |
| `hostname()` | Returns the hostname | `{{ hostname() }}` |
| `arch()` | Returns the architecture | `{{ arch() }}` |
| `env(name="VAR")` | Reads an environment variable | `{{ env(name="HOME") }}` |

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

## Env Var Sandboxing with Sources

When using [multi-source config](sources.md), templates from a config source can only access that source's provided env vars and system facts — not your personal env vars. This prevents data exfiltration through templates.
