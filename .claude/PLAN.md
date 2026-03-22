# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

## Implementation order

| # | Section | Rationale |
|---|---------|-----------|
| 1 | Windows support | Large standalone effort, no dependencies on above |
| 2 | Linux desktop configurators | Small standalone effort, fills parity gap discovered during Windows design |
| 3 | Upstream Kubernetes work | Deferred until after adoption (explicit trigger criteria) |

---

## Windows support

Full design in [specs/2026-03-22-windows-support-design.md](specs/2026-03-22-windows-support-design.md). Implementation plan TBD (will be generated from the spec).

- [ ] Platform abstraction layer (cfgd-core/src/lib.rs): create_symlink, file_permissions_mode, set_file_permissions, is_executable, is_same_inode, acquire_apply_lock, terminate_process, is_root, expand_tilde USERPROFILE support
- [ ] Daemon IPC: named pipe abstraction (DaemonIpc) alongside Unix domain socket
- [ ] Script execution: Windows cmd.exe /C inline commands, extension-based executable check, TerminateProcess timeout
- [ ] System configurators: ShellConfigurator (Windows Terminal), EnvironmentConfigurator (registry/setx), WindowsRegistryConfigurator, WindowsServiceConfigurator
- [ ] Reconciler env: PowerShell env file generation, profile injection, Git Bash detection
- [ ] Package managers: winget, chocolatey, scoop (PackageManager trait implementations)
- [ ] Windows daemon: Windows Service via windows-service crate, Event Log tracing subscriber
- [ ] Self-upgrade: .zip extraction, rename-dance binary replacement, Windows release targets
- [ ] CI: Windows job in ci.yml (fmt, clippy, test for cfgd-core + cfgd)
- [ ] Release: x86_64-pc-windows-msvc and aarch64-pc-windows-msvc targets, .zip artifacts
- [ ] Documentation: Windows installation, packages, configuration, daemon, CLI reference

---

## Linux desktop configurators

Gap discovered during Windows design: macOS has `MacosDefaultsConfigurator` (plist preferences), Windows will have `WindowsRegistryConfigurator`. Linux has no equivalent for desktop environment preferences.

- [ ] Evaluate scope: `gsettings`/`dconf` covers GNOME, but KDE (kwriteconfig5), XFCE (xfconf-query), and others have their own systems. Determine which DEs to support and whether a single configurator or per-DE implementations are appropriate.
- [ ] Implement configurator(s) based on evaluation findings

---

## Upstream Kubernetes work

Deferred until after adoption. CRD versioning (v1alpha1→v1beta1 conversion webhook, dual-version serving, migration runbook) and 3 upstream KEPs (native moduleRef pod spec field, cfgdModule volume type, kubectl debug --module). Full plan in [plans/upstream-kubernetes.md](plans/upstream-kubernetes.md).

- [ ] CRD versioning and upstream KEPs (see plan for details and trigger criteria)
