# Windows Support

Make cfgd CLI fully functional on Windows. The operator remains Linux-only (Kubernetes nodes). The CLI should support file management, package installation (winget, chocolatey, scoop), env configuration (PowerShell), template rendering, modules, drift detection, and the daemon (as a Windows Service).

## What already works

The core engine is cross-platform Rust with no Unix deps:
- Config loading, parsing, profile resolution, layer merging
- Template rendering (Tera)
- Module system (loading, dependency resolution, package resolution)
- State store (SQLite via rusqlite)
- Git operations (git2)
- Reconciler planning and diffing
- Platform detection (`platform/mod.rs` — already detects OS/arch)

## Phase 1: Compile on Windows

Gate all Unix-specific code behind `#[cfg(unix)]` so the crate compiles on Windows.

- [ ] `crates/cfgd/src/files/mod.rs`: gate `use std::os::unix::fs::PermissionsExt` and all `set_permissions` / `mode()` calls behind `#[cfg(unix)]`
- [ ] `crates/cfgd/src/files/mod.rs`: gate `std::os::unix::fs::symlink` behind `#[cfg(unix)]`; on Windows, symlink/hardlink strategies fall back to copy
- [ ] `crates/cfgd/src/files/mod.rs`: gate `MetadataExt` usage in tests behind `#[cfg(unix)]`
- [ ] `crates/cfgd/src/system/node.rs`: gate entire module behind `#[cfg(unix)]` (sysctl, systemd, containerd, kubelet, apparmor, seccomp, certificates are Linux-only)
- [ ] `crates/cfgd/src/system/mod.rs`: `ShellConfigurator` — gate `chsh` behind `#[cfg(unix)]`
- [ ] `crates/cfgd/src/system/mod.rs`: `EnvironmentConfigurator` — gate Linux/macOS paths behind `#[cfg(unix)]`, add `#[cfg(windows)]` path using `setx`
- [ ] `crates/cfgd-core/src/reconciler/mod.rs`: env file generation — gate bash/zsh/fish behind `#[cfg(unix)]`, add `#[cfg(windows)]` PowerShell path
- [ ] `crates/cfgd-core/src/upgrade.rs`: gate `#[cfg(unix)]` permission bits on downloaded binary
- [ ] `crates/cfgd-core/src/server_client.rs`: gate `#[cfg(unix)]` permission calls
- [ ] `crates/cfgd-core/src/daemon/mod.rs`: gate Unix signal handling; Windows daemon is Phase 5
- [ ] CI: add `runs-on: windows-latest` job to `ci.yml` — build + test (skip `#[cfg(unix)]` tests)

## Phase 2: Windows file management

- [ ] `FileStrategy::Symlink` on Windows: attempt `std::os::windows::fs::symlink_file` / `symlink_dir`; if permission denied (no Developer Mode), warn and fall back to copy
- [ ] `FileStrategy::Hardlink` on Windows: `std::fs::hard_link` works cross-platform — just gate the Unix permission bits
- [ ] File permissions: skip `chmod`-style permission setting on Windows (NTFS ACLs are a different model; out of scope)
- [ ] Config schema: `permissions` map in `FilesSpec` is ignored on Windows with an info-level log

## Phase 3: Windows package managers

Three new `PackageManager` trait implementations in `crates/cfgd/src/packages/`:

### winget (`packages/winget.rs`)

- [ ] `name()` → `"winget"`
- [ ] `available()` → `command_available("winget")`
- [ ] `installed_packages()` → parse `winget list --source winget` output
- [ ] `install(packages)` → `winget install --id <pkg> --accept-package-agreements --accept-source-agreements`
- [ ] `uninstall(packages)` → `winget uninstall --id <pkg>`
- [ ] Bootstrap: winget ships with Windows 11 and App Installer on Windows 10. No bootstrap needed; error if missing.

### chocolatey (`packages/chocolatey.rs`)

