# Windows Support — Design Spec

Make `cfgd` CLI and `cfgd-core` fully functional on Windows. Every feature that works on Unix has a real, working Windows counterpart where the platform concept exists. No stubs, no deferred work, no "unsupported" messages.

**Out of scope:** `cfgd-operator` and `cfgd-csi` are Kubernetes infrastructure that runs on Linux nodes. No Windows support needed — excluded from the Windows CI build via explicit `-p` flags.

## Platform Abstraction Layer

Cross-platform primitives in `cfgd-core/src/lib.rs` (alongside existing shared utilities). Each has `#[cfg(unix)]` and `#[cfg(windows)]` implementations. All callsites use these — no scattered `#[cfg]` blocks elsewhere.

**Dependency:** `windows-sys` crate added to `cfgd-core/Cargo.toml` and `cfgd/Cargo.toml` under `[target.'cfg(windows)'.dependencies]` with feature flags: `Win32_Storage_FileSystem`, `Win32_System_Threading`, `Win32_Security`, `Win32_Foundation`, `Win32_UI_Shell` (for `IsUserAnAdmin`), `Win32_System_Registry` (for registry operations).

### Home directory — `expand_tilde(path)`

The existing `expand_tilde()` in `lib.rs` uses `HOME` env var, which is not standard on Windows (`USERPROFILE` is the Windows equivalent). The `#[cfg(windows)]` path checks `USERPROFILE` first, then `HOME` as fallback (for WSL/Git Bash contexts).

### Symlinks — `create_symlink(src, dst)`

- **Unix:** `std::os::unix::fs::symlink`
- **Windows:** `std::os::windows::fs::symlink_file` / `symlink_dir` (chosen by `src.is_dir()`)
- On permission denied: error with clear message explaining Developer Mode or admin elevation is needed
- **Callsites:** `files/mod.rs`, `reconciler/mod.rs` (5 sites), `cli/mod.rs`, `cli/profile.rs`

### File permissions — `file_permissions_mode(metadata) -> Option<u32>`

- **Unix:** `PermissionsExt::mode() & 0o777`
- **Windows:** returns `None` (NTFS has no mode bits; ACLs are inherited from parent)
- When config parsing encounters a `permissions` field on Windows, log `info!` explaining NTFS uses inherited ACLs
- **Callsites:** `files/mod.rs` (file tree building), `reconciler/mod.rs` (planning)

### Set file permissions — `set_file_permissions(path, mode)`

- **Unix:** `Permissions::from_mode(mode)` + `set_permissions`
- **Windows:** no-op (correct NTFS behavior — inheritance handles it). `tracing::debug!` only.
- **Callsites:** `reconciler/mod.rs` (SetPermissions action), `secrets/mod.rs`, `server_client.rs`, `upgrade.rs`, `cli/module.rs`, `gateway/api.rs`

### Executable check — `is_executable(metadata) -> bool`

- **Unix:** `mode() & 0o111 != 0`
- **Windows:** checks file extension against known-executable set (`.exe`, `.cmd`, `.bat`, `.ps1`, `.com`)
- **Callsite:** `reconciler/mod.rs` (script execution)

### Inode comparison — `is_same_inode(a, b) -> bool`

- **Unix:** `MetadataExt` — compare `ino()` and `dev()`
- **Windows:** `GetFileInformationByHandle` — compare `nFileIndexHigh`/`nFileIndexLow` and `dwVolumeSerialNumber`
- **Callsite:** `files/mod.rs`

### File locking — `acquire_apply_lock(state_dir) -> Result<ApplyLockGuard>`

- **Unix:** `libc::flock(fd, LOCK_EX | LOCK_NB)`, released on drop (fd close)
- **Windows:** `LockFileEx` with `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`, released via `UnlockFileEx` on drop
- PID written to lock file on both platforms
- **Callsite:** `lib.rs` (already exists, extend with `#[cfg(windows)]`)

### Process termination — `terminate_process(pid: u32)`

- **Unix:** `libc::kill(pid as pid_t, SIGTERM)`
- **Windows:** `OpenProcess(PROCESS_TERMINATE, pid)` + `TerminateProcess`
- **Callsites:** `upgrade.rs` (daemon restart), `reconciler/mod.rs` (script timeout)

