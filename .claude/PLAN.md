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

Full design in [specs/2026-03-22-windows-support-design.md](specs/2026-03-22-windows-support-design.md). Implementation plan at [plans/2026-03-22-windows-support-plan-2-features.md](plans/2026-03-22-windows-support-plan-2-features.md). Prompt at [prompts/windows-support.md](prompts/windows-support.md).

**Plan 1 (Platform Foundations) — COMPLETE.** Moved to COMPLETED.md.

**Plan 2 (Windows Features) — remaining work:**

- [ ] Config schema: winget, chocolatey, scoop fields in PackagesSpec + desired_packages_for
- [ ] Package managers: winget, chocolatey, scoop (PackageManager trait implementations)
- [ ] Module-level package aliases for Windows managers
- [ ] Reconciler env: PowerShell env file generation, profile injection, Git Bash detection
- [ ] System configurators: ShellConfigurator (Windows Terminal), EnvironmentConfigurator (registry/setx), WindowsRegistryConfigurator, WindowsServiceConfigurator
- [ ] Windows daemon: Windows Service via windows-service crate, Event Log tracing subscriber
- [ ] Documentation: Windows installation, packages, configuration, daemon, CLI reference
- [ ] JSON schema and explain command updates

---

## Linux desktop configurators

Gap discovered during Windows design: macOS has `MacosDefaultsConfigurator` (plist preferences), Windows will have `WindowsRegistryConfigurator`. Linux has no equivalent for desktop environment preferences.

- [ ] Evaluate scope: `gsettings`/`dconf` covers GNOME, but KDE (kwriteconfig5), XFCE (xfconf-query), and others have their own systems. Determine which DEs to support and whether a single configurator or per-DE implementations are appropriate.
- [ ] Implement configurator(s) based on evaluation findings

---

## Upstream Kubernetes work

Deferred until after adoption. CRD versioning (v1alpha1→v1beta1 conversion webhook, dual-version serving, migration runbook) and 3 upstream KEPs (native moduleRef pod spec field, cfgdModule volume type, kubectl debug --module). Full plan in [plans/upstream-kubernetes.md](plans/upstream-kubernetes.md).

- [ ] CRD versioning and upstream KEPs (see plan for details and trigger criteria)