- [ ] `name()` → `"chocolatey"`
- [ ] `available()` → `command_available("choco")`
- [ ] `installed_packages()` → parse `choco list --local-only` output
- [ ] `install(packages)` → `choco install <pkg> -y`
- [ ] `uninstall(packages)` → `choco uninstall <pkg> -y`
- [ ] Bootstrap: `PackageAction::Bootstrap` with PowerShell one-liner from chocolatey.org

### scoop (`packages/scoop.rs`)

- [ ] `name()` → `"scoop"`
- [ ] `available()` → `command_available("scoop")`
- [ ] `installed_packages()` → parse `scoop list` output (or read `~/scoop/apps/` directory)
- [ ] `install(packages)` → `scoop install <pkg>`
- [ ] `uninstall(packages)` → `scoop uninstall <pkg>`
- [ ] Bootstrap: `PackageAction::Bootstrap` with PowerShell `irm get.scoop.sh | iex`

### Config format

```yaml
spec:
  packages:
    winget:
      - Microsoft.VisualStudioCode
      - Git.Git
    chocolatey:
      - nodejs
      - python
    scoop:
      - ripgrep
      - fd
```

### Cross-platform module resolution

`platform/mod.rs` already maps canonical package names to per-manager names. Add Windows manager entries:

```yaml
# module.yaml
packages:
  - name: ripgrep
    aliases:
      winget: BurntSushi.ripgrep.MSVC
      scoop: ripgrep
      chocolatey: ripgrep
```

- [ ] Register winget, chocolatey, scoop in `ProviderRegistry` when `cfg(windows)` or when platform detection finds Windows
- [ ] Add `PackagesSpec` fields: `winget: Vec<String>`, `chocolatey: Vec<String>`, `scoop: ScoopSpec` (with buckets support)
- [ ] Add config schema and JSON schema entries
- [ ] Update `desired_packages_for()` to handle new managers

## Phase 4: Windows env and shell integration

### PowerShell profile

Instead of `~/.cfgd.env`, generate a PowerShell script sourced by `$PROFILE`:

**`~/.cfgd-env.ps1`:**
```powershell
# managed by cfgd — do not edit
$env:EDITOR = "code"
$env:PATH = "C:\Users\user\.cargo\bin;$env:PATH"
```

- [ ] `generate_powershell_env_content()` — `$env:NAME = "value"` syntax
- [ ] Shell detection on Windows: check `$SHELL` (WSL/Git Bash) first, fall back to PowerShell
- [ ] PowerShell profile injection: append `. ~/.cfgd-env.ps1` to `$PROFILE` (typically `~\Documents\PowerShell\Microsoft.PowerShell_profile.ps1`)
- [ ] Idempotent: check for `cfgd-env` in profile before appending
- [ ] `plan_env()` and `verify_env()`: `#[cfg(windows)]` paths for PowerShell

### System environment variables

`EnvironmentConfigurator` on Windows:

- [ ] Read desired vars from `spec.system.environment`
- [ ] Set via `setx NAME "value"` (persists to registry, available to new processes)
- [ ] Current state: read from registry `HKCU\Environment` or `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`
- [ ] Diff and apply like the Linux/macOS implementation

## Phase 5: Windows daemon

Run cfgd as a Windows Service for continuous reconciliation.

- [ ] Add `windows-service` crate dependency (behind `#[cfg(windows)]`)
- [ ] Implement service entry point: register with SCM, handle start/stop/pause
- [ ] `cfgd daemon install` — register the Windows Service (`sc.exe create` or `windows-service` API)
- [ ] `cfgd daemon uninstall` — remove the Windows Service
- [ ] `cfgd daemon start` / `cfgd daemon stop` — control via SCM
- [ ] Same reconciliation loop as Unix daemon, different lifecycle wrapper
- [ ] Event Log integration instead of syslog

## Phase 6: CI and release

- [ ] CI: Windows test job (`runs-on: windows-latest`) in `ci.yml`
- [ ] Release workflow: cross-compile `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` targets
- [ ] Release artifacts: `cfgd-{version}-windows-x86_64.zip` (zip not tar.gz for Windows)
- [ ] Update docs: installation instructions for Windows, PowerShell examples
- [ ] Update `docs/packages.md` with winget/chocolatey/scoop documentation