### Privilege check — `is_root() -> bool`

- **Unix:** `libc::geteuid() == 0`
- **Windows:** `shell32::IsUserAnAdmin()` via `windows-sys`
- **Callsite:** `packages/mod.rs` (relocate from inline in `packages/mod.rs` to `lib.rs` as a shared platform primitive)

## Daemon IPC

The daemon uses line-delimited JSON over a stream connection for CLI-daemon communication.

### Abstraction in `cfgd-core/src/daemon/mod.rs`

```
default_ipc_path() -> String
  Unix: "/tmp/cfgd.sock"
  Windows: "\\.\pipe\cfgd"

bind_ipc(path) -> Result<IpcListener>       // server side
  Unix: tokio::net::UnixListener
  Windows: tokio::net::windows::named_pipe::ServerOptions

connect_ipc_sync(path) -> Result<IpcStream>  // client side
  Unix: std::os::unix::net::UnixStream
  Windows: std::fs::OpenOptions on named pipe path
```

Tokio's Windows named pipe requires re-creating the pipe instance after each client connects. The `IpcListener` abstraction normalizes this into the same `accept()` loop pattern as `UnixListener`.

The JSON protocol, `run_health_server()`, and `query_daemon_status()` are unchanged — they operate on the abstracted stream types.

## Script Execution

Three platform-specific behaviors in `reconciler/mod.rs` `execute_script()`, handled with inline `#[cfg]` blocks (single callsite, no abstraction needed):

1. **Executable bit check:** Unix checks `mode() & 0o111`. Windows checks file extension (`.exe`, `.cmd`, `.bat`, `.ps1`). Uses `is_executable()` from the platform layer.

2. **Inline command shell:** Unix passes to `sh -c`. Windows passes to `cmd.exe /C`.

3. **Timeout termination:** Unix sends `SIGTERM` then `kill()` after 5s. Windows calls `TerminateProcess` (no graceful signal equivalent for arbitrary processes). Uses `terminate_process()` from the platform layer.

## System Configurators

Platform-specific configurators registered by `ProviderRegistry` based on the current OS.

### ShellConfigurator

- **Unix:** `chsh -s <shell>` (existing)
- **Windows:** Manages Windows Terminal's `settings.json` `defaultProfile` field
  - **Read:** Parse `%LOCALAPPDATA%\Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json`, resolve `defaultProfile` GUID to profile name from `profiles.list`
  - **Drift:** Compare against desired shell name from config
  - **Apply:** Update `defaultProfile` GUID in `settings.json` (atomic write, preserve all other settings)
  - If Windows Terminal is not installed, report that there's nothing to configure (same as systemd on macOS)

### EnvironmentConfigurator

- **Unix:** Linux writes to `/etc/environment` + `/etc/profile.d/cfgd-env.sh`. macOS writes LaunchAgent plist + calls `launchctl setenv`. (Existing)
- **Windows:**
  - **Read current:** Registry `HKCU\Environment` via `RegGetValueW` / `RegEnumValueW`
  - **Apply:** `setx NAME "value"` (persists to registry, available to new processes)
  - **Current session:** `std::env::set_var` so the running process sees changes immediately
  - **Diff:** Same desired-vs-actual map comparison as Linux/macOS

### WindowsRegistryConfigurator (new, Windows-only)

Equivalent of `MacosDefaultsConfigurator`. Manages Windows Registry keys for system and application preferences.

Config format (registry key paths are always single-quoted — handles spaces and keeps backslashes literal):
```yaml
system:
  windowsRegistry:
    'HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced':
      HideFileExt: 0
      Hidden: 1
    'HKCU\Control Panel\Desktop':
      WallPaper: 'C:\Users\user\wallpaper.png'
```

- **Read:** `RegGetValueW` from `windows-sys`
- **Diff:** Compare desired vs actual, produce `SystemDrift` entries
- **Apply:** `RegSetValueExW` with appropriate type:
  - YAML strings -> REG_SZ
  - YAML integers -> REG_DWORD
  - Explicit type annotation available for REG_EXPAND_SZ, REG_MULTI_SZ
- Config parsing: configurator internally deserializes the `serde_yaml::Value` it receives via the `system` HashMap (same pattern as all other configurators — no typed config struct)

