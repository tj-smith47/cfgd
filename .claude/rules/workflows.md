---
paths:
  - ".github/**"
---

# cfgd GitHub Actions conventions — `.github/**`

The universal step-shape rule (every step leads with `name:`, blank line
between steps, naming style) lives in the user-level
`~/.claude/rules/github-actions.md` and is enforced by the
`post-edit-workflow.sh` hook. This file covers only cfgd-specific
single-source-of-truth wiring.

## SSOT map (keep intact)

| Pin | Single source | Consumers |
|---|---|---|
| anodizer version | repo variable `ANODIZER_VERSION` (`gh variable set`) | release, ci, nightly, determinism-shards (all via `env: ${{ vars.ANODIZER_VERSION }}`) |
| protoc version | `.github/actions/setup-protoc` input default | setup-rust composite, release/nightly/determinism-shards (call bare, no `version:`) |
| crossplane version + sha256 | `.github/actions/setup-crossplane` input defaults | release (function/push jobs), e2e-setup; `tests/e2e/setup-cluster.sh` fallback mirrors it for local runs |
| cosign version | `COSIGN_VERSION` env in e2e.yml | both cosign-installer steps |
| MSRV | `rust-version` in root Cargo.toml | ci.yml msrv job reads it with sed |

## Job wiring invariants

- Every publish leg is a `publish-crate.yml` reusable-workflow call
  (crd/core as ordered single calls with `rollback: true`, the trio as a
  matrix with rollback left false). Ordering crd → core → trio is
  load-bearing: crates.io dep polls depend on it.
- Determinism lanes come from the tag job's `det_matrix` output: trio
  crates shard across all three OSes, library crates linux-only (via
  determinism-shards' `os-labels` input). Publish legs restore their
  crate's `dist-<crate>-*` artifact — there is no inline preserve-dist.
- Trio rollback is the dedicated `rollback-trio` job, never a per-leg
  step (fail-fast off means legs run concurrently).
- `permissions:` read-only at workflow level; publish jobs elevate to the
  full write set, image-push jobs to `packages: write` only.
- Preflight's bump-message guard breaks the tag→CI→Release self-retrigger
  loop; don't loosen it.
- Self-hosted runner labels for actionlint live in `.github/actionlint.yaml`.
