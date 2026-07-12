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
  full write set, image-push jobs to `packages: write` only. The preflight
  job also carries `id-token: write` — not to publish, but because the
  runtime only injects `ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN` into jobs that
  can mint OIDC tokens, and anodizer's secret preflight validates those on
  behalf of the MCP-registry publisher.
- Deferred-branch release topology (anodizer >= v0.16.0, uniform-local
  `tag`): the tag step runs `tag --changelog --push-tags-only` (tags only —
  the bump commit is reachable ONLY via the tags until publish completes),
  and the `advance-master` job fast-forwards master post-publish
  (`gh api PATCH`, `force=false`, GH_PAT). Its `if:` is the drift-proof
  collapsed form `!cancelled() && needs.tag.result == 'success' &&
  !contains(needs.*.result, 'failure') && !contains(needs.*.result,
  'cancelled')` — semantically "tag succeeded AND no needed job failed or
  was cancelled; skips allowed", with the `needs.*` sweeps automatically
  gating any job later added to the needs list. Never weaken it to a
  per-leg `!= 'failure'` enumeration, and keep EVERY publish leg in the
  job's `needs:` — a leg absent from needs is invisible to the gate. A
  failed release must advance neither master nor a release. The tag job
  also carries a pre-tag stranded-bump guard (highest `v[0-9]*` tag must
  be an ancestor of the release ref, else fail with the
  `git push origin <tag-sha>:refs/heads/master` reconcile command) —
  keep it before the anodizer tag step.
- Preflight's bump-message guard breaks the advance-master→CI→Release
  self-retrigger loop (GH_PAT pushes DO retrigger CI — deliberately, for
  master-badge health); don't loosen it.
- Nightly is sharded per-OS via anodizer split/merge (`partial.by: os` in
  `.anodizer.yaml`): three `build` shards (ubuntu/macos/windows, same runner
  labels as determinism-shards, `auto-install: 'true'`, fail-fast off) each
  run `release --nightly --split --no-preflight` and upload
  `nightly-dist-<shard>` with `include-hidden-files: true`; the ubuntu
  `publish` leg downloads all shards (`merge-multiple: true`) and runs
  `release --nightly --merge --no-preflight`. Publish/sign secrets
  (gpg/apk keys, CLOUDSMITH/SMTP/SNAPCRAFT/GPG_FINGERPRINT) live ONLY on the
  merge leg; split legs get GH_PAT alone. Never collapse nightly back to a
  single ubuntu job — darwin targets cannot zig-link without a macOS SDK.
- The `test-freebsd` job in ci.yml is a `vmactions/freebsd-vm` guest (no
  GitHub-hosted FreeBSD runner exists), pinned to the same release as the
  acceptance VM. It runs `task test:ci` like every other test leg — the
  FreeBSD scope decision lives in the Taskfile, not the workflow: `test:ci`
  detects FreeBSD via `uname -s` (no `RUNNER_OS` inside the guest) and scopes
  to `-p cfgd-core -p cfgd`, because cfgd-csi/cfgd-operator are k8s
  server-side with no FreeBSD surface (same rationale as the Windows branch).
  The toolchain is `rustup-init` not pkg `rust` (guarantees `>= MSRV`, mirrors
  the VM); `task`/`nextest` come from pkg; no protoc (neither in-scope crate
  compiles protos). `task test:freebsd` runs the same leg locally against the
  accept VM (start-if-stopped, poll, sync, `task test:ci`).
- Self-hosted runner labels for actionlint live in `.github/actionlint.yaml`.
- Any job that `uses: ./.github/actions/...` MUST have a checkout step
  before it (the local action file only exists on the runner after
  checkout), and the checkout must precede any `download-artifact` step
  (checkout's git-clean deletes files already in the workspace).
- Reruns of a failed run execute the workflow file FROZEN at its original
  dispatch — a workflow fix never reaches an existing run. If a push-leg
  defect strands built artifacts, recover with a temporary dispatch
  workflow that downloads the run's artifacts and republishes them (the
  v0.5.0 recipe, `backfill-xpkg.yml`, lives in git history — added and
  removed around the v0.5.0 crossplane backfill), then delete it: a
  standing copy of push steps drifts from release.yml.
