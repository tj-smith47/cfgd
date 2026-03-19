# Package Managers

cfgd manages packages across 15 package managers (Homebrew manages taps, formulae, and casks as separate sub-managers). Each is implemented behind a trait, so the reconciler works the same way regardless of which managers are available. You can also define custom script-based managers for tools that don't fit any built-in manager.

## Supported Managers

| Manager | Platforms | Config Key | What It Does |
|---|---|---|---|
| Homebrew | macOS, Linux | `brew` | Manages taps, formulae, and casks separately |
| apt | Debian/Ubuntu | `apt` | `apt-get install` with sudo handling |
| dnf | Fedora/RHEL 8+ | `dnf` | `dnf install` |
| yum | RHEL 7/CentOS 7 | `yum` | `yum install` |
| pacman | Arch/Manjaro | `pacman` | `pacman -S` |
| apk | Alpine | `apk` | `apk add` |
| zypper | OpenSUSE | `zypper` | `zypper install` |
| pkg | FreeBSD | `pkg` | `pkg install` |
| Cargo | Any (with Rust) | `cargo` | `cargo install` |
| npm | Any (with Node) | `npm` | `npm install -g` |
| pipx | Any (with Python) | `pipx` | `pipx install` |
| Snap | Linux (with snapd) | `snap` | `snap install` |
| Flatpak | Linux (with flatpak) | `flatpak` | `flatpak install` |
| Nix | Any (with Nix) | `nix` | `nix profile install` |
| Go | Any (with Go) | `go` | `go install` |

Package managers that aren't installed on the current system are silently skipped. `cfgd apply --dry-run` shows which managers will be used and which packages will be installed or removed.

## Profile Usage

```yaml
packages:
  brew:
    taps:
      - homebrew/cask-fonts
    formulae:
      - git
      - ripgrep
    casks:
      - visual-studio-code
  apt:
    packages:
      - build-essential
      - curl
  cargo:
    - bat
    - eza
  npm:
    global:
      - typescript
  pipx:
    - httpie
  dnf:
    - gcc
```

## Module Packages

In [modules](modules.md), packages use cross-platform resolution instead of manager-specific lists:

```yaml
packages:
  - name: neovim
    minVersion: "0.9"
    prefer: [brew, snap, apt]
    aliases:
      snap: nvim
```

cfgd picks the first available manager that satisfies the version constraint, using `aliases` to map package names where they differ.

## Version Queries

Each manager supports querying available package versions without installing:

| Manager | How version is queried |
|---|---|
| apt | `apt-cache policy <pkg>` — Candidate line |
| brew | `brew info --json=v2 <pkg>` — stable version |
| dnf | `dnf info <pkg>` — Version field |
| pacman | `pacman -Si <pkg>` — Version field |
| apk | `apk policy <pkg>` |
| snap | `snap info <pkg>` — latest/stable channel |
| npm | `npm view <pkg> version` |
| pipx | PyPI JSON API |
| cargo | `cargo search <pkg> --limit 1` |

## Dry Run

`cfgd apply --dry-run` shows the full package plan without making changes:

```
Packages:
  + brew install ripgrep fd bat
  - brew uninstall unused-tool
  = apt: 5 packages up to date
  ⊘ snap: not installed (skipping)
```

See the [CLI reference](cli-reference.md) for `cfgd profile update --package` and `cfgd module update --package` commands.
