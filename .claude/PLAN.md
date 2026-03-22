# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

## Implementation order

| # | Section | Rationale |
|---|---------|-----------|
| 1 | CLI UX improvements | Stabilize CLI surface before updating ecosystem docs |
| 2 | Ecosystem integration | Align policies/docs after CLI + source changes settle |
| 3 | Windows support | Large standalone effort, no dependencies on above |
| 4 | Upstream Kubernetes work | Deferred until after adoption (explicit trigger criteria) |

---

## Ecosystem integration

- [ ] Update `policies/` for new CRD fields: ClusterConfigPolicy CRD, Module CRD `spec.signature.cosign.publicKey`, `spec.security.trustedRegistries`, MachineConfig conditions split (Reconciled, DriftDetected, ModulesResolved, Compliant), `observedGeneration` on Condition struct, DriftAlert conditions (Acknowledged, Resolved, Escalated)
- [ ] Update idiomatic naming in ecosystem files after naming audit: `moduleRef`/`configRef` style cross-references, TitleCase enums, camelCase CRD field names

---

## CLI UX improvements

11 items covering CLI consistency, new commands, and script lifecycle overhaul. Full plan with design detail and execution order in [plans/cli-ux-improvements.md](plans/cli-ux-improvements.md).

- [x] Implement CLI UX improvements (daemon subcommands, profile show name, --yes on source remove, OutputFormatArg, source create positional, ls aliases, diff --module, profile update default active, plan command, structured output, script lifecycle overhaul)
- [x] Update user-facing docs for CLI UX changes (script lifecycle hooks, plan command, output formats, --skip-scripts, --on-drift)

---

## Windows support

Full design in [windows-support.md](windows-support.md). 26 unguarded `std::os::unix` uses across 13 files. No Kubernetes dependency — pick up when targeting Windows users.

- [ ] Phase 1 — compilation gates: `#[cfg(unix)]` on all 26 unix-specific sites. Add `cargo build --target x86_64-pc-windows-msvc` CI job
- [ ] Phase 2 — file management: symlink fallback to copy, skip Unix permission bits on NTFS
- [ ] Phase 3 — package managers: winget, chocolatey, scoop
- [ ] Phase 4 — PowerShell env integration
- [ ] Phase 5 — Windows Service daemon
- [ ] Phase 6 — CI and release: cross-compile job, `.zip` release artifact, Windows docs

---

## Upstream Kubernetes work

Deferred until after adoption. CRD versioning (v1alpha1→v1beta1 conversion webhook, dual-version serving, migration runbook) and 3 upstream KEPs (native moduleRef pod spec field, cfgdModule volume type, kubectl debug --module). Full plan in [plans/upstream-kubernetes.md](plans/upstream-kubernetes.md).

- [ ] CRD versioning and upstream KEPs (see plan for details and trigger criteria)
