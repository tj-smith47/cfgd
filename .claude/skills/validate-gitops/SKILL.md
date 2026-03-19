---
name: validate-gitops
description: Validate plans, config schemas, and implementations against KRM and GitOps principles
allowed-tools: ["Read", "Glob", "Grep"]
user-invocable: true
argument-hint: "[plan|schema|code|all]"
---

## KRM / GitOps Principle Validation

Audit the cfgd project for adherence to Kubernetes Resource Model and GitOps principles. Target: $ARGUMENTS (default: all).

### Core Principles to Validate

**KRM Principles:**
1. **Declarative over imperative** — resources describe desired state, not steps to reach it
2. **Structured resources** — every config object has `apiVersion`, `kind`, `metadata`, `spec` (and optionally `status`)
3. **Self-contained** — a resource contains everything needed to reconcile it; no hidden dependencies
4. **Composable** — resources can be combined, layered, and referenced without tight coupling
5. **Observable** — resources have a `status` that reflects actual state vs desired state

**GitOps Principles:**
1. **Declarative** — the entire desired state is described declaratively
2. **Versioned and immutable** — desired state is stored in git with full history
3. **Pulled automatically** — agents pull desired state from the source, never pushed imperatively
4. **Continuously reconciled** — agents detect and correct drift automatically

### What to Check

**In plan files** (`.claude/PLAN.md`, `.claude/kubernetes-first-class.md`):
- [ ] No plan step says "run this script to configure X" when it should be declarative state
- [ ] Reconciliation is described as diff-based (actual vs desired), not sequential execution
- [ ] Drift detection is continuous, not one-shot
- [ ] State is pulled from source (git/server), never pushed to machines
- [ ] All config resources follow KRM structure (apiVersion, kind, metadata, spec)
- [ ] No imperative verbs in resource definitions ("install X" is an action, not a state — the state is "X is present")
- [ ] Resources have a status concept (even if status is computed, not stored in the resource)
- [ ] Profile inheritance is composable — no hardcoded assumptions about what layers exist

**In config schemas** (YAML examples in `examples/`, `docs/`, and `.claude/` design docs):
- [ ] Every resource type has apiVersion, kind, metadata, spec
- [ ] `spec` describes desired state, not actions
- [ ] No fields that describe "how" instead of "what" (e.g., `install-method` is wrong; `source: brew` is right)
- [ ] System config is provider-agnostic — `system:` is a map, not hardcoded fields
- [ ] Packages declared as "desired present" not "to install"
- [ ] Files declared as "desired at target" not "to copy"

**In source code** (`src/**/*.rs`):
- [ ] Reconciler computes a plan by diffing actual vs desired — not by sequencing actions
- [ ] No imperative "do X then Y" without a declarative state backing it
- [ ] State store records observed state, not just actions taken
- [ ] Drift detection compares current system state against declared desired state
- [ ] Provider implementations are stateless — they read system state, don't cache it

**Scope audit — not "dotfiles" but "machine config":**
- [ ] Documentation and comments don't use "dotfiles" as the primary description
- [ ] File management isn't limited to `$HOME` — `files.target` can be any path
- [ ] Package management isn't limited to developer tools — any package manager works
- [ ] System config isn't limited to macOS/desktop — the trait model supports any configurator
- [ ] The tool description says "machine configuration state management", not "dotfile manager"
- [ ] No assumptions that the user is a developer (vs. an operator, SRE, etc.)

### How to Report

For each violation found, report:
1. **File and location** — where the violation is
2. **Principle violated** — which KRM/GitOps principle
3. **What it says** — the current text/code
4. **What it should say** — the corrected version
5. **Severity** — critical (breaks the model) vs. minor (inconsistent language)

### After validation:
Provide a summary score: how many checks passed vs failed, grouped by category (plan/schema/code/scope).
