# cfgd

Declarative, GitOps-style machine configuration state management. Written in Rust.

- **cfgd** — unified machine config CLI + daemon (workstation + k8s node providers). Modes: `cfgd <command>` (CLI) and `cfgd daemon` (reconcile loop).
- **cfgd-operator** — k8s operator + optional device gateway (fleet control plane, web UI, enrollment).
- **cfgd-csi** — CSI Node plugin for module injection into pods.

Unified Helm chart in `chart/cfgd/` ships operator + agent + CSI.

## Phased work
See `.claude/PLAN.md` for the phased plan. Do not add features outside the current phase. If a phase's acceptance criteria aren't met, the phase isn't done.

## Rules (auto-loaded when editing `.rs` files)
- `.claude/rules/hard-rules.md` — 6 non-negotiable rules (output routing, no unwrap in lib, provider traits, error typing, config location, process boundaries)
- `.claude/rules/output-module.md` — Printer is the sole terminal interface
- `.claude/rules/module-boundaries.md` — `std::process::Command` allow-list
- `.claude/rules/shared-utils.md` — catalog of `cfgd-core/src/lib.rs` helpers; check before adding any new helper
- `.claude/rules/database.md` — SQLite conventions (WAL, foreign_keys, versioned migrations)
- `.claude/rules/style.md` — formatting, linting, naming, serde
- `.claude/rules/patterns.md` — builder, trait objects, tracing
- `.claude/rules/testing.md` — `cargo test` gating and test placement
- `.claude/rules/module-map.md` — full crate/module layout
- `.claude/rules/user-layer-notes.md` — how user-level hooks layer on top of project hooks

## Reference docs (load on demand)
- `docs/` — user-facing documentation (`configuration.md` has the YAML schema reference)
- `.claude/kubernetes-first-class.md` — Kubernetes ecosystem engineering spec (CRDs, controllers, webhooks, CSI, OCI, multi-tenancy, Crossplane)

## Config format
Primary YAML (KRM-inspired: `apiVersion`, `kind`, `metadata`, `spec`). TOML also supported. All parsing in `config/`.

## Quality scripts
- `.claude/scripts/audit.sh` — DRY violations, banned patterns, module boundary violations

## Skills
`/implement-phase [N]`, `/audit`, `/add-package-manager [name]`, `/validate-gitops [target]`, `/scope-audit`