### Platform registration

| Configurator | Linux | macOS | Windows |
|---|---|---|---|
| ShellConfigurator | Yes (chsh) | Yes (chsh) | Yes (Windows Terminal) |
| SystemdUnitConfigurator | Yes | No | No |
| LaunchAgentConfigurator | No | Yes | No |
| MacosDefaultsConfigurator | No | Yes | No |
| EnvironmentConfigurator | Yes | Yes | Yes |
| WindowsRegistryConfigurator | No | No | Yes |
| WindowsServiceConfigurator | No | No | Yes |
| Node configurators (sysctl, kubelet, etc.) | Yes | No | No |

### WindowsServiceConfigurator (new, Windows-only)

Equivalent of `SystemdUnitConfigurator` / `LaunchAgentConfigurator`. Manages Windows Services declared in config.

Config format:
```yaml
system:
  windowsServices:
    - name: MyService
      displayName: My Custom Service
      binaryPath: 'C:\Program Files\MyApp\service.exe'
      startType: auto    # auto | manual | disabled
      state: running     # running | stopped
```

- **Read:** Query SCM for service state via `QueryServiceStatusEx` / `QueryServiceConfigW`
- **Apply:** `CreateServiceW` / `ChangeServiceConfigW` for install/config, `StartServiceW` / `ControlService(SERVICE_CONTROL_STOP)` for state, `ChangeServiceConfigW` for start type
- **Drift:** Compare desired state (running/stopped, auto/manual/disabled) against actual

## Reconciler Env File Generation

### PowerShell env file

New `generate_powershell_env_content()` function alongside existing bash/fish generators.

Generates `~/.cfgd-env.ps1`:
```powershell
# managed by cfgd - do not edit
$env:EDITOR = "code"
$env:PATH = "C:\Users\user\.cargo\bin;$env:PATH"
```

**Aliases:** PowerShell aliases differ from shell aliases.
- Simple aliases (no args): `Set-Alias name command`
- Aliases with arguments: `function name { command $args }`
- The generator inspects each alias and picks the right form

### Shell detection on Windows

`plan_env()` on Windows:
1. PowerShell is always present — generate `~/.cfgd-env.ps1`
2. If `sh.exe` on PATH (Git Bash installed) — also generate `~/.cfgd.env` (bash format)
3. If `fish.exe` on PATH — also generate fish env file

### Profile injection

Append `. ~/.cfgd-env.ps1` to the user's `$PROFILE` path:
- PowerShell 7: `~\Documents\PowerShell\Microsoft.PowerShell_profile.ps1`
- PowerShell 5.1: `~\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1`
- Idempotent: check for `cfgd-env` in profile before appending

### verify_env()

Windows path: checks PowerShell env file content matches expectations and that the profile sources it. Same structure as the Unix verification.

## Package Managers

Three new `PackageManager` trait implementations in `crates/cfgd/src/packages/`.

### winget (`winget.rs`)

- `name()` -> `"winget"`
- `available()` -> `command_available("winget")`
- `installed_packages()` -> parse `winget list --source winget` output (tabular column-aligned format)
- `install(packages)` -> `winget install --id <pkg> --accept-package-agreements --accept-source-agreements`
- `uninstall(packages)` -> `winget uninstall --id <pkg>`
- Ships with Windows 11 / App Installer on 10. No bootstrap — error if missing.

### chocolatey (`chocolatey.rs`)

- `name()` -> `"chocolatey"`
- `available()` -> `command_available("choco")`
- `installed_packages()` -> parse `choco list` output (`name version` per line)
- `install(packages)` -> `choco install <pkg> -y`
- `uninstall(packages)` -> `choco uninstall <pkg> -y`
- Bootstrap: `PackageAction::Bootstrap` with PowerShell one-liner from chocolatey.org

### scoop (`scoop.rs`)

- `name()` -> `"scoop"`
- `available()` -> `command_available("scoop")`
- `installed_packages()` -> parse `scoop list` output or read `~/scoop/apps/` directory
- `install(packages)` -> `scoop install <pkg>`
- `uninstall(packages)` -> `scoop uninstall <pkg>`
- Bootstrap: `PackageAction::Bootstrap` with `irm get.scoop.sh | iex`

### Config and registry

- `PackagesSpec` gets `winget: Vec<String>`, `chocolatey: Vec<String>`, `scoop: Vec<String>` fields
- `ProviderRegistry` registers these on Windows
- `desired_packages_for()` handles the new managers
- Module-level package aliases support the new manager names (`aliases.winget`, `aliases.chocolatey`, `aliases.scoop`)
- JSON schema updated for config validation

## Windows Daemon (Windows Service)

**Crate:** `windows-service` under `[target.'cfg(windows)'.dependencies]`.

### Service lifecycle (same CLI surface as Unix)

- `cfgd daemon install` — registers Windows Service (`cfgd`, automatic start)
- `cfgd daemon uninstall` — removes the service
- `cfgd daemon start` / `cfgd daemon stop` — controls via SCM
- `cfgd daemon status` — queries SCM + named pipe for detailed status

### Service entry point

When launched by SCM, detects service context via `windows_service::service_dispatcher`. Maps SCM control codes:
- `SERVICE_CONTROL_STOP` -> graceful shutdown (same codepath as SIGTERM on Unix)
- `SERVICE_CONTROL_INTERROGATE` -> report status

The reconciliation loop, file watchers, sync tasks, IPC server are all identical — shared code, platform-specific lifecycle wrapper only.

### Logging

Windows Event Log via `tracing` subscriber configured at startup:
- **Unix:** journald/syslog (existing)
- **Windows:** Event Log via a minimal custom `tracing` subscriber wrapping `windows-sys` `ReportEventW` (avoids pulling in unmaintained third-party crates for a thin wrapper)

The rest of the codebase uses `tracing::info!` etc. unchanged.

## Self-Upgrade and Release

### Release artifacts

`release.yml` gets two new targets:
- `x86_64-pc-windows-msvc` on `windows-latest`
- `aarch64-pc-windows-msvc` on `windows-latest` (cross-compile)
- Artifact naming: `cfgd-{version}-{target}.zip` (`.zip` not `.tar.gz`)

### Binary replacement on Windows

On Unix, you can unlink and replace a running executable — the existing `atomic_replace` (via `tempfile::NamedTempFile::persist()`) handles this. On Windows, the OS holds a lock on the running binary, so `persist()` fails. The `#[cfg(windows)]` path uses the standard rename-dance instead:
1. Write new binary to `cfgd.exe.new`
2. Rename current to `cfgd.exe.old`
3. Rename new to `cfgd.exe`
4. Delete `.old` on next run

### Asset matching

`upgrade.rs` asset-matching code selects `.zip` on Windows, `.tar.gz` elsewhere. Extraction uses `zip` crate on Windows, `flate2`/`tar` on Unix.

## CI

### New Windows job in `ci.yml`

```yaml
windows:
  runs-on: windows-latest
  steps:
    - cargo fmt --check -p cfgd-core -p cfgd
    - cargo clippy -p cfgd-core -p cfgd -- -D warnings
    - cargo test -p cfgd-core -p cfgd
```

Installs protoc if needed by transitive dependencies. Uses explicit `-p` flags — `cfgd-operator` and `cfgd-csi` are excluded.

Linux CI jobs continue to use `--workspace`.

## Documentation

- `docs/installation.md` — Windows installation instructions (download, winget/scoop/chocolatey install methods)
- `docs/packages.md` — winget/chocolatey/scoop documentation
- `docs/configuration.md` — `windowsRegistry` schema, PowerShell env, Windows Terminal shell config
- `docs/daemon.md` — Windows Service setup
- `docs/cli-reference.md` — any Windows-specific CLI behavior

## Config Schema Additions

```yaml
spec:
  packages:
    winget:
      - Microsoft.VisualStudioCode
    chocolatey:
      - nodejs
    scoop:
      - ripgrep
  system:
    windowsRegistry:
      'HKCU\path\to\key':
        ValueName: value
```

`PackagesSpec` gets new fields for Windows managers. The `system` field in config is a `HashMap<String, serde_yaml::Value>` — each `SystemConfigurator` receives its config as a `serde_yaml::Value` keyed by the configurator's `name()`. The `WindowsRegistryConfigurator` and `WindowsServiceConfigurator` follow this existing pattern (no new typed struct on the config side). JSON schema generation updated to document the new keys.
